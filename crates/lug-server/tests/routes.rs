//! 服务器集成测试：HTTP 契约（CORS / 鉴权 / 计算响应 / SSE 序列 / 404 文案）
//! 与 KeyStore 的 Python 格式兼容性。经 tower oneshot 驱动，无需真实网络。

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use lug_server::config::Config;
use lug_server::keystore::KeyStore;
use lug_server::routes;
use std::path::PathBuf;
use tower::ServiceExt;

fn test_config(dir: &std::path::Path) -> Config {
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

async fn app_with_key(dir: &std::path::Path) -> (axum::Router, String) {
    let cfg = test_config(dir);
    let keys = KeyStore::open(cfg.api_keys_path.clone()).unwrap();
    let (raw, _rec) = keys.create(Some("test".into()), "local").unwrap();
    let state = std::sync::Arc::new(routes::AppState {
        config: cfg,
        keys,
        audit: lug_server::audit::AuditLog::open(dir.join("audit.log")),
        auth: lug_server::platform::AuthConfig::default(),
        flow: std::sync::Arc::new(FakeFlow::default()),
        sessions: lug_server::platform::SessionStore::new(lug_server::platform::SESSION_TTL),
    });
    (routes::router(state), raw)
}

// ------------------------------------------------------------------ 平台控制台（GitHub 登录 / 会话 / Key）

use lug_server::platform::{AuthConfig, GithubFlow, OAuthError, StateStore, User};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

type BoxFut = Pin<Box<dyn Future<Output = Result<User, OAuthError>> + Send>>;

/// 测试替身：state 流程复用真实 StateStore，exchange 按code 返回固定用户。
struct FakeFlow {
    states: StateStore,
    users: Mutex<std::collections::HashMap<String, User>>,
}

impl Default for FakeFlow {
    fn default() -> Self {
        Self {
            states: StateStore::new(),
            users: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl FakeFlow {
    fn with_user(uid: u64, login: &str) -> Self {
        let f = Self::default();
        f.users.lock().unwrap().insert(
            format!("code-{uid}"),
            User {
                provider: "github".into(),
                id: uid.to_string(),
                login: login.into(),
            },
        );
        f
    }
}

impl GithubFlow for FakeFlow {
    fn enabled(&self) -> bool {
        true
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
        let user = self.users.lock().unwrap().get(&code).cloned();
        Box::pin(async move { user.ok_or(OAuthError::GithubUnavailable) })
    }
}

fn platform_app(dir: &std::path::Path, flow: FakeFlow) -> axum::Router {
    let cfg = test_config(dir);
    let keys = KeyStore::open(cfg.api_keys_path.clone()).unwrap();
    let state = std::sync::Arc::new(routes::AppState {
        config: cfg,
        keys,
        audit: lug_server::audit::AuditLog::open(dir.join("audit.log")),
        auth: AuthConfig::default(),
        flow: std::sync::Arc::new(flow),
        sessions: lug_server::platform::SessionStore::new(lug_server::platform::SESSION_TTL),
    });
    routes::router(state)
}

async fn request(
    app: axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (axum::http::StatusCode, axum::http::HeaderMap, String) {
    let mut builder = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let res = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let hdrs = res.headers().clone();
    let text =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    (status, hdrs, text)
}

fn set_cookie(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all("Set-Cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with(&format!("{name}=")))
        .map(|c| c.split(';').next().unwrap().to_string())
}

/// 完整登录：authorize → callback → (会话 Cookie, CSRF, 用户)。
async fn login(
    app: &axum::Router,
    uid: u64,
    login_name: &str,
) -> (String, String, serde_json::Value) {
    let (st, hdrs, _) = request(app.clone(), "GET", "/f/a/o/platform/auth/github", &[], "").await;
    assert_eq!(st, axum::http::StatusCode::FOUND);
    let loc = hdrs.get("Location").unwrap().to_str().unwrap().to_string();
    let state = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let state_cookie = set_cookie(&hdrs, "lug_oauth_state").unwrap();
    let (st, hdrs, _) = request(
        app.clone(),
        "GET",
        &format!("/f/a/o/platform/auth/github/callback?state={state}&code=code-{uid}"),
        &[("Cookie", &state_cookie)],
        "",
    )
    .await;
    assert_eq!(
        st,
        axum::http::StatusCode::FOUND,
        "callback should redirect"
    );
    assert_eq!(hdrs.get("Location").unwrap(), "/platform");
    let session_cookie = set_cookie(&hdrs, "lug_session").unwrap();
    let (st, _, body) = request(
        app.clone(),
        "GET",
        "/f/a/o/platform/me",
        &[("Cookie", &session_cookie)],
        "",
    )
    .await;
    assert_eq!(st, axum::http::StatusCode::OK, "me after login: {body}");
    let me: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    assert_eq!(me["user"]["login"], login_name, "me after login: {body}");
    (
        session_cookie,
        me["csrf_token"].as_str().unwrap_or_default().to_string(),
        me,
    )
}

async fn post_json(
    app: axum::Router,
    path: &str,
    key: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method("POST").uri(path);
    if let Some(k) = key {
        builder = builder.header("Authorization", format!("Bearer {k}"));
    }
    let res = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let text =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    (status, text)
}

#[tokio::test]
async fn calculate_requires_api_key() {
    let tmp = tempfile_dir();
    let (app, _) = app_with_key(&tmp).await;
    let (status, body) = post_json(app, "/f/a/o/calculate", None, r#"{"expression":"1+1"}"#).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("invalid_api_key"), "{body}");
}

#[tokio::test]
async fn calculate_json_response() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;
    let (status, body) = post_json(
        app,
        "/f/a/o/calculate",
        Some(&key),
        r#"{"expression":"3+5*2**(1+1)"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["object"], "evaluation");
    assert_eq!(v["model"], "calculator-1");
    assert_eq!(v["result"], 23);
    assert_eq!(v["usage"]["prompt_tokens"], 11);
    // 计费口径 D2：reasoning = 步骤数
    assert_eq!(
        v["usage"]["completion_tokens_details"]["reasoning_tokens"],
        4
    );
    assert_eq!(v["usage"]["completion_tokens"], 5);
    assert_eq!(v["usage"]["total_tokens"], 16);
}

#[tokio::test]
async fn calculate_engine_errors_are_openai_style() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;
    let (status, body) = post_json(
        app,
        "/f/a/o/calculate",
        Some(&key),
        r#"{"expression":"1/0"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["error"]["message"],
        "Step 1 failed: 1/0: Division by zero"
    );
    assert_eq!(v["error"]["code"], "computation_error");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert_eq!(v["error"]["param"], serde_json::Value::Null);
}

#[tokio::test]
async fn calculate_body_errors_match_prototype() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;
    // 空 body
    let (s1, b1) = post_json(app.clone(), "/f/a/o/calculate", Some(&key), "").await;
    assert_eq!(s1, StatusCode::BAD_REQUEST);
    assert_eq!(
        b1,
        r#"{"error":{"message":"Request body is empty.","type":"invalid_request_error","param":null,"code":null}}"#
    );
    // 非 JSON
    let (s2, b2) = post_json(app.clone(), "/f/a/o/calculate", Some(&key), "not-json").await;
    assert_eq!(s2, StatusCode::BAD_REQUEST);
    assert!(b2.contains("Invalid JSON in request body."));
    // 缺 expression
    let (s3, b3) = post_json(app, "/f/a/o/calculate", Some(&key), r#"{"x":1}"#).await;
    assert_eq!(s3, StatusCode::BAD_REQUEST);
    assert!(
        b3.contains(r#"e.g. {\"expression\": \"3+5*2**(1+1)\"}"#)
            || b3.contains("Input must be an expression string")
    );
}

#[tokio::test]
async fn expression_length_guard() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;
    let long = "1+".repeat(600) + "1"; // 1201 字符 > 1024
    let (status, body) = post_json(
        app,
        "/f/a/o/calculate",
        Some(&key),
        &format!(r#"{{"expression":"{long}"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("Expression too long"), "{body}");
}

#[tokio::test]
async fn sse_event_sequence() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/f/a/o/calculate")
                .header("Authorization", format!("Bearer {key}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"expression":"1+2*3","process":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    // 事件序列：prefix → step×2 → final → [DONE]；cost_tokens 恒为 1（D2）
    let events: Vec<&str> = body
        .split("\n\n")
        .filter(|s| s.starts_with("data: "))
        .collect();
    assert_eq!(events.len(), 5, "{body}");
    assert!(
        events[0].contains(r#""type":"prefix""#) && events[0].contains(r#""prefix":"+ 1 * 2 3""#),
        "{}",
        events[0]
    );
    assert!(
        events[1].contains(r#""cost_tokens":1"#) && events[1].contains(r#""expr":"2*3""#),
        "{}",
        events[1]
    );
    assert!(
        events[3].contains(r#""type":"final""#) && events[3].contains(r#""steps_count":2"#),
        "{}",
        events[3]
    );
    assert_eq!(events[4], "data: [DONE]");
}

#[tokio::test]
async fn operator_table_matches_prototype() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/f/a/o/operator")
                .header("Authorization", format!("Bearer {key}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["object"], "operator_code_table");
    let first = &v["data"][0];
    assert_eq!(first["code"], 0);
    assert_eq!(first["symbol"], "+");
    assert_eq!(first["name"], "加法");
    // 单词别名按 Python sorted()（码点序）
    assert_eq!(
        first["aliases"],
        serde_json::json!(["add", "plus", "加", "加上"]) // D7: sum 升格为聚合函数
    );
    assert_eq!(v["data"][6]["symbol"], "**");
    assert_eq!(v["data"][6]["code"], 6);
}

#[tokio::test]
async fn options_preflight_and_404_messages() {
    let tmp = tempfile_dir();
    let (app, _key) = app_with_key(&tmp).await;
    // OPTIONS 预检 → 204 + CORS
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/f/a/o/calculate")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        res.headers().get("Access-Control-Allow-Origin").unwrap(),
        "*"
    );
    // GET 未知路径 → 404 文案列实际路由
    let res = app
        .clone()
        .oneshot(Request::builder().uri("/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    // POST 未知路径 → 专用文案
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/nope")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let text =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(
        text.contains("Only POST /f/a/o/calculate is available"),
        "{text}"
    );
}

/// KeyStore 与 Python 原型 JSON 格式互通（手写一条原型格式记录，读取并校验）。
#[test]
fn keystore_reads_python_format() {
    let dir = tempfile_dir();
    let key_hash = {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(b"sk-test123");
        d.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let py_style = format!(
        r#"{{"keys": {{"abc123": {{"id": "abc123", "owner_id": "github:1", "name": "py",
        "key_hash": "{key_hash}", "prefix": "sk-test123...", "created_at": 1700000000,
        "status": "active", "last_used": null,
        "usage": {{"requests": 1, "prompt_tokens": 5, "completion_tokens": 6, "total_tokens": 11}}}}}}}}"#
    );
    std::fs::write(dir.join("api_keys.json"), py_style).unwrap();
    let ks = KeyStore::open(dir.join("api_keys.json")).unwrap();
    let rec = ks.validate("sk-test123").expect("应命中 Python 写入的记录");
    assert_eq!(rec.id, "abc123");
    assert_eq!(rec.usage.requests, 1);
    // 撤销后失效
    assert!(ks.revoke("abc123"));
    assert!(ks.validate("sk-test123").is_none());
}

/// 损坏的存储文件必须拒绝启动（不覆盖用户数据，与原型一致）。
#[test]
fn keystore_rejects_corrupt_file() {
    let dir = tempfile_dir();
    std::fs::write(dir.join("api_keys.json"), "{broken").unwrap();
    assert!(KeyStore::open(dir.join("api_keys.json")).is_err());
}

fn tempfile_dir() -> PathBuf {
    // 唯一性用原子计数器保证：SystemTime 纳秒在 macOS 上只有微秒粒度，
    // 并行测试会拿到同名目录 → rename 竞态（ENOENT）
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "lug-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 审计日志（§3.7 审计不能省）：成功与错误都留痕，字段完整，
/// 且审计写入先于响应返回（先落账后播流/响应）。
#[tokio::test]
async fn audit_log_records_ok_and_error() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;

    // 成功 + 错误各一笔
    let _ = post_json(
        app.clone(),
        "/f/a/o/calculate",
        Some(&key),
        r#"{"expression":"3+5*2**(1+1)"}"#,
    )
    .await;
    let _ = post_json(
        app,
        "/f/a/o/calculate",
        Some(&key),
        r#"{"expression":"1/0"}"#,
    )
    .await;

    let lines: Vec<serde_json::Value> = std::fs::read_to_string(tmp.join("audit.log"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");

    let ok = &lines[0];
    assert_eq!(ok["kind"], "ok");
    assert_eq!(ok["expression"], "3+5*2**(1+1)");
    assert_eq!(ok["result"], "23");
    assert_eq!(ok["usage"]["prompt_tokens"], 11);
    assert!(ok["request_id"].as_str().unwrap().len() == 16);
    assert!(ok["duration_ms"].is_u64());

    let err = &lines[1];
    assert_eq!(err["kind"], "error");
    assert_eq!(err["expression"], "1/0");
    assert!(err["result"].as_str().unwrap().contains("Division by zero"));
    assert_eq!(err["request_id"], "-");

    // 同一 Key 的聚合计数：错误请求不计费（requests 只算成功；审计日志已单独留痕）
    let ks = KeyStore::open(tmp.join("api_keys.json")).unwrap();
    let recs = ks.list("local");
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].usage.requests, 1);
    assert_eq!(recs[0].usage.total_tokens, 16);
}

/// 存量兼容：原型早期记录缺 owner_id 字段（旧密钥），必须可读且视作 local。
#[test]
fn keystore_reads_legacy_records_without_owner() {
    let dir = tempfile_dir();
    let key_hash = {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(b"sk-legacy");
        d.iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    // 服务器上真实存在的旧格式：无 owner_id 字段
    let legacy = format!(
        r#"{{"keys": {{"old1": {{"id": "old1", "name": "legacy",
        "key_hash": "{key_hash}", "prefix": "sk-legacy...",
        "created_at": 1700000000, "status": "active", "last_used": null,
        "usage": {{"requests": 0, "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}}}}}}}}"#
    );
    std::fs::write(dir.join("api_keys.json"), legacy).unwrap();
    let ks = KeyStore::open(dir.join("api_keys.json")).unwrap();
    let rec = ks.validate("sk-legacy").expect("旧格式记录必须可用");
    assert_eq!(rec.owner_id, "local");
}

/// L1 语义层经 API 生效：口语输入 → 规范计算（MoO 第二层增强）。
#[tokio::test]
async fn calculate_accepts_colloquial_input() {
    let tmp = tempfile_dir();
    let (app, key) = app_with_key(&tmp).await;
    // prompt_tokens = 词法 token 数：续接（3万5千）合并为单 token，属正确词法
    for (expr, expect, prompt) in [
        ("三加五乘二", 13, 5u64),
        ("3万5千", 35000, 1u64),
        ("两点五 * 2", 5, 3u64),
    ] {
        let (status, body) = post_json(
            app.clone(),
            "/f/a/o/calculate",
            Some(&key),
            &format!(r#"{{"expression":"{expr}"}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{expr}: {body}");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"], expect, "{expr}: {body}");
        assert_eq!(v["usage"]["prompt_tokens"], prompt, "{expr}");
    }
}

// ================================================================== 平台控制台契约

#[tokio::test]
async fn platform_auth_status_is_public() {
    let tmp = tempfile_dir();
    let app = platform_app(&tmp, FakeFlow::default());
    let (st, hdrs, body) = request(app, "GET", "/f/a/o/platform/auth", &[], "").await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v, serde_json::json!({"github_enabled": true}));
    // 平台路由收紧：no-store，无公开 CORS
    assert_eq!(hdrs.get("Cache-Control").unwrap(), "no-store");
    assert!(hdrs.get("Access-Control-Allow-Origin").is_none());
}

#[tokio::test]
async fn platform_requires_session() {
    let tmp = tempfile_dir();
    let app = platform_app(&tmp, FakeFlow::default());
    for path in ["keys", "usage", "me"] {
        let (st, _, body) = request(
            app.clone(),
            "GET",
            &format!("/f/a/o/platform/{path}"),
            &[],
            "",
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{path}: {body}");
        assert!(body.contains("invalid_session"), "{path}: {body}");
    }
    let (st, _, body) = request(
        app.clone(),
        "POST",
        "/f/a/o/platform/keys",
        &[],
        r#"{"name":"unauthorized"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{body}");
    let (st, _, body) = request(app, "DELETE", "/f/a/o/platform/keys/anything", &[], "").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{body}");
}

#[tokio::test]
async fn platform_full_login_and_key_lifecycle() {
    let tmp = tempfile_dir();
    let app = platform_app(&tmp, FakeFlow::with_user(101, "alice"));
    let (cookie, csrf, me) = login(&app, 101, "alice").await;
    assert_eq!(me["user"]["id"], "101");
    assert_eq!(me["user"]["provider"], "github");
    assert!(!csrf.is_empty());

    // keys 列表（空）
    let (st, _, body) = request(
        app.clone(),
        "GET",
        "/f/a/o/platform/keys",
        &[("Cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["object"], "list");
    assert_eq!(v["data"], serde_json::json!([]));

    // 创建 Key：无 CSRF → 403；有 CSRF → 明文仅本次返回
    let (st, _, body) = request(
        app.clone(),
        "POST",
        "/f/a/o/platform/keys",
        &[("Cookie", &cookie)],
        r#"{"name":"k1"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    assert!(body.contains("invalid_csrf"), "{body}");

    let (st, _, body) = request(
        app.clone(),
        "POST",
        "/f/a/o/platform/keys",
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        r#"{"name":"k1"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let created: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(created["key"].as_str().unwrap().starts_with("sk-"));
    assert_eq!(
        created["note"],
        "Save this key now; it will not be shown again."
    );
    let kid = created["id"].as_str().unwrap().to_string();

    // 用量汇总包含新 Key（零用量）
    let (st, _, body) = request(
        app.clone(),
        "GET",
        "/f/a/o/platform/usage",
        &[("Cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["object"], "usage_report");
    assert_eq!(v["total"]["requests"], 0);
    assert_eq!(v["keys"].as_array().unwrap().len(), 1);
    // 公开视图绝不包含 key_hash / owner_id
    assert!(v["keys"][0].get("key_hash").is_none());
    assert!(v["keys"][0].get("owner_id").is_none());

    // 撤销
    let (st, _, body) = request(
        app.clone(),
        "DELETE",
        &format!("/f/a/o/platform/keys/{kid}"),
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        r#"{"id":"PLACEHOLDER","status":"revoked"}"#.replace("PLACEHOLDER", &kid)
    );
    // 再次撤销 → 404
    let (st, _, body) = request(
        app.clone(),
        "DELETE",
        &format!("/f/a/o/platform/keys/{kid}"),
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains(&format!("Unknown key id: {kid}")), "{body}");

    // 登出 → 会话失效
    let (st, hdrs, body) = request(
        app.clone(),
        "POST",
        "/f/a/o/platform/logout",
        &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // set_cookie helper 只取值段（分号前）：空值 = 清除指令（Max-Age=0 在属性里）
    let cleared = set_cookie(&hdrs, "lug_session").unwrap();
    assert_eq!(cleared, "lug_session=");
    let (st, _, body) = request(
        app.clone(),
        "GET",
        "/f/a/o/platform/me",
        &[("Cookie", &cookie)],
        "",
    )
    .await;
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "session must be revoked: {body}"
    );
}

#[tokio::test]
async fn platform_state_is_one_time_and_replay_is_rejected() {
    let tmp = tempfile_dir();
    let app = platform_app(&tmp, FakeFlow::with_user(101, "alice"));
    let (st, hdrs, _) = request(app.clone(), "GET", "/f/a/o/platform/auth/github", &[], "").await;
    assert_eq!(st, StatusCode::FOUND);
    let loc = hdrs.get("Location").unwrap().to_str().unwrap().to_string();
    let state = loc.split("state=").nth(1).unwrap().to_string();
    let state_cookie = set_cookie(&hdrs, "lug_oauth_state").unwrap();

    let (st, hdrs, _) = request(
        app.clone(),
        "GET",
        &format!("/f/a/o/platform/auth/github/callback?state={state}&code=code-101"),
        &[("Cookie", &state_cookie)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::FOUND);
    assert_eq!(hdrs.get("Location").unwrap(), "/platform");

    // 重放同一 state → login_error=invalid_oauth_state，绝不发会话
    let (st, hdrs, _) = request(
        app.clone(),
        "GET",
        &format!("/f/a/o/platform/auth/github/callback?state={state}&code=code-101"),
        &[("Cookie", &state_cookie)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::FOUND);
    let loc = hdrs.get("Location").unwrap().to_str().unwrap();
    assert!(loc.contains("login_error=invalid_oauth_state"), "{loc}");
    assert!(set_cookie(&hdrs, "lug_session").is_none());

    // 浏览器 Cookie 缺失（CSRF 绑定）→ 拒绝
    let (st, hdrs, _) = request(
        app.clone(),
        "GET",
        "/f/a/o/platform/auth/github/callback?state=st&code=code-101",
        &[],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::FOUND);
    let loc = hdrs.get("Location").unwrap().to_str().unwrap();
    assert!(loc.contains("login_error=invalid_oauth_state"), "{loc}");
}

#[tokio::test]
async fn platform_multi_user_isolation_idor() {
    let tmp = tempfile_dir();
    // flow 需同时注册 alice 与 bob 的 code（多用户隔离场景）
    let flow = FakeFlow::with_user(202, "bob");
    flow.users.lock().unwrap().insert(
        "code-101".into(),
        User {
            provider: "github".into(),
            id: "101".into(),
            login: "alice".into(),
        },
    );
    let app = platform_app(&tmp, flow);
    // 先给 alice 建一个 Key
    let (cookie_a, csrf_a, _) = login(&app, 101, "alice").await;
    let (st, _, body) = request(
        app.clone(),
        "POST",
        "/f/a/o/platform/keys",
        &[("Cookie", &cookie_a), ("X-CSRF-Token", &csrf_a)],
        r#"{"name":"alice-key"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let kid: String = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // bob 登录（同 flow 实例，code-202 → 用户 202）
    let (cookie_b, csrf_b, _) = login(&app, 202, "bob").await;

    // bob 看不到 alice 的 Key
    let (st, _, body) = request(
        app.clone(),
        "GET",
        "/f/a/o/platform/keys",
        &[("Cookie", &cookie_b)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        v["data"],
        serde_json::json!([]),
        "bob must not see alice keys: {body}"
    );

    // bob 无法撤销 alice 的 Key（IDOR → 404）
    let (st, _, body) = request(
        app.clone(),
        "DELETE",
        &format!("/f/a/o/platform/keys/{kid}"),
        &[("Cookie", &cookie_b), ("X-CSRF-Token", &csrf_b)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("Unknown key id"), "{body}");

    // alice 自己仍可撤销
    let (st, _, _) = request(
        app.clone(),
        "DELETE",
        &format!("/f/a/o/platform/keys/{kid}"),
        &[("Cookie", &cookie_a), ("X-CSRF-Token", &csrf_a)],
        "",
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn platform_cross_origin_write_rejected() {
    let tmp = tempfile_dir();
    let app = platform_app(&tmp, FakeFlow::with_user(101, "alice"));
    let (cookie, csrf, _) = login(&app, 101, "alice").await;
    // 跨站 Sec-Fetch-Site → 403
    let (st, _, body) = request(
        app.clone(),
        "POST",
        "/f/a/o/platform/keys",
        &[
            ("Cookie", &cookie),
            ("X-CSRF-Token", &csrf),
            ("Sec-Fetch-Site", "cross-site"),
        ],
        r#"{"name":"evil"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    // CSRF 错误 → 403
    let (st, _, body) = request(
        app.clone(),
        "POST",
        "/f/a/o/platform/keys",
        &[("Cookie", &cookie), ("X-CSRF-Token", "wrong")],
        r#"{"name":"evil"}"#,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
}

#[tokio::test]
async fn platform_legacy_owner_migration() {
    let dir = tempfile_dir();
    let legacy = r#"{"keys":{"abc123":{"id":"abc123","name":"old","key_hash":"00","prefix":"sk-x...yyyy","created_at":1,"status":"active","last_used":null,"usage":{"requests":0,"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}}}}"#;
    std::fs::write(dir.join("api_keys.json"), legacy).unwrap();
    let ks = KeyStore::open_with_legacy_migration(dir.join("api_keys.json"), "101").unwrap();
    let recs = ks.list_public("github:101");
    assert_eq!(recs.len(), 1, "legacy key must migrate to github:101");
    // 迁移必须持久化（重启后仍在）
    let ks2 = KeyStore::open(dir.join("api_keys.json")).unwrap();
    assert_eq!(ks2.list("github:101").len(), 1);
    // 未配置迁移目标时不迁移
    let dir2 = tempfile_dir();
    std::fs::write(dir2.join("api_keys.json"), legacy).unwrap();
    let ks3 = KeyStore::open_with_legacy_migration(dir2.join("api_keys.json"), "").unwrap();
    assert_eq!(ks3.list("local").len(), 1);
}
