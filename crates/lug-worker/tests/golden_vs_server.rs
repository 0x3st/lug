//! C10 golden 对照套件：lug-worker 纯函数 builders ↔ lug-server router() 实测
//! 输出**逐字节**对照（契约源 = lug-server，平移面逐项背书）。
//!
//! 方法：lug-server 以 dev-dependency 进程内链接（其模块与 AppState 字段全
//! pub），经 tower oneshot 驱动 `routes::router()`；KeyStore::create 造 Bearer
//! key，SessionStore::create + Cookie/X-CSRF-Token 造会话。lug-worker 侧只调
//! 纯函数 builders（D1/KV 绑定 host 不可构造），响应体与 lug-server 实测响应
//! 逐字节比对；随机/时钟字段（id/created、token）以 lug-server 实测值回填。
//!
//! 覆盖（对照清单）：①api_error/unauthorized 401 体 ②calculate 拒绝路径
//! ③engine_error_response ④calculate_ok_json（id/created 回填）⑤SSE body
//! ⑥operator_table_json ⑦usage_report_json ⑧keys list/create/revoke JSON
//! ⑨auth_status_json ⑩Set-Cookie 字节级（登录/登出/state cookie）
//! ⑪cookie_secure 全矩阵 vs lug-server AuthConfig::cookie_secure。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;

use lug_server::config::Config;
use lug_server::keystore::KeyStore;
use lug_server::platform::{
    AuthConfig, GithubFlow, OAuthError, SessionStore, StateStore, User, SESSION_TTL,
};
use lug_server::routes::{self, AppState};

use lug_worker::platform as wplatform;
use lug_worker::routes as wroutes;
use lug_worker::sse as wsse;
use lug_worker::storage::keystore::{KeyPublic, KeyRec};

// ------------------------------------------------------------------ 夹具

static DIR_SEQ: AtomicU32 = AtomicU32::new(0);

/// 每用例独立临时目录（KeyStore 文件 / 审计日志互不串扰）。
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lug-golden-{}-{}-{}",
        tag,
        std::process::id(),
        DIR_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn server_config(dir: &std::path::Path) -> Config {
    Config {
        host: "127.0.0.1".into(),
        port: 0,
        web_dir: dir.join("web"),
        api_keys_path: dir.join("api_keys.json"),
        audit_log_path: dir.join("audit.log"),
        max_value_digits: 10_000_000,
        max_expr_len: 1024,
    }
}

fn test_user(uid: u64, login: &str) -> User {
    User {
        provider: "github".into(),
        id: uid.to_string(),
        login: login.into(),
    }
}

type BoxFut = Pin<Box<dyn Future<Output = Result<User, OAuthError>> + Send>>;

/// 测试替身 flow（形态同 lug-server tests/routes.rs 的 FakeFlow）：
/// state 流程复用真实 StateStore，exchange 按 code 返回固定用户。
struct FakeFlow {
    enabled: bool,
    states: StateStore,
    users: HashMap<String, User>,
}

impl FakeFlow {
    fn enabled() -> Self {
        Self {
            enabled: true,
            states: StateStore::new(),
            users: [("code-1".to_string(), test_user(1, "octo"))].into_iter().collect(),
        }
    }

    fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::enabled()
        }
    }
}

impl GithubFlow for FakeFlow {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn begin(&self) -> Result<(String, String), OAuthError> {
        let (state, _verifier) = self.states.create();
        Ok((
            state.clone(),
            format!("https://github.com/login/oauth/authorize?state={state}"),
        ))
    }

    fn consume(&self, state: &str, browser_state: &str) -> Result<String, OAuthError> {
        self.states.consume(state, browser_state)
    }

    fn exchange(&self, code: String, _verifier: String) -> BoxFut {
        let user = self.users.get(&code).cloned();
        Box::pin(async move { user.ok_or(OAuthError::GithubUnavailable) })
    }
}

struct ServerApp {
    app: Router,
    raw_key: String,
    state: Arc<AppState>,
}

fn server_app(dir: &std::path::Path, auth: AuthConfig, flow: FakeFlow) -> ServerApp {
    let cfg = server_config(dir);
    let keys = KeyStore::open(cfg.api_keys_path.clone()).unwrap();
    let (raw_key, _rec) = keys.create(Some("golden".into()), "local").unwrap();
    let state = Arc::new(AppState {
        config: cfg,
        keys,
        audit: lug_server::audit::AuditLog::open(dir.join("audit.log")),
        auth,
        flow: Arc::new(flow),
        sessions: SessionStore::new(SESSION_TTL),
    });
    let app = routes::router(state.clone());
    ServerApp { app, raw_key, state }
}

/// 直签一个会话（SessionStore::create + Cookie 造会话，同任务勘察面）。
fn make_session(sa: &ServerApp) -> (String, String) {
    let token = sa.state.sessions.create(test_user(123, "octo")).unwrap();
    let csrf = sa.state.sessions.get(&token).unwrap().csrf_token;
    (token, csrf)
}

/// tower oneshot 驱动 lug-server router()（契约源实测）。
async fn serve(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (StatusCode, HeaderMap, axum::body::Bytes) {
    let mut builder = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_vec())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let hdrs = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, hdrs, bytes)
}

/// lug-worker 纯函数 builder 的响应体字节。
async fn worker_bytes(res: axum::response::Response) -> axum::body::Bytes {
    res.into_body().collect().await.unwrap().to_bytes()
}

/// 逐字节对照（契约源 = lug-server 实测输出在前）。
fn assert_bytes_eq(server: &[u8], worker: &[u8], ctx: &str) {
    assert_eq!(
        server,
        worker,
        "golden mismatch: {ctx}\n  lug-server: {}\n  lug-worker: {}",
        String::from_utf8_lossy(server),
        String::from_utf8_lossy(worker),
    );
}

fn set_cookies(hdrs: &HeaderMap) -> Vec<String> {
    hdrs.get_all("Set-Cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(String::from)
        .collect()
}

fn host_headers(host: &str) -> HeaderMap {
    let mut hm = HeaderMap::new();
    hm.insert("Host", HeaderValue::from_str(host).unwrap());
    hm
}

/// lug-server keys JSON（public_rec_json 形态）→ lug-worker KeyPublic。
/// 反向喂给 worker 纯 builder，再与 lug-server 响应体逐字节对拍
/// （间接锁定 KeyPublic::to_public_json 的字段序）。
fn key_public_from(v: &serde_json::Value) -> KeyPublic {
    KeyPublic {
        id: v["id"].as_str().unwrap().into(),
        name: v["name"].as_str().unwrap().into(),
        prefix: v["prefix"].as_str().unwrap().into(),
        created_at: v["created_at"].as_i64().unwrap(),
        status: v["status"].as_str().unwrap().into(),
        last_used: v["last_used"].as_i64(),
        usage_requests: v["usage"]["requests"].as_u64().unwrap(),
        usage_prompt_tokens: v["usage"]["prompt_tokens"].as_u64().unwrap(),
        usage_completion_tokens: v["usage"]["completion_tokens"].as_u64().unwrap(),
        usage_total_tokens: v["usage"]["total_tokens"].as_u64().unwrap(),
    }
}

// ------------------------------------------------------------------ ① 401 体

#[tokio::test]
async fn golden_01_api_error_and_unauthorized_401_body() {
    let dir = temp_dir("01-unauth");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());

    // lug-server 实测：无 Bearer 打 operator → 401 体
    let (st, hdrs, sbody) = serve(&sa.app, "GET", "/f/a/o/operator", &[], b"").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(hdrs["content-type"], "application/json");
    let wbody = worker_bytes(wroutes::unauthorized()).await;
    assert_bytes_eq(&sbody, &wbody, "401 unauthorized body");

    // api_error 公共构造两侧直拍（同入参 → 同字节）
    let sres = lug_server::routes::api_error(
        StatusCode::BAD_REQUEST,
        "boom msg",
        "invalid_request_error",
        Some("expression_error"),
    );
    let sbody = worker_bytes(sres).await;
    let wres = wroutes::api_error(
        StatusCode::BAD_REQUEST,
        "boom msg",
        "invalid_request_error",
        Some("expression_error"),
    );
    let wbody = worker_bytes(wres).await;
    assert_bytes_eq(&sbody, &wbody, "api_error body");

    // 会话 401 invalid_session：lug-server 实测 vs lug-worker 纯函数
    let (st, _, sbody) = serve(
        &sa.app,
        "GET",
        "/f/a/o/platform/me",
        &[("Cookie", "lug_session=bogus")],
        b"",
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let wbody = worker_bytes(wplatform::invalid_session_response()).await;
    assert_bytes_eq(&sbody, &wbody, "401 invalid_session body");

    // 会话 403 invalid_csrf：有效会话、同源、但无 CSRF 头
    let (token, _csrf) = make_session(&sa);
    let cookie = format!("lug_session={token}");
    let (st, _, sbody) = serve(
        &sa.app,
        "POST",
        "/f/a/o/platform/logout",
        &[
            ("Cookie", cookie.as_str()),
            ("Origin", "http://127.0.0.1"),
            ("Host", "127.0.0.1"),
        ],
        b"",
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let wbody = worker_bytes(wplatform::invalid_csrf_response()).await;
    assert_bytes_eq(&sbody, &wbody, "403 invalid_csrf body");
}

// ------------------------------------------------------ ② calculate 拒绝路径

#[tokio::test]
async fn golden_02_calculate_rejection_paths() {
    let dir = temp_dir("02-reject");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let bearer = format!("Bearer {}", sa.raw_key);
    let auth = [("Authorization", bearer.as_str())];

    let cases: &[(&str, &[u8])] = &[
        ("empty body", b""),
        ("invalid json", b"not json"),
        ("non-object", b"[1,2]"),
        ("missing expression", b"{}"),
        ("non-string expression", br#"{"expression": 42}"#),
        ("blank expression", br#"{"expression":"   "}"#),
    ];
    for (name, body) in cases {
        let (st, _, sbody) = serve(&sa.app, "POST", "/f/a/o/calculate", &auth, body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{name}");
        let rejection = match wroutes::parse_calculate_request(body, 1024) {
            Err(wroutes::CalcRejection::Response(res)) => res,
            Err(other) => panic!("{name}: expected plain rejection, got {other:?}"),
            Ok(_) => panic!("{name}: expected rejection, got Ok"),
        };
        let wbody = worker_bytes(rejection).await;
        assert_bytes_eq(&sbody, &wbody, name);
    }

    // 超长表达式：先审计留痕（服务端内）再返回同字节 400
    let long_expr = "1".repeat(1025);
    let body = format!(r#"{{"expression":"{long_expr}"}}"#).into_bytes();
    let (st, _, sbody) = serve(&sa.app, "POST", "/f/a/o/calculate", &auth, &body).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    match wroutes::parse_calculate_request(&body, 1024) {
        Err(wroutes::CalcRejection::TooLong {
            expression,
            message,
            response,
        }) => {
            assert_eq!(expression, long_expr);
            assert_eq!(message, "Expression too long (max 1024 characters)");
            let wbody = worker_bytes(response).await;
            assert_bytes_eq(&sbody, &wbody, "too long");
        }
        Err(other) => panic!("expected TooLong, got {other:?}"),
        Ok(_) => panic!("expected TooLong, got Ok"),
    }
}

// --------------------------------------------------- ③ engine_error_response

#[tokio::test]
async fn golden_03_engine_error_response() {
    let dir = temp_dir("03-engine-error");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let bearer = format!("Bearer {}", sa.raw_key);
    let auth = [("Authorization", bearer.as_str())];

    for expr in ["1+", "1/0"] {
        let body = format!(r#"{{"expression":"{expr}"}}"#).into_bytes();
        let (st, _, sbody) = serve(&sa.app, "POST", "/f/a/o/calculate", &auth, &body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{expr}");
        let err = lug_engine::evaluate(expr).unwrap_err();
        let wres = wroutes::engine_error_response(&err);
        assert_eq!(wres.status(), StatusCode::BAD_REQUEST, "{expr}");
        let wbody = worker_bytes(wres).await;
        assert_bytes_eq(&sbody, &wbody, expr);
    }
}

// ------------------------------------------- ④ calculate_ok_json（回填对照）

#[tokio::test]
async fn golden_04_calculate_ok_json() {
    let dir = temp_dir("04-ok-json");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let bearer = format!("Bearer {}", sa.raw_key);
    let auth = [("Authorization", bearer.as_str())];

    // Int / Big / Float 三态 + 整值浮点整数化
    for expr in ["3+5*2**(1+1)", "2**100", "1/4", "0.5*4"] {
        let body = format!(r#"{{"expression":"{expr}"}}"#).into_bytes();
        let (st, _, sbody) = serve(&sa.app, "POST", "/f/a/o/calculate", &auth, &body).await;
        assert_eq!(st, StatusCode::OK, "{expr}");
        // id/created 随机/时钟字段以 lug-server 实测值回填
        let sv: serde_json::Value = serde_json::from_slice(&sbody).unwrap();
        let request_id = sv["id"]
            .as_str()
            .unwrap()
            .strip_prefix("eval-")
            .unwrap()
            .to_string();
        let created = sv["created"].as_u64().unwrap();
        let output = lug_engine::evaluate(expr).unwrap();
        let wv = wroutes::calculate_ok_json(&request_id, created, expr, &output);
        let wbody = serde_json::to_vec(&wv).unwrap();
        assert_bytes_eq(&sbody, &wbody, expr);
    }
}

// ------------------------------------------------------------- ⑤ SSE body

#[tokio::test]
async fn golden_05_sse_body() {
    let dir = temp_dir("05-sse");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let bearer = format!("Bearer {}", sa.raw_key);
    let auth = [("Authorization", bearer.as_str())];

    // 多步 / 零步（常量）/ Big 结果
    for expr in ["1+2*3", "42", "2**100"] {
        let body = format!(r#"{{"expression":"{expr}","process":true}}"#).into_bytes();
        let (st, hdrs, sbody) = serve(&sa.app, "POST", "/f/a/o/calculate", &auth, &body).await;
        assert_eq!(st, StatusCode::OK, "{expr}");
        assert_eq!(hdrs["content-type"], "text/event-stream", "{expr}");
        let output = lug_engine::evaluate(expr).unwrap();
        let frames = wsse::build_events(expr, &output);
        let wbody: Vec<u8> = frames.concat().into_bytes();
        assert_bytes_eq(&sbody, &wbody, expr);
    }
}

// ------------------------------------------------------ ⑥ operator_table_json

#[tokio::test]
async fn golden_06_operator_table_json() {
    let dir = temp_dir("06-operator");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let bearer = format!("Bearer {}", sa.raw_key);
    let auth = [("Authorization", bearer.as_str())];

    let (st, _, sbody) = serve(&sa.app, "GET", "/f/a/o/operator", &auth, b"").await;
    assert_eq!(st, StatusCode::OK);
    let wbody = serde_json::to_vec(&wroutes::operator_table_json()).unwrap();
    assert_bytes_eq(&sbody, &wbody, "operator table");
}

// ------------------------------------------------------- ⑦ usage_report_json

#[tokio::test]
async fn golden_07_usage_report_json() {
    let dir = temp_dir("07-usage");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let (token, csrf) = make_session(&sa);
    let cookie = format!("lug_session={token}");
    let write_hdrs = [
        ("Cookie", cookie.as_str()),
        ("X-CSRF-Token", csrf.as_str()),
        ("Origin", "http://127.0.0.1"),
        ("Host", "127.0.0.1"),
    ];

    // 会话签一把 Key（owner=github:123），打一次 calculate 计费，再看用量
    let (st, _, cbody) = serve(
        &sa.app,
        "POST",
        "/f/a/o/platform/keys",
        &write_hdrs,
        br#"{"name":"golden"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&cbody));
    let created: serde_json::Value = serde_json::from_slice(&cbody).unwrap();
    let bearer = format!("Bearer {}", created["key"].as_str().unwrap());
    let (st, _, _) = serve(
        &sa.app,
        "POST",
        "/f/a/o/calculate",
        &[("Authorization", bearer.as_str())],
        br#"{"expression":"1+2"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, _, sbody) = serve(
        &sa.app,
        "GET",
        "/f/a/o/platform/usage",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let sv: serde_json::Value = serde_json::from_slice(&sbody).unwrap();
    let keys: Vec<KeyPublic> = sv["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(key_public_from)
        .collect();
    let wbody = serde_json::to_vec(&wroutes::usage_report_json(&keys)).unwrap();
    assert_bytes_eq(&sbody, &wbody, "usage report");
}

// ------------------------------------------------ ⑧ keys list/create/revoke

#[tokio::test]
async fn golden_08_keys_list_create_revoke_json() {
    let dir = temp_dir("08-keys");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let (token, csrf) = make_session(&sa);
    let cookie = format!("lug_session={token}");
    let write_hdrs = [
        ("Cookie", cookie.as_str()),
        ("X-CSRF-Token", csrf.as_str()),
        ("Origin", "http://127.0.0.1"),
        ("Host", "127.0.0.1"),
    ];

    // create：key_hash/owner_id 不出网，其余字段以 lug-server 实测值回填
    let (st, _, sbody) = serve(
        &sa.app,
        "POST",
        "/f/a/o/platform/keys",
        &write_hdrs,
        br#"{"name":"golden key"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let sv: serde_json::Value = serde_json::from_slice(&sbody).unwrap();
    let rec = KeyRec {
        id: sv["id"].as_str().unwrap().into(),
        owner_id: "github:123".into(),
        name: sv["name"].as_str().unwrap().into(),
        key_hash: "unused".into(),
        prefix: sv["prefix"].as_str().unwrap().into(),
        created_at: sv["created_at"].as_i64().unwrap(),
        status: "active".into(),
        last_used: None,
        usage_requests: 0,
        usage_prompt_tokens: 0,
        usage_completion_tokens: 0,
        usage_total_tokens: 0,
    };
    let raw = sv["key"].as_str().unwrap();
    let wbody = serde_json::to_vec(&wroutes::key_created_json(&rec, raw)).unwrap();
    assert_bytes_eq(&sbody, &wbody, "key create");

    // list
    let (st, _, sbody) = serve(
        &sa.app,
        "GET",
        "/f/a/o/platform/keys",
        &[("Cookie", cookie.as_str())],
        b"",
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let sv: serde_json::Value = serde_json::from_slice(&sbody).unwrap();
    let keys: Vec<KeyPublic> = sv["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(key_public_from)
        .collect();
    let wbody = serde_json::to_vec(&wroutes::keys_list_json(&keys)).unwrap();
    assert_bytes_eq(&sbody, &wbody, "keys list");

    // revoke
    let kid = rec.id.clone();
    let path = format!("/f/a/o/platform/keys/{kid}");
    let (st, _, sbody) = serve(&sa.app, "DELETE", &path, &write_hdrs, b"").await;
    assert_eq!(st, StatusCode::OK);
    let wbody = serde_json::to_vec(&wroutes::key_revoked_json(&kid, "revoked")).unwrap();
    assert_bytes_eq(&sbody, &wbody, "key revoke");

    // 重复撤销（仅 active 可撤销）→ 404 Unknown key id
    let (st, _, sbody) = serve(&sa.app, "DELETE", &path, &write_hdrs, b"").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let wres = wroutes::api_error(
        StatusCode::NOT_FOUND,
        &format!("Unknown key id: {kid}"),
        "invalid_request_error",
        None,
    );
    let wbody = worker_bytes(wres).await;
    assert_bytes_eq(&sbody, &wbody, "key revoke 404");

    // create 拒绝路径（与 calculate 同源错误）
    let cases: &[(&str, &[u8], fn() -> axum::response::Response)] = &[
        ("create empty body", b"", wroutes::reject_empty_body),
        ("create invalid json", b"not json", wroutes::reject_invalid_json),
        ("create non-object", b"[1]", wroutes::reject_not_object),
    ];
    for (name, body, build) in cases {
        let (st, _, sbody) = serve(&sa.app, "POST", "/f/a/o/platform/keys", &write_hdrs, body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{name}");
        let wbody = worker_bytes(build()).await;
        assert_bytes_eq(&sbody, &wbody, name);
    }
}

// --------------------------------------------------------- ⑨ auth_status_json

#[tokio::test]
async fn golden_09_auth_status_json() {
    let dir = temp_dir("09-auth-on");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::enabled());
    let (st, _, sbody) = serve(&sa.app, "GET", "/f/a/o/platform/auth", &[], b"").await;
    assert_eq!(st, StatusCode::OK);
    let wbody = serde_json::to_vec(&wroutes::auth_status_json(true)).unwrap();
    assert_bytes_eq(&sbody, &wbody, "auth status enabled");

    let dir = temp_dir("09-auth-off");
    let sa = server_app(&dir, AuthConfig::default(), FakeFlow::disabled());
    let (st, _, sbody) = serve(&sa.app, "GET", "/f/a/o/platform/auth", &[], b"").await;
    assert_eq!(st, StatusCode::OK);
    let wbody = serde_json::to_vec(&wroutes::auth_status_json(false)).unwrap();
    assert_bytes_eq(&sbody, &wbody, "auth status disabled");
}

// ------------------------------------- ⑩ Set-Cookie 字节级（登录/登出/state）

#[tokio::test]
async fn golden_10_set_cookie_bytes() {
    // 两个配置：无公网地址（按 Host 判定 Secure）+ https 公网地址（恒 Secure）
    for (public_url, host) in [
        ("", "localhost:8787"),
        ("https://lug.by530.com", "lug.by530.com"),
    ] {
        let dir = temp_dir("10-cookies");
        let auth = if public_url.is_empty() {
            AuthConfig::default()
        } else {
            AuthConfig::from_parts("cid", "csecret", public_url, "").unwrap()
        };
        let sa = server_app(&dir, auth, FakeFlow::enabled());
        let hm = host_headers(host);
        let host_hdr = ("Host", host);

        // login begin → state cookie（state 以 lug-server 实测值回填）
        let (st, hdrs, _) = serve(
            &sa.app,
            "GET",
            "/f/a/o/platform/auth/github",
            &[host_hdr],
            b"",
        )
        .await;
        assert_eq!(st, StatusCode::FOUND, "begin ({public_url})");
        let loc = hdrs["Location"].to_str().unwrap();
        let state = loc.split("state=").nth(1).unwrap().split('&').next().unwrap();
        let cookies = set_cookies(&hdrs);
        assert_eq!(cookies.len(), 1, "begin cookies ({public_url})");
        let expected = wplatform::cookie_header(
            public_url,
            &hm,
            wplatform::STATE_COOKIE,
            state,
            wplatform::STATE_TTL as i64,
        );
        assert_eq!(cookies[0], expected, "state cookie ({public_url})");

        // callback → state 清除 cookie + 会话 cookie（token 以实测值回填）
        let state_cookie = format!("{}={}", wplatform::STATE_COOKIE, state);
        let path = format!("/f/a/o/platform/auth/github/callback?state={state}&code=code-1");
        let (st, hdrs, _) = serve(
            &sa.app,
            "GET",
            &path,
            &[host_hdr, ("Cookie", state_cookie.as_str())],
            b"",
        )
        .await;
        assert_eq!(st, StatusCode::FOUND, "callback ({public_url})");
        assert_eq!(hdrs["Location"], "/platform");
        let cookies = set_cookies(&hdrs);
        assert_eq!(cookies.len(), 2, "callback cookies ({public_url})");
        let session_token = cookies
            .iter()
            .find(|c| c.starts_with("lug_session="))
            .unwrap()
            .split(';')
            .next()
            .unwrap()["lug_session=".len()..]
            .to_string();
        let expected_cleared = wplatform::cookie_header(
            public_url,
            &hm,
            wplatform::STATE_COOKIE,
            "",
            0,
        );
        let expected_session = wplatform::cookie_header(
            public_url,
            &hm,
            wplatform::SESSION_COOKIE,
            &session_token,
            SESSION_TTL as i64,
        );
        assert_eq!(cookies[0], expected_cleared, "cleared state cookie ({public_url})");
        assert_eq!(cookies[1], expected_session, "session cookie ({public_url})");

        // logout → 会话清除 cookie（CSRF 取 lug-server 实测会话）
        let csrf = sa.state.sessions.get(&session_token).unwrap().csrf_token;
        let session_cookie = format!("lug_session={session_token}");
        let origin = if public_url.is_empty() {
            format!("http://{host}")
        } else {
            public_url.to_string()
        };
        let (st, hdrs, body) = serve(
            &sa.app,
            "POST",
            "/f/a/o/platform/logout",
            &[
                host_hdr,
                ("Cookie", session_cookie.as_str()),
                ("X-CSRF-Token", csrf.as_str()),
                ("Origin", origin.as_str()),
            ],
            b"",
        )
        .await;
        assert_eq!(
            st,
            StatusCode::OK,
            "logout ({public_url}): {}",
            String::from_utf8_lossy(&body)
        );
        let cookies = set_cookies(&hdrs);
        assert_eq!(cookies.len(), 1, "logout cookies ({public_url})");
        let expected_logout =
            wplatform::cookie_header(public_url, &hm, wplatform::SESSION_COOKIE, "", 0);
        assert_eq!(cookies[0], expected_logout, "logout cleared cookie ({public_url})");
    }
}

// -------------------------------------- ⑪ cookie_secure 全矩阵（契约源对拍）

#[test]
fn golden_11_cookie_secure_matrix() {
    for public_url in ["", "https://lug.by530.com", "http://localhost:8787"] {
        let cfg = AuthConfig::from_parts("", "", public_url, "").unwrap();
        for host in [
            None,
            Some("localhost"),
            Some("127.0.0.1:8000"),
            Some("::1"),
            Some("[::1]:8000"),
            Some("lug.by530.com"),
            Some("LOCALHOST:8787"),
        ] {
            let server = cfg.cookie_secure(host);
            let worker = wplatform::cookie_secure(public_url, host);
            assert_eq!(
                server, worker,
                "cookie_secure mismatch: public_url={public_url:?} host={host:?}"
            );
        }
    }
}
