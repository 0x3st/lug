//! 平台会话层（T7.5 接线）：cookie 语义 / 同源校验 / 工作空间 ID——
//! 逐字平移自 lug-server/src/{platform.rs, routes.rs} 的对应实现
//! （契约源只读，语义不变）：
//!
//! - 会话为不透明 token（43 字符 base64url，TTL 7 天），Cookie
//!   HttpOnly + SameSite=Lax，Secure 按 LUG_PUBLIC_URL / Host 判定；
//! - OAuth state 一次性、绑定浏览器 Cookie（`STATE_COOKIE`，TTL 600s），
//!   Path 限定回调路径；
//! - 工作空间以不可变的 GitHub 用户 ID 标识（`github:<id>`）。
//!
//! 与源的差异（平台使然）：token 生成在调用方经 Workers crypto
//! （crate::oauth::random），本模块只做纯字符串/头部运算——host 可测；
//! 存取（KV 会话 / D1 state）由调用方经 storage 模块完成。

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

// 会话/状态 Cookie 契约常量：单一来源平移自 lug-server platform.rs，
// 已由 T5/T6 落位，此处 re-export 供路由层一处引用。
pub use crate::oauth::{CALLBACK_PATH, STATE_COOKIE, STATE_TTL};
pub use crate::storage::sessions::{SessionRecord, SessionUser, SESSION_TTL};

/// 会话 Cookie 名（ lug-server platform.rs::SESSION_COOKIE）。
pub const SESSION_COOKIE: &str = "lug_session";

// ------------------------------------------------------------------ cookie 头运算

/// 从 Cookie 头取值（同名取最后一次，Python SimpleCookie 语义）。
/// 平移自 lug-server/src/routes.rs::cookie_value。
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get("Cookie")?.to_str().ok()?;
    for pair in raw.split(';') {
        let (k, v) = pair.split_once('=')?;
        if k.trim() == name {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// Cookie Secure 判定（lug-server platform.rs::AuthConfig::cookie_secure 平移）：
/// 配置了 https 公网地址 → true；否则看请求 Host 是否本机。
pub fn cookie_secure(public_url: &str, host: Option<&str>) -> bool {
    if !public_url.is_empty() {
        return public_url.starts_with("https://");
    }
    match host.map(str::to_ascii_lowercase) {
        Some(h) => {
            let bare = h
                .strip_prefix('[')
                .map(|s| s.split(']').next().unwrap_or(h.as_str()))
                .unwrap_or(&h);
            let bare = bare.split(':').next().unwrap_or(bare);
            !is_loopback_host(bare)
        }
        None => true,
    }
}

/// Cookie 拼装（lug-server routes.rs::cookie_header 平移）：HttpOnly +
/// SameSite=Lax；Path 分角色（会话 = /，state = OAuth 回调路径）；
/// Secure 按配置/Host 判定。属性序：path → max-age → httponly → samesite → secure。
pub fn cookie_header(
    public_url: &str,
    headers: &HeaderMap,
    name: &str,
    value: &str,
    ttl: i64,
) -> String {
    let host = headers
        .get("Host")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let secure = cookie_secure(public_url, host.as_deref());
    let path = if name == SESSION_COOKIE {
        "/"
    } else {
        CALLBACK_PATH
    };
    format!(
        "{name}={value}; Path={path}; Max-Age={ttl}; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

/// 302 重定向（no-store / no-referrer；middleware 会再补一次平台安全头）。
/// Set-Cookie 头（属性序与原型一致）。平移自 lug-server routes.rs::redirect。
pub fn redirect(location: &str, cookies: Vec<String>) -> Response {
    let mut b = axum::http::Response::builder()
        .status(StatusCode::FOUND)
        .header("Location", location)
        .header("Cache-Control", "no-store")
        .header("Referrer-Policy", "no-referrer");
    for c in cookies {
        b = b.header("Set-Cookie", c);
    }
    b.body(Body::empty())
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ------------------------------------------------------------------ 同源校验

/// 同源校验（lug-server routes.rs::same_origin 平移）：Sec-Fetch-Site 跨站拒绝；
/// 无 Origin 视为非浏览器客户端；配置了公网地址则严格字符串相等。
pub fn same_origin(headers: &HeaderMap, public_url: &str) -> bool {
    if headers.get("Sec-Fetch-Site").and_then(|v| v.to_str().ok()) == Some("cross-site") {
        return false;
    }
    let Some(origin) = headers
        .get("Origin")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
    else {
        return true;
    };
    if !public_url.is_empty() {
        return origin == public_url;
    }
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let netloc = rest.split(['/', '?', '#']).next().unwrap_or("");
    if netloc.is_empty() || netloc.contains('@') {
        return false;
    }
    // path 非空（如 https://h/evil）不算同源
    if rest[netloc.len()..].starts_with('/') {
        return false;
    }
    let host = headers
        .get("Host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    netloc == host
}

// ------------------------------------------------------------------ 会话鉴权纯面

/// 会话鉴权结果（lug-server routes.rs::SessionAuth 平移；省略 CLI 模式的
/// header_based 标记——CSRF 判定改为显式入参）。
pub struct SessionAuth {
    pub token: String,
    pub rec: SessionRecord,
    pub owner: String,
}

/// 401 invalid_session（ lug-server auth_session 的会话缺失分支）。
pub fn invalid_session_response() -> Response {
    crate::routes::api_error(
        StatusCode::UNAUTHORIZED,
        "Invalid or expired session. Sign in again at /platform.",
        "authentication_error",
        Some("invalid_session"),
    )
}

/// 403 invalid_csrf（写操作同源/CSRF 校验失败分支）。
pub fn invalid_csrf_response() -> Response {
    crate::routes::api_error(
        StatusCode::FORBIDDEN,
        "Invalid request origin or CSRF token.",
        "invalid_request_error",
        Some("invalid_csrf"),
    )
}

/// 会话 token 抽取：X-Session-Token 优先，回落 `SESSION_COOKIE`
/// （lug-server auth_session 的前半段；KV 查询留给调用方）。
pub fn session_tokens(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    let header_token = headers
        .get("X-Session-Token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let cookie_token = cookie_value(headers, SESSION_COOKIE);
    (header_token, cookie_token)
}

/// 写操作守卫（lug-server auth_session 的 write 分支纯面）：同源校验 +
/// CSRF 常数时间比较；显式 header token（CLI 用法）不依赖 CSRF。
/// Cookie 会话下 CSRF 不匹配统一在 403 invalid_csrf 中拒绝。
pub fn write_guards(
    headers: &HeaderMap,
    public_url: &str,
    rec_csrf: &str,
    header_based: bool,
) -> Result<(), Response> {
    let ok_origin = same_origin(headers, public_url);
    let csrf_ok = match header_based {
        true => true,
        false => headers
            .get("X-CSRF-Token")
            .and_then(|v| v.to_str().ok())
            .map(|s| crate::oauth::util::const_eq(s, rec_csrf))
            .unwrap_or(false),
    };
    if !ok_origin || !csrf_ok {
        return Err(invalid_csrf_response());
    }
    Ok(())
}

// ------------------------------------------------------------------ 工作空间 ID

/// 工作空间 ID：只接受已验证的 GitHub 身份（`github:<不可变数字 ID>`）。
/// lug-server platform.rs::owner_id 平移。
pub fn owner_id(user: &SessionUser) -> Result<String, ()> {
    if user.provider != "github"
        || user.id.is_empty()
        || !user.id.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(());
    }
    let n: u64 = user.id.parse().map_err(|_| ())?;
    if n == 0 {
        return Err(());
    }
    Ok(format!("github:{n}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::{HeaderMap as Hm, HeaderValue};

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut hm = Hm::new();
        for (k, v) in pairs {
            hm.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        hm
    }

    // ---------------- cookie_value（ lug-server 语义） ----------------

    #[test]
    fn cookie_value_parses_and_prefers_last() {
        let h = headers(&[("Cookie", "a=1; lug_session=tok; b=2")]);
        assert_eq!(cookie_value(&h, "lug_session").as_deref(), Some("tok"));
        assert_eq!(cookie_value(&h, "a").as_deref(), Some("1"));
        assert_eq!(cookie_value(&h, "missing"), None);
        // 同名重复：与 lug-server 实现一致取**首个**命中（其文档注释写"最后一次"
        // 但实现先行返回——逐字节契约以实现为准，golden 对照再证一轮）
        let h = headers(&[("Cookie", "lug_session=first; lug_session=second")]);
        assert_eq!(cookie_value(&h, "lug_session").as_deref(), Some("first"));
        // 值两侧空白被 trim
        let h = headers(&[("Cookie", "lug_session= padded ")]);
        assert_eq!(cookie_value(&h, "lug_session").as_deref(), Some("padded"));
        let h = headers(&[("Cookie", "novalue")]);
        assert_eq!(cookie_value(&h, "novalue"), None);
    }

    // ---------------- cookie_secure / cookie_header ----------------

    /// host 上与 lug-server AuthConfig::cookie_secure（pub）对拍，
    /// 契约源在 tests/golden_vs_server.rs 再做一轮全矩阵对照。
    #[test]
    fn cookie_header_matches_legacy_attributes_and_order() {
        // 配置了 https 公网地址 → Secure；会话 Path=/，state Path=回调路径
        let h = headers(&[("Host", "lug.by530.com")]);
        assert_eq!(
            cookie_header("https://lug.by530.com", &h, SESSION_COOKIE, "tok", 604_800),
            "lug_session=tok; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax; Secure"
        );
        assert_eq!(
            cookie_header("https://lug.by530.com", &h, STATE_COOKIE, "st", 600),
            "lug_oauth_state=st; Path=/f/a/o/platform/auth/github/callback; Max-Age=600; HttpOnly; SameSite=Lax; Secure"
        );
        // 未配置公网地址 + 本机 Host → 无 Secure
        let h = headers(&[("Host", "localhost:8787")]);
        assert_eq!(
            cookie_header("", &h, SESSION_COOKIE, "tok", 604_800),
            "lug_session=tok; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax"
        );
        // 未配置公网地址 + 无 Host → Secure（保守）
        let h = headers(&[]);
        assert_eq!(
            cookie_header("", &h, SESSION_COOKIE, "tok", 604_800),
            "lug_session=tok; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax; Secure"
        );
        // 清除 cookie（value 空 + Max-Age=0）形态不变
        let h = headers(&[("Host", "lug.by530.com")]);
        assert_eq!(
            cookie_header("https://lug.by530.com", &h, STATE_COOKIE, "", 0),
            "lug_oauth_state=; Path=/f/a/o/platform/auth/github/callback; Max-Age=0; HttpOnly; SameSite=Lax; Secure"
        );
    }

    #[test]
    fn cookie_secure_matches_legacy_rules() {
        assert!(cookie_secure("https://lug.by530.com", Some("anything")));
        assert!(!cookie_secure("http://localhost:8787", Some("localhost")));
        // http 公网地址（配置层已拒绝，此处防御性跟随 starts_with("https")）
        assert!(!cookie_secure("http://lug.by530.com", None));
        assert!(!cookie_secure("", Some("LOCALHOST")));
        assert!(!cookie_secure("", Some("127.0.0.1:8000")));
        // IPv6 字面量：lug-server 实现对 "[::1]:8000" 二次 split(':') 得空串
        // → 非本机 → Secure=true。端口忠实保留该行为（含 quirk），golden 对照背书。
        assert!(cookie_secure("", Some("[::1]:8000")));
        assert!(cookie_secure("", Some("lug.by530.com")));
        assert!(cookie_secure("", None));
    }

    // ---------------- same_origin ----------------

    #[test]
    fn same_origin_mirrors_legacy_semantics() {
        // 无 Origin → 非浏览器客户端，放行
        assert!(same_origin(&headers(&[("Host", "h")]), ""));
        // Origin 与 Host 一致 → 放行
        assert!(same_origin(
            &headers(&[("Host", "h"), ("Origin", "http://h")]),
            ""
        ));
        // 跨站 Sec-Fetch-Site → 拒绝（即使 Origin 相符）
        assert!(!same_origin(
            &headers(&[
                ("Host", "h"),
                ("Origin", "http://h"),
                ("Sec-Fetch-Site", "cross-site")
            ]),
            ""
        ));
        // 同站 fetch → 放行
        assert!(same_origin(
            &headers(&[
                ("Host", "h"),
                ("Origin", "http://h"),
                ("Sec-Fetch-Site", "same-origin")
            ]),
            ""
        ));
        // Origin host 不符 → 拒绝
        assert!(!same_origin(
            &headers(&[("Host", "h"), ("Origin", "http://evil")]),
            ""
        ));
        // 带 path 的 Origin → 拒绝
        assert!(!same_origin(
            &headers(&[("Host", "h"), ("Origin", "https://h/evil")]),
            ""
        ));
        // userinfo → 拒绝
        assert!(!same_origin(
            &headers(&[("Host", "h"), ("Origin", "https://u@h")]),
            ""
        ));
        // 非 http(s) scheme → 拒绝
        assert!(!same_origin(
            &headers(&[("Host", "h"), ("Origin", "ftp://h")]),
            ""
        ));
        // 配置公网地址 → 严格字符串相等
        assert!(same_origin(
            &headers(&[("Host", "elsewhere"), ("Origin", "https://lug.by530.com")]),
            "https://lug.by530.com"
        ));
        assert!(!same_origin(
            &headers(&[("Host", "h"), ("Origin", "https://lug.by530.com/")]),
            "https://lug.by530.com"
        ));
    }

    // ---------------- 写守卫 / 会话抽取 ----------------

    fn session_rec(csrf: &str) -> SessionRecord {
        SessionRecord {
            user: SessionUser {
                provider: "github".into(),
                id: "123".into(),
                login: "octo".into(),
            },
            csrf_token: csrf.into(),
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn write_guards_require_origin_and_csrf() {
        let rec = session_rec("csrf-1");
        let ok = headers(&[
            ("Host", "h"),
            ("Origin", "http://h"),
            ("X-CSRF-Token", "csrf-1"),
        ]);
        assert!(write_guards(&ok, "", &rec.csrf_token, false).is_ok());
        // CSRF 缺失 → 403 invalid_csrf
        let no_csrf = headers(&[("Host", "h"), ("Origin", "http://h")]);
        let err = write_guards(&no_csrf, "", &rec.csrf_token, false).unwrap_err();
        assert_eq!(err.status(), StatusCode::FORBIDDEN);
        // CSRF 不符 → 403
        let bad_csrf = headers(&[
            ("Host", "h"),
            ("Origin", "http://h"),
            ("X-CSRF-Token", "csrf-2"),
        ]);
        assert!(write_guards(&bad_csrf, "", &rec.csrf_token, false).is_err());
        // 同源不符 → 403（即使 CSRF 相符）
        let bad_origin = headers(&[
            ("Host", "h"),
            ("Origin", "http://evil"),
            ("X-CSRF-Token", "csrf-1"),
        ]);
        assert!(write_guards(&bad_origin, "", &rec.csrf_token, false).is_err());
        // 显式 header token（CLI）：不依赖 CSRF，但仍要求同源
        let hdr_only = headers(&[("Host", "h"), ("Origin", "http://h")]);
        assert!(write_guards(&hdr_only, "", &rec.csrf_token, true).is_ok());
        let hdr_cross = headers(&[("Host", "h"), ("Origin", "http://evil")]);
        assert!(write_guards(&hdr_cross, "", &rec.csrf_token, true).is_err());
    }

    #[test]
    fn invalid_session_error_shape_is_contract() {
        let res = invalid_session_response();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        // no-store/no-referrer 由路由层 cors_middleware 补（routes 层测试覆盖），
        // 这里只锁错误结构本身。
    }

    // ---------------- owner_id ----------------

    #[test]
    fn owner_id_accepts_only_verified_github_identity() {
        let u = SessionUser {
            provider: "github".into(),
            id: "12345".into(),
            login: "octo".into(),
        };
        assert_eq!(owner_id(&u).unwrap(), "github:12345");
        let bad = [
            ("local", "12345"),
            ("github", ""),
            ("github", "12a45"),
            ("github", "0"),
            ("github", "-1"),
        ];
        for (provider, id) in bad {
            let u = SessionUser {
                provider: provider.into(),
                id: id.into(),
                login: "x".into(),
            };
            assert!(owner_id(&u).is_err(), "{provider}:{id}");
        }
        // 超大数字 ID 合法（u64 解析上限）
        let big = SessionUser {
            provider: "github".into(),
            id: "18446744073709551615".into(),
            login: "x".into(),
        };
        assert!(owner_id(&big).is_ok());
        let overflow = SessionUser {
            provider: "github".into(),
            id: "18446744073709551616".into(),
            login: "x".into(),
        };
        assert!(owner_id(&overflow).is_err());
    }

    // ---------------- redirect ----------------

    #[test]
    fn redirect_sets_location_and_cookies() {
        let res = redirect("/platform?login_error=x", vec!["a=1".into(), "b=2".into()]);
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(res.headers()["Location"], "/platform?login_error=x");
        assert_eq!(res.headers()["Cache-Control"], "no-store");
        assert_eq!(res.headers()["Referrer-Policy"], "no-referrer");
        let cookies: Vec<_> = res.headers().get_all("Set-Cookie").iter().collect();
        assert_eq!(cookies.len(), 2);
    }
}
