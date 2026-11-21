//! 路由与处理器：与 Python 原型的 HTTP 契约逐项对齐（CORS / 错误结构 /
//! SSE 事件序列），差异仅为 §5 差异表声明的部分（计费口径、404 文案随
//! 实际路由表更新）。

use crate::audit::AuditLog;
use crate::config::Config;
use crate::keystore::KeyStore;
use crate::platform::{
    AuthConfig, GithubFlow, OAuthError, SessionRecord, SessionStore, SESSION_COOKIE, SESSION_TTL,
    STATE_COOKIE, STATE_TTL,
};
use axum::body::Body;
use axum::extract::{Path as AxumPath, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use lug_engine::Value;
use serde_json::json;
use std::convert::Infallible;
use std::sync::Arc;

/// 共享应用状态。
pub struct AppState {
    pub config: Config,
    pub keys: KeyStore,
    pub audit: AuditLog,
    /// GitHub 登录配置（环境变量）。
    pub auth: AuthConfig,
    /// OAuth 流程（真实 GitHub / 测试替身）。
    pub flow: Arc<dyn GithubFlow>,
    /// 内存会话存储（TTL 7 天）。
    pub sessions: SessionStore,
}

pub type SharedState = Arc<AppState>;

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/", get(serve_html_index))
        .route("/index.html", get(serve_html_index))
        .route("/docs", get(serve_html_docs))
        .route("/platform", get(serve_html_platform))
        .route("/assets/{name}", get(serve_asset))
        .route("/f/a/o/operator", get(operator_table))
        .route("/f/a/o/calculate", post(calculate))
        // ---- 平台控制台 API（GitHub 登录 / 会话 / Key / 用量）----
        .route("/f/a/o/platform/auth", get(platform_auth_status))
        .route("/f/a/o/platform/me", get(platform_me))
        .route("/f/a/o/platform/auth/github", get(github_login))
        .route("/f/a/o/platform/auth/github/callback", get(github_callback))
        .route("/f/a/o/platform/logout", post(platform_logout))
        .route(
            "/f/a/o/platform/keys",
            get(platform_keys_list).post(platform_keys_create),
        )
        .route("/f/a/o/platform/keys/{id}", delete(platform_key_revoke))
        .route("/f/a/o/platform/usage", get(platform_usage))
        .fallback(fallback)
        // 已注册路径的未支持方法（如 OPTIONS/DELETE）也走统一兜底
        .method_not_allowed_fallback(fallback)
        .layer(axum::middleware::from_fn(cors_middleware))
        .with_state(state)
}

// ------------------------------------------------------------------ CORS 中间件

/// CORS/安全头中间件（镜像原型 `_cors`）。注意：axum 0.8 的
/// `method_not_allowed_fallback` 不经过 `Router::layer`，因此 fallback
/// 响应需自行调用 [`apply_cors`]。
pub async fn cors_middleware(req: Request<Body>, next: axum::middleware::Next) -> Response {
    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;
    apply_cors(&path, res.headers_mut());
    res
}

/// CORS/安全头公共实现：平台路由收紧（no-store / no-referrer），
/// 计算 API 公开 CORS。
pub fn apply_cors(path: &str, headers: &mut axum::http::HeaderMap) {
    if path.starts_with("/f/a/o/platform/") {
        headers.insert("Cache-Control", HeaderValue::from_static("no-store"));
        headers.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    } else {
        headers.insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
        headers.insert(
            "Access-Control-Allow-Headers",
            HeaderValue::from_static("Authorization, Content-Type"),
        );
        headers.insert(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
    }
}

// ------------------------------------------------------------------ 错误结构

/// OpenAI 风格错误结构（`{"error": {message, type, param, code}}`）。
pub fn api_error(
    status: StatusCode,
    message: &str,
    err_type: &str,
    code: Option<&str>,
) -> Response {
    let body = json!({
        "error": {"message": message, "type": err_type, "param": null, "code": code}
    });
    (status, Json(body)).into_response()
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

// ------------------------------------------------------------------ 引擎数值 → JSON

/// 引擎数值 → JSON 数字。Big/Float 经**十进制字面量**构造（arbitrary_precision），
/// 输出与 Python `json.dumps` 逐字节一致（含 2.0、0.0→0 的双格式化语义）。
pub fn num_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Int(i) => json!(i),
        Value::Big(b) => serde_json::from_str(&b.to_string()).unwrap_or(serde_json::Value::Null),
        Value::Float(f) => {
            // 先 format_num（整数化），再用 py repr 字面量（与 Python json 一致）
            let disp = lug_engine::value::format_num(*f).to_display();
            serde_json::from_str(&disp).unwrap_or(serde_json::Value::Null)
        }
    }
}

// ------------------------------------------------------------------ 鉴权

/// Bearer API Key 鉴权（消息与原型逐字节一致）。
fn require_api_key(state: &AppState, headers: &HeaderMap) -> Option<crate::keystore::KeyRec> {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let raw = auth.strip_prefix("Bearer ").map(str::trim).unwrap_or("");
    if let Some(rec) = state.keys.validate(raw) {
        return Some(rec);
    }
    None
}

fn unauthorized() -> Response {
    api_error(
        StatusCode::UNAUTHORIZED,
        "Missing or invalid API key. Create one in the platform console (GET /platform) and pass it in the 'Authorization: Bearer sk-xxx' header.",
        "authentication_error",
        Some("invalid_api_key"),
    )
}

// ------------------------------------------------------------------ 平台会话工具

/// 从 Cookie 头取值（同名取最后一次，Python SimpleCookie 语义）。
/// Set-Cookie 头（属性序与原型一致：path → max-age → httponly → samesite → secure）。
/// 302 重定向（no-store / no-referrer；middleware 会再补一次平台安全头）。
fn redirect(location: &str, cookies: Vec<String>) -> Response {
    let mut b = Response::builder()
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

/// 极简 Cookie 头解析（`a=1; b=2` → 指定 name 的值）。
fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get("Cookie")?.to_str().ok()?;
    for pair in raw.split(';') {
        let (k, v) = pair.split_once('=')?;
        if k.trim() == name {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Cookie 拼装（镜像原型 `_cookie`）：HttpOnly + SameSite=Lax；Path 分角色
/// （会话 = /，state = OAuth 回调路径）；Secure 按配置/Host 判定。
fn cookie_header(
    auth: &crate::platform::AuthConfig,
    headers: &HeaderMap,
    name: &str,
    value: &str,
    ttl: i64,
) -> String {
    let host = headers
        .get("Host")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let secure = auth.cookie_secure(host.as_deref());
    let path = if name == crate::platform::SESSION_COOKIE {
        "/"
    } else {
        crate::platform::CALLBACK_PATH
    };
    format!(
        "{name}={value}; Path={path}; Max-Age={ttl}; HttpOnly; SameSite=Lax{}",
        if secure { "; Secure" } else { "" }
    )
}

/// 同源校验（镜像原型 `_same_origin`）：Sec-Fetch-Site 跨站拒绝；
/// 无 Origin 视为非浏览器客户端；配置了公网地址则严格字符串相等。
fn same_origin(headers: &HeaderMap, public_url: &str) -> bool {
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

/// 会话鉴权结果。
struct SessionAuth {
    token: String,
    #[allow(dead_code)] // CLI 模式标记；当前仅用于调试观察
    header_based: bool,
    rec: SessionRecord,
    owner: String,
}

/// Cookie / X-Session-Token 会话鉴权（镜像原型 `_auth_session`）：
/// 过期/无效 → 401 invalid_session；写操作要求同源 + CSRF。
#[allow(clippy::result_large_err)]
fn auth_session(
    state: &AppState,
    headers: &HeaderMap,
    write: bool,
) -> Result<SessionAuth, Response> {
    let header_token = headers
        .get("X-Session-Token")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let token = header_token
        .clone()
        .or_else(|| cookie_value(headers, SESSION_COOKIE))
        .unwrap_or_default();
    let Some(rec) = state.sessions.get(&token) else {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Invalid or expired session. Sign in again at /platform.",
            "authentication_error",
            Some("invalid_session"),
        ));
    };
    // 注：Cookie 会话下 CSRF 不匹配统一在写操作的 403 invalid_csrf 中拒绝
    //（跨站/篡改的 CSRF 属于请求伪造，不泄露会话轮换信息）
    if write {
        let ok_origin = same_origin(headers, &state.auth.public_url);
        let csrf_ok = match &header_token {
            Some(_) => true, // 显式 header token（CLI 用法）不依赖 CSRF
            None => headers
                .get("X-CSRF-Token")
                .and_then(|v| v.to_str().ok())
                .map(|s| crate::platform::const_eq(s, &rec.csrf_token))
                .unwrap_or(false),
        };
        if !ok_origin || !csrf_ok {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "Invalid request origin or CSRF token.",
                "invalid_request_error",
                Some("invalid_csrf"),
            ));
        }
    }
    let owner = crate::platform::owner_id(&rec.user)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
    Ok(SessionAuth {
        token,
        header_based: header_token.is_some(),
        rec,
        owner,
    })
}

// ------------------------------------------------------------------ 平台路由

/// GET /f/a/o/platform/auth：登录选项（公开）。
async fn platform_auth_status(State(state): State<SharedState>) -> Response {
    // 走 flow 抽象：测试替身可整体替换（含 enabled 语义）
    json_response(
        StatusCode::OK,
        json!({"github_enabled": state.flow.enabled()}),
    )
}

/// GET /f/a/o/platform/me：当前会话（用户 + CSRF）。
async fn platform_me(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let s = match auth_session(&state, &headers, false) {
        Ok(s) => s,
        Err(r) => return r,
    };
    json_response(
        StatusCode::OK,
        json!({"user": s.rec.user.to_json(), "csrf_token": s.rec.csrf_token}),
    )
}

/// GET /f/a/o/platform/auth/github：发起 GitHub 登录（302 → authorize URL + state Cookie）。
async fn github_login(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    match state.flow.begin() {
        Ok((st, url)) => {
            let cookie = cookie_header(&state.auth, &headers, STATE_COOKIE, &st, STATE_TTL as i64);
            redirect(&url, vec![cookie])
        }
        Err(_) => redirect("/platform?login_error=github_not_configured", vec![]),
    }
}

/// GET /f/a/o/platform/auth/github/callback：OAuth 回调。
/// state 校验 → 换取身份 → 换发会话 → 302 /platform。
async fn github_callback(
    State(state): State<SharedState>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    let cleared = cookie_header(&state.auth, &headers, STATE_COOKIE, "", 0);
    let fail = |e: &OAuthError, cookies: Vec<String>| {
        redirect(&format!("/platform?login_error={e}"), {
            let mut c = cookies;
            c.push(cleared.clone());
            c
        })
    };

    if !state.flow.enabled() {
        return fail(&OAuthError::GithubNotConfigured, vec![]);
    }
    // 查询参数解析（parse_qs 语义：忽略空值；重复参数视为非法）
    let pairs = parse_query(raw.as_deref().unwrap_or(""));
    let states: Vec<&str> = pairs
        .iter()
        .filter(|(k, _)| k == "state")
        .map(|(_, v)| v.as_str())
        .collect();
    if states.len() != 1 {
        return fail(&OAuthError::InvalidOauthState, vec![]);
    }
    let browser_state = cookie_value(&headers, STATE_COOKIE).unwrap_or_default();
    let verifier = match state.flow.consume(states[0], &browser_state) {
        Ok(v) => v,
        Err(e) => return fail(&e, vec![]),
    };
    if pairs.iter().any(|(k, _)| k == "error") {
        return fail(&OAuthError::GithubCancelled, vec![]);
    }
    let codes: Vec<&str> = pairs
        .iter()
        .filter(|(k, _)| k == "code")
        .map(|(_, v)| v.as_str())
        .collect();
    if codes.len() != 1 || codes[0].len() > 1024 {
        return fail(&OAuthError::GithubUnavailable, vec![]);
    }
    let user = match state.flow.exchange(codes[0].to_string(), verifier).await {
        Ok(u) => u,
        Err(e) => return fail(&e, vec![]),
    };
    // 换发会话：旧会话立即失效
    if let Some(old) = cookie_value(&headers, SESSION_COOKIE) {
        state.sessions.revoke(&old);
    }
    let token = match state.sessions.create(user) {
        Ok(t) => t,
        Err(_) => {
            return fail(&OAuthError::GithubUnavailable, vec![]);
        }
    };
    let session_cookie = cookie_header(
        &state.auth,
        &headers,
        SESSION_COOKIE,
        &token,
        SESSION_TTL as i64,
    );
    redirect("/platform", vec![cleared, session_cookie])
}

/// POST /f/a/o/platform/logout：撤销会话 + 清 Cookie。
async fn platform_logout(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let s = match auth_session(&state, &headers, true) {
        Ok(s) => s,
        Err(r) => return r,
    };
    state.sessions.revoke(&s.token);
    let cleared = cookie_header(&state.auth, &headers, SESSION_COOKIE, "", 0);
    let mut res = json_response(StatusCode::OK, json!({"ok": true}));
    res.headers_mut()
        .append("Set-Cookie", cleared.parse().unwrap());
    res
}

/// GET /f/a/o/platform/keys：当前用户的 Key 列表（不含哈希/属主）。
async fn platform_keys_list(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let s = match auth_session(&state, &headers, false) {
        Ok(s) => s,
        Err(r) => return r,
    };
    json_response(
        StatusCode::OK,
        json!({"object": "list", "data": state.keys.list_public(&s.owner)}),
    )
}

/// POST /f/a/o/platform/keys：签发新 Key（明文仅本次返回）。
async fn platform_keys_create(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let s = match auth_session(&state, &headers, true) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if body.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Request body is empty.",
            "invalid_request_error",
            None,
        );
    }
    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Invalid JSON in request body.",
                "invalid_request_error",
                None,
            )
        }
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Request body must be a JSON object.",
                "invalid_request_error",
                None,
            )
        }
    };
    let name = obj.get("name").and_then(|v| v.as_str()).map(str::to_string);
    let (raw, rec) = match state.keys.create(name, &s.owner) {
        Ok(v) => v,
        Err(e) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &e,
                "invalid_request_error",
                None,
            )
        }
    };
    json_response(
        StatusCode::OK,
        json!({
            "id": rec.id, "name": rec.name, "key": raw,
            "prefix": rec.prefix, "created_at": rec.created_at,
            "note": "Save this key now; it will not be shown again.",
        }),
    )
}

/// DELETE /f/a/o/platform/keys/{id}：撤销当前用户自己的 Key。
async fn platform_key_revoke(
    State(state): State<SharedState>,
    headers: HeaderMap,
    AxumPath(kid): AxumPath<String>,
) -> Response {
    let s = match auth_session(&state, &headers, true) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.keys.revoke_scoped(&kid, &s.owner) {
        Some(rec) => {
            // Key 生命周期审计（§10.7 审计不能省）
            state.audit.record(&crate::audit::AuditEntry {
                key_id: "-",
                key_prefix: &rec.prefix,
                request_id: "-",
                expression: &format!("key:revoke:{}", rec.id),
                kind: "key_revoked",
                outcome: &rec.status,
                prompt_tokens: 0,
                completion_tokens: 0,
                duration_ms: 0,
            });
            json_response(StatusCode::OK, json!({"id": rec.id, "status": rec.status}))
        }
        None => api_error(
            StatusCode::NOT_FOUND,
            &format!("Unknown key id: {kid}"),
            "invalid_request_error",
            None,
        ),
    }
}

/// GET /f/a/o/platform/usage：当前用户的用量汇总。
async fn platform_usage(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    let s = match auth_session(&state, &headers, false) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let keys = state.keys.list_public(&s.owner);
    let mut total = json!({
        "requests": 0u64, "prompt_tokens": 0u64,
        "completion_tokens": 0u64, "total_tokens": 0u64,
    });
    for k in &keys {
        for field in [
            "requests",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
        ] {
            if let (Some(sum), Some(v)) = (
                total.get_mut(field).and_then(|x| x.as_u64()),
                k.get("usage")
                    .and_then(|u| u.get(field))
                    .and_then(|x| x.as_u64()),
            ) {
                total[field] = json!(sum + v);
            }
        }
    }
    json_response(
        StatusCode::OK,
        json!({"object": "usage_report", "total": total, "keys": keys}),
    )
}

/// parse_qs 等价：按 '&' 分隔、'+' 视作空格、百分号解码、忽略空值。
fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            if v.is_empty() {
                return None; // parse_qs 默认 keep_blank_values=False
            }
            Some((percent_decode(k), percent_decode(v)))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                let hex = |b: u8| (b as char).to_digit(16).unwrap();
                out.push(((hex(bytes[i + 1]) << 4) | hex(bytes[i + 2])) as u8);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------ 静态资源

/// HTML 页面（web/ 下白名单文件，逐请求读取，与原型一致）。
fn serve_html_file(state: &AppState, filename: &str) -> Response {
    let path = state.config.web_dir.join(filename);
    match std::fs::read_to_string(&path) {
        Ok(body) => ([("Content-Type", "text/html; charset=utf-8")], body).into_response(),
        Err(_) => api_error(
            StatusCode::NOT_FOUND,
            &format!("Page not found: web/{filename}"),
            "invalid_request_error",
            None,
        ),
    }
}

async fn serve_html_index(State(state): State<SharedState>) -> Response {
    serve_html_file(&state, "index.html")
}
async fn serve_html_docs(State(state): State<SharedState>) -> Response {
    serve_html_file(&state, "docs.html")
}
async fn serve_html_platform(State(state): State<SharedState>) -> Response {
    serve_html_file(&state, "platform.html")
}

/// 构建产物白名单扩展名（防目录穿越 + 类型固定，与原型 ASSET_TYPES 一致）。
fn asset_content_type(ext: &str) -> Option<&'static str> {
    match ext {
        "js" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "ico" => Some("image/x-icon"),
        "woff2" => Some("font/woff2"),
        "map" => Some("application/json"),
        _ => None,
    }
}

async fn serve_asset(
    State(state): State<SharedState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    let base = state.config.web_dir.join("assets");
    let full = base.join(&name);
    // 防目录穿越：canonicalize 后必须仍在 base 之下
    let ok = std::fs::canonicalize(&full)
        .map(|p| p.starts_with(&base))
        .unwrap_or(false);
    let ext = full
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match (ok, asset_content_type(&ext)) {
        (true, Some(ct)) => match std::fs::read(&full) {
            Ok(data) => (
                [
                    ("Content-Type", ct),
                    ("Cache-Control", "public, max-age=31536000, immutable"),
                ],
                data,
            )
                .into_response(),
            Err(_) => api_error(
                StatusCode::NOT_FOUND,
                &format!("Asset not found: {name}"),
                "invalid_request_error",
                None,
            ),
        },
        _ => api_error(
            StatusCode::NOT_FOUND,
            &format!("Asset not found: {name}"),
            "invalid_request_error",
            None,
        ),
    }
}

// ------------------------------------------------------------------ 计算接口

/// GET /f/a/o/operator：注册表生成的编码表（0–6 冻结区 + 单词别名）。
async fn operator_table(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    if require_api_key(&state, &headers).is_none() {
        return unauthorized();
    }
    let mut data = Vec::new();
    for def in lug_engine::registry::REGISTRY {
        let mut aliases: Vec<&str> = lug_engine::registry::WORD_ALIASES
            .iter()
            .filter(|(_, s)| *s == def.symbol)
            .map(|(a, _)| *a)
            .collect();
        aliases.sort_unstable(); // Python sorted()：UTF-8 字节序 == 码点序
        data.push(json!({
            "code": def.code, "symbol": def.symbol,
            "name": def.name, "aliases": aliases,
        }));
    }
    json_response(
        StatusCode::OK,
        json!({"object": "operator_code_table", "data": data}),
    )
}

/// POST /f/a/o/calculate：唯一计算入口。
/// `{"expression": "...", "process": true?}` → JSON 或 SSE 过程流。
async fn calculate(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let rec = match require_api_key(&state, &headers) {
        Some(r) => r,
        None => return unauthorized(),
    };

    // ---- 请求体解析（错误信息与原型逐字节一致）----
    if body.is_empty() {
        return api_error(
            StatusCode::BAD_REQUEST,
            "Request body is empty.",
            "invalid_request_error",
            None,
        );
    }
    let parsed: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Invalid JSON in request body.",
                "invalid_request_error",
                None,
            )
        }
    };
    let obj = match parsed.as_object() {
        Some(o) => o,
        None => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Request body must be a JSON object.",
                "invalid_request_error",
                None,
            )
        }
    };

    let started = std::time::Instant::now();

    // ---- 表达式抽取（唯一输入形态）----
    let expr = match obj.get("expression").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            return api_error(
                StatusCode::BAD_REQUEST,
                "Input must be an expression string, e.g. {\"expression\": \"3+5*2**(1+1)\"}",
                "invalid_request_error",
                None,
            )
        }
    };
    // 表达式长度上限（§3.7 工作有界，D4 家族护栏）
    if expr.chars().count() > state.config.max_expr_len {
        let msg = format!(
            "Expression too long (max {} characters)",
            state.config.max_expr_len
        );
        // 错误不计费，但审计必须留痕
        state.audit.record(&crate::audit::AuditEntry {
            key_id: &rec.id,
            key_prefix: &rec.prefix,
            request_id: "-",
            expression: &expr,
            kind: "error",
            outcome: &msg,
            prompt_tokens: 0,
            completion_tokens: 0,
            duration_ms: started.elapsed().as_millis(),
        });
        return api_error(
            StatusCode::BAD_REQUEST,
            &msg,
            "invalid_request_error",
            Some("expression_error"),
        );
    }

    // ---- 求值（错误 → OpenAI 风格错误；错误不计费，但审计留痕）----
    let output = match lug_engine::evaluate_with(&expr, &state.config.ctx()) {
        Ok(o) => o,
        Err(e) => {
            state.audit.record(&crate::audit::AuditEntry {
                key_id: &rec.id,
                key_prefix: &rec.prefix,
                request_id: "-",
                expression: &expr,
                kind: "error",
                outcome: &e.message,
                prompt_tokens: 0,
                completion_tokens: 0,
                duration_ms: started.elapsed().as_millis(),
            });
            return api_error(
                StatusCode::BAD_REQUEST,
                &e.message,
                "invalid_request_error",
                Some(e.code.as_str()),
            );
        }
    };
    let request_id = random_hex(16);

    // process 真值判定（Python truthy：true / 非零数 / 非空串）
    let process = match obj.get("process") {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    };

    // §3.7：先落账（用量 + 审计），后播流/响应（断连不影响账本）
    state.keys.record_usage(
        &rec.id,
        output.usage.prompt_tokens as u64,
        output.usage.completion_tokens as u64,
    );
    state.audit.record(&crate::audit::AuditEntry {
        key_id: &rec.id,
        key_prefix: &rec.prefix,
        request_id: &request_id,
        expression: &expr,
        kind: "ok",
        outcome: &lug_engine::value::format_num_value(&output.result).to_display(),
        prompt_tokens: output.usage.prompt_tokens as u64,
        completion_tokens: output.usage.completion_tokens as u64,
        duration_ms: started.elapsed().as_millis(),
    });

    if process {
        return sse_stream(&expr, &output);
    }
    json_response(
        StatusCode::OK,
        json!({
            "id": format!("eval-{request_id}"),
            "object": "evaluation",
            "created": unix_now(),
            "model": "calculator-1",
            "expression": expr,
            "result": num_to_json(&output.result),
            "usage": usage_json(&output.usage),
        }),
    )
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn random_hex(n: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; n.div_ceil(2)];
    rand::rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect::<String>()[..n].to_string()
}

fn usage_json(u: &lug_engine::Usage) -> serde_json::Value {
    json!({
        "prompt_tokens": u.prompt_tokens,
        "completion_tokens": u.completion_tokens,
        "total_tokens": u.total_tokens,
        "completion_tokens_details": {"reasoning_tokens": u.reasoning_tokens},
    })
}

// ------------------------------------------------------------------ SSE 过程流

/// 过程流：prefix → step×N（cost_tokens 恒为 1，D2）→ final → [DONE]。
/// 零延迟（§4.2：事件即算即发）；引擎结果已就绪，此处只做序列化。
fn sse_stream(expr: &str, output: &lug_engine::EvalOutput) -> Response {
    use axum::response::sse::{Event, KeepAlive};
    let mut events: Vec<Result<Event, Infallible>> = Vec::new();

    events.push(Ok(Event::default()
        .json_data(json!({
            "type": "prefix", "prefix": output.prefix_display,
        }))
        .unwrap_or_else(|_| Event::default().data("{}"))));

    for st in &output.steps {
        events.push(Ok(Event::default()
            .json_data(json!({
                "type": "step",
                "cost_tokens": 1,
                "step": st.step,
                "expr": st.expr,
                "operator": st.operator,
                "operator_code": st.operator_code,
                "result": num_to_json(&st.result),
                "state": st.state,
            }))
            .unwrap_or_else(|_| Event::default().data("{}"))));
    }

    events.push(Ok(Event::default()
        .json_data(json!({
            "type": "final",
            "expression": expr,
            "result": num_to_json(&output.result),
            "steps_count": output.steps.len(),
            "usage": usage_json(&output.usage),
        }))
        .unwrap_or_else(|_| Event::default().data("{}"))));

    events.push(Ok(Event::default().data("[DONE]")));

    Sse::new(tokio_stream::iter(events))
        .keep_alive(KeepAlive::default())
        .into_response()
}

// ------------------------------------------------------------------ 兜底

/// 全部未匹配路由：OPTIONS → 204 + CORS；其余 → OpenAI 风格 404
/// （文案列出 v1 实际可用路由——文档写对原则）。
async fn fallback(method: Method, req: Request<Body>) -> Response {
    let path = req.uri().path().to_string();
    let mut res = match method {
        Method::OPTIONS => (StatusCode::NO_CONTENT, "").into_response(),
        Method::POST => api_error(
            StatusCode::NOT_FOUND,
            &format!("Unknown request URL (POST {path}). Only POST /f/a/o/calculate is available."),
            "invalid_request_error",
            None,
        ),
        method => api_error(
            StatusCode::NOT_FOUND,
            &format!(
                "Unknown request URL ({method} {path}). Available: / /docs /platform, \
                 GET /f/a/o/operator, GET /f/a/o/platform/auth|me|keys|usage, \
                 POST /f/a/o/calculate"
            ),
            "invalid_request_error",
            None,
        ),
    };
    // axum 0.8 的 method_not_allowed_fallback 不经过 Router::layer，CORS 需手动补
    apply_cors(&path, res.headers_mut());
    res
}
