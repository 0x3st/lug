//! 路由与处理器：与 lug-server/src/routes.rs 契约逐项对齐（路由表 / CORS /
//! 错误结构 / fallback / SSE 事件序 / 计费口径）。T7.5 接线：业务逻辑入位，
//! 持久化经 T5 storage 后端、OAuth 经 T6 oauth 模块，随机性一律走
//! oauth/random.rs 的平台 CSPRNG（rand/getrandom 禁入）。
//!
//! 分层：本模块为**公共纯逻辑 + 存储胶水**，不依赖 wasm-only 入口，host 可
//! 编译可测（wasm 侧经 `entry` 挂到 `#[event(fetch)]`）。无 lug-server 对应
//! 失败面的 D1/KV 错误统一映射 500 `internal_error`（fail-closed，不泄露
//! 存储细节）；逐字节契约（错误结构/SSE/计费/operator 表）由
//! tests/golden_vs_server.rs 对照 lug-server 实测输出背书。

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;

use crate::oauth::{self, GithubFlow, WorkerAuthConfig, WorkerGithubFlow};
use crate::panic_guard;
use crate::storage::{self, keystore::KeyRec};

// ------------------------------------------------------------------ 应用状态

/// 服务端配置（lug-server config.rs 的 Workers 落点）：
/// 只保留两个护栏变量（HOST/PORT/WEB_DIR/API_KEYS_PATH/AUDIT_LOG_PATH
/// 均被平台能力替代，见 lug-migration/design/persistence.md §4）。
#[derive(Clone, Copy, Debug)]
pub struct WorkerConfig {
    pub max_value_digits: usize,
    pub max_expr_len: usize,
}

impl WorkerConfig {
    /// 引擎求值上下文（预算透传，同 lug-server Config::ctx）。
    pub fn ctx(&self) -> lug_engine::Ctx {
        lug_engine::Ctx {
            max_digits: self.max_value_digits,
        }
    }
}

/// 共享应用状态（每请求在 entry::from_env 构建一次；后端句柄 Arc 化——
/// `with_state` 要求 `Clone`，而 D1Database 非 Clone）。
#[derive(Clone)]
pub struct AppState {
    pub config: WorkerConfig,
    pub keys: Arc<storage::keystore::KeyStore>,
    pub audit: Arc<storage::audit::AuditLog>,
    pub sessions: Arc<storage::sessions::SessionStore>,
    pub auth: WorkerAuthConfig,
    pub flow: WorkerGithubFlow<D1StateStore>,
}

/// OAuth state 的 D1 存储适配器：把 T5 `storage::oauth_states::OAuthStateStore`
/// （put 恒用其内部 STATE_TTL=600）适配到 T6 `oauth::OAuthStateStore` trait
/// （put 显式收 ttl_secs）——两处 TTL 常量由常量对齐测试锁定一致。
/// `Arc` 满足 `WorkerGithubFlow<S>` 的 `S: Clone`（D1Database 非 Clone）。
#[derive(Clone)]
pub struct D1StateStore {
    inner: Arc<storage::oauth_states::OAuthStateStore>,
}

impl D1StateStore {
    pub fn open(db: worker::D1Database) -> Self {
        Self {
            inner: Arc::new(storage::oauth_states::OAuthStateStore::open(db)),
        }
    }
}

impl oauth::OAuthStateStore for D1StateStore {
    fn put(
        &self,
        state: String,
        verifier: String,
        _ttl_secs: u64,
    ) -> oauth::StoreFut<Result<(), oauth::OAuthError>> {
        let store = self.inner.clone();
        Box::pin(worker::send::SendFuture::new(async move {
            // ttl 由 storage 层按 STATE_TTL 派生（== trait 入参，见常量对齐测试）
            let now = storage::audit::unix_now();
            store
                .create(&state, &verifier, now)
                .await
                .map_err(|_| oauth::OAuthError::GithubUnavailable)
        }))
    }

    fn take(&self, state: String) -> oauth::StoreFut<Result<String, oauth::OAuthError>> {
        let store = self.inner.clone();
        Box::pin(worker::send::SendFuture::new(async move {
            let now = storage::audit::unix_now();
            match store.consume(&state, now).await {
                Ok(Some(verifier)) => Ok(verifier),
                // 不存在/已过期/已消费 → 单一错误码（防枚举，同原 StateStore）
                Ok(None) => Err(oauth::OAuthError::InvalidOauthState),
                // D1 故障：fail-closed（无法验证一次性语义即拒绝）
                Err(err) => {
                    worker::console_error!("oauth state consume failed: {err}");
                    Err(oauth::OAuthError::GithubUnavailable)
                }
            }
        }))
    }
}

// ------------------------------------------------------------------ router

/// lug 路由表（与 lug-server `router()` 同表；静态页面暂以 stub 占位，
/// C8 改由 Workers Static Assets 承载后删除）。
pub fn router(state: AppState) -> Router {
    Router::new()
        // 验活（JSON 状态体；worker 自有探针，无 lug-server 对应）
        .route("/healthz", get(healthz))
        // 静态页面（/ /index.html /docs /platform /assets/*）**不在 axum 挂路由**：
        // Workers Static Assets 资产优先路由直接命中（wrangler.toml [assets]，
        // run_worker_first = ["/f/a/o/*"] 只让 API 进 worker），未命中静态资源
        // 由 SPA 回退兜底——静态路径根本不会进 fetch handler。lug-server 的
        // serve_html_* / serve_asset 读盘逻辑随之退役（ASSETS binding 保留在
        // wrangler.toml 供未来模板化改写用）。
        // 计算 API
        .route("/f/a/o/operator", get(operator_table))
        .route("/f/a/o/calculate", post(calculate))
        // 控制台（GitHub OAuth + Key 管理）
        .route("/f/a/o/platform/auth", get(platform_auth_status))
        .route("/f/a/o/platform/me", get(platform_me))
        .route("/f/a/o/platform/usage", get(platform_usage))
        .route("/f/a/o/platform/auth/github", get(github_login))
        .route("/f/a/o/platform/auth/github/callback", get(github_callback))
        .route("/f/a/o/platform/logout", post(platform_logout))
        .route(
            "/f/a/o/platform/keys",
            get(platform_keys_list).post(platform_keys_create),
        )
        .route(
            "/f/a/o/platform/keys/{id}",
            axum::routing::delete(platform_key_revoke),
        )
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
pub async fn cors_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;
    apply_cors(&path, res.headers_mut());
    res
}

/// CORS/安全头公共实现：平台路由收紧（no-store / no-referrer），
/// 计算 API 公开 CORS。
pub fn apply_cors(path: &str, headers: &mut HeaderMap) {
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

/// OpenAI 风格错误结构（`{"error": {message, type, param, code}}`），
/// 与 lug-server/Python 原型逐字节契约一致。
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

/// Bearer API Key 鉴权失败的 401（消息与原型逐字节一致）。
pub const UNAUTHORIZED_MESSAGE: &str = "Missing or invalid API key. Create one in the platform console (GET /platform) and pass it in the 'Authorization: Bearer sk-xxx' header.";

pub fn unauthorized() -> Response {
    api_error(
        StatusCode::UNAUTHORIZED,
        UNAUTHORIZED_MESSAGE,
        "authentication_error",
        Some("invalid_api_key"),
    )
}

/// D1/KV 故障（lug-server 无对应失败面）：fail-closed 500，
/// 不泄露存储细节。平移面之外的新增错误分支。
pub fn storage_error_response() -> Response {
    api_error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Storage backend error.",
        "internal_error",
        None,
    )
}

// ------------------------------------------------------ 引擎数值 → JSON

/// 引擎数值 → JSON 数字。Big/Float 经**十进制字面量**构造（arbitrary_precision），
/// 输出与 Python `json.dumps` 逐字节一致（含 2.0、0.0→0 的双格式化语义）。
/// 逐字平移自 lug-server/src/routes.rs::num_to_json（契约源，只读）。
pub fn num_to_json(v: &lug_engine::Value) -> serde_json::Value {
    match v {
        lug_engine::Value::Int(i) => json!(i),
        lug_engine::Value::Big(b) => {
            serde_json::from_str(&b.to_string()).unwrap_or(serde_json::Value::Null)
        }
        lug_engine::Value::Float(f) => {
            // 先 format_num（整数化），再用 py repr 字面量（与 Python json 一致）
            let disp = lug_engine::value::format_num(*f).to_display();
            serde_json::from_str(&disp).unwrap_or(serde_json::Value::Null)
        }
    }
}

/// 计费用量 → JSON（字段序与 lug-server usage_json 一致；preserve_order 下
/// 插入序即输出序）。逐字平移自 lug-server/src/routes.rs::usage_json。
pub fn usage_json(u: &lug_engine::Usage) -> serde_json::Value {
    json!({
        "prompt_tokens": u.prompt_tokens,
        "completion_tokens": u.completion_tokens,
        "total_tokens": u.total_tokens,
        "completion_tokens_details": {"reasoning_tokens": u.reasoning_tokens},
    })
}

// ------------------------------------------------------------------ 计算接口纯面

/// POST /f/a/o/calculate 请求体解析结果。
pub struct CalculateRequest {
    pub expression: String,
    pub process: bool,
}

/// calculate 请求级拒绝（lug-server 内联 return 的结构化版本）：
/// `Response` = 直接返回（无审计）；`TooLong` = 先按 lug-server 顺序审计留痕
/// （kind=error、request_id="-"、不计费）再返回。
#[derive(Debug)]
pub enum CalcRejection {
    Response(Response),
    TooLong {
        expression: String,
        message: String,
        response: Response,
    },
}

/// calculate 前段（无存储纯函数）：请求体解析 → 表达式抽取 → 长度护栏
/// → process 真值判定。错误分支逐字平移自 lug-server calculate（消息
/// 逐字节一致），golden 对照测试背书。
pub fn parse_calculate_request(
    body: &[u8],
    max_expr_len: usize,
) -> Result<CalculateRequest, CalcRejection> {
    if body.is_empty() {
        return Err(CalcRejection::Response(reject_empty_body()));
    }
    let parsed: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Err(CalcRejection::Response(reject_invalid_json())),
    };
    let Some(obj) = parsed.as_object() else {
        return Err(CalcRejection::Response(reject_not_object()));
    };
    // ---- 表达式抽取（唯一输入形态）----
    let expr = match obj.get("expression").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Err(CalcRejection::Response(reject_missing_expression())),
    };
    // 表达式长度上限（§3.7 工作有界，D4 家族护栏）
    if expr.chars().count() > max_expr_len {
        let message = too_long_message(max_expr_len);
        return Err(CalcRejection::TooLong {
            response: too_long_response(&message),
            message,
            expression: expr,
        });
    }
    let process = process_truthy(obj.get("process"));
    Ok(CalculateRequest {
        expression: expr,
        process,
    })
}

/// 400 系拒绝响应（消息与 lug-server calculate 逐字节一致）。
pub fn reject_empty_body() -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        "Request body is empty.",
        "invalid_request_error",
        None,
    )
}

pub fn reject_invalid_json() -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        "Invalid JSON in request body.",
        "invalid_request_error",
        None,
    )
}

pub fn reject_not_object() -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        "Request body must be a JSON object.",
        "invalid_request_error",
        None,
    )
}

pub fn reject_missing_expression() -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        "Input must be an expression string, e.g. {\"expression\": \"3+5*2**(1+1)\"}",
        "invalid_request_error",
        None,
    )
}

pub fn too_long_message(max_expr_len: usize) -> String {
    format!("Expression too long (max {max_expr_len} characters)")
}

pub fn too_long_response(message: &str) -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        message,
        "invalid_request_error",
        Some("expression_error"),
    )
}

/// 引擎求值错误 → OpenAI 风格 400（code = expression_error|computation_error）。
pub fn engine_error_response(e: &lug_engine::ExprError) -> Response {
    api_error(
        StatusCode::BAD_REQUEST,
        &e.message,
        "invalid_request_error",
        Some(e.code.as_str()),
    )
}

/// process 真值判定（Python truthy：true / 非零数 / 非空串）。
/// 平移自 lug-server calculate 内联逻辑。
pub fn process_truthy(v: Option<&serde_json::Value>) -> bool {
    match v {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// 计算成功 JSON 响应体（字段序与 lug-server calculate 一致）。
/// id/created 由调用方注入（运行时随机/时钟；golden 对照以 lug-server
/// 实测响应的值回填）。
pub fn calculate_ok_json(
    request_id: &str,
    created: u64,
    expr: &str,
    output: &lug_engine::EvalOutput,
) -> serde_json::Value {
    json!({
        "id": format!("eval-{request_id}"),
        "object": "evaluation",
        "created": created,
        "model": "calculator-1",
        "expression": expr,
        "result": num_to_json(&output.result),
        "usage": usage_json(&output.usage),
    })
}

/// GET /f/a/o/operator：注册表生成的编码表（0–6 冻结区 + 单词别名）。
/// 平移自 lug-server operator_table 的表体构造（鉴权在 handler）。
pub fn operator_table_json() -> serde_json::Value {
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
    json!({"object": "operator_code_table", "data": data})
}

/// GET /f/a/o/platform/usage：当前用户的用量汇总（ lug-server platform_usage
/// 的纯面；keys 由调用方经 KeyStore::list_public 取得）。
pub fn usage_report_json(keys: &[storage::keystore::KeyPublic]) -> serde_json::Value {
    let data: Vec<serde_json::Value> = keys
        .iter()
        .map(storage::keystore::KeyPublic::to_public_json)
        .collect();
    let mut total = json!({
        "requests": 0u64, "prompt_tokens": 0u64,
        "completion_tokens": 0u64, "total_tokens": 0u64,
    });
    for k in &data {
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
    json!({"object": "usage_report", "total": total, "keys": data})
}

// ------------------------------------------------------------------ 时间与随机

/// 当前 Unix 秒（Workers `Date::now()`；与 lug-server `int(time.time())` 同粒度）。
pub fn unix_now_secs() -> u64 {
    storage::audit::unix_now().max(0) as u64
}

/// 毫秒时钟（Workers Date；用于 duration_ms）。
fn now_ms() -> u64 {
    worker::Date::now().as_millis()
}

/// 计时结束 → duration_ms（ lug-server Instant::elapsed().as_millis 的
/// Workers 等价物；负钟差钳 0）。
pub fn elapsed_ms(started_ms: u64) -> u128 {
    worker::Date::now().as_millis().saturating_sub(started_ms) as u128
}

/// `n` 个 hex 字符的请求 ID（lug-server random_hex 平移；随机源换成平台
/// CSPRNG）。仅限 wasm32 运行时调用（host 桩 panic）。
pub fn random_hex(n: usize) -> String {
    let buf = crate::oauth::random::token_bytes(n.div_ceil(2));
    crate::oauth::util::hex_encode(&buf)[..n].to_string()
}

// ------------------------------------------------------------------ 鉴权

/// Bearer 提取（ lug-server require_api_key 的头部解析半段）。
pub fn bearer_key(headers: &HeaderMap) -> &str {
    let auth = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    auth.strip_prefix("Bearer ").map(str::trim).unwrap_or("")
}

/// Bearer key → KeyStore 鉴权（hash 点查）。存储故障 → 500（见
/// [`storage_error_response`]），未命中 → 401。
async fn require_api_key(state: &AppState, headers: &HeaderMap) -> Result<KeyRec, Response> {
    let raw = bearer_key(headers);
    match state.keys.validate(raw).await {
        Ok(Some(rec)) => Ok(rec),
        Ok(None) => Err(unauthorized()),
        Err(err) => {
            worker::console_error!("api key validate failed: {err}");
            Err(storage_error_response())
        }
    }
}

// ------------------------------------------------------------------ 平台会话鉴权

/// 会话鉴权结果（lug-server routes.rs::SessionAuth 平移）。
struct SessionAuth {
    token: String,
    rec: storage::sessions::SessionRecord,
    owner: String,
}

/// Cookie / X-Session-Token 会话鉴权（lug-server auth_session 平移）：
/// KV 查询是唯一异步点；过期/无效 → 401 invalid_session；写操作要求
/// 同源 + CSRF（platform::write_guards）。KV 读失败 → fail-closed 401
/// （无法验证会话即拒绝；lug-server 无对应失败面）。
async fn auth_session(
    state: &AppState,
    headers: &HeaderMap,
    write: bool,
) -> Result<SessionAuth, Response> {
    worker::send::SendFuture::new(async {
        let (header_token, cookie_token) = crate::platform::session_tokens(headers);
        let token = header_token
            .clone()
            .or_else(|| cookie_token.clone())
            .unwrap_or_default();
        let rec = match state.sessions.get(&token).await {
            Ok(Some(rec)) => rec,
            Ok(None) => return Err(crate::platform::invalid_session_response()),
            Err(err) => {
                worker::console_error!("session lookup failed: {err}");
                return Err(crate::platform::invalid_session_response());
            }
        };
        // 注：Cookie 会话下 CSRF 不匹配统一在写操作的 403 invalid_csrf 中拒绝
        //（跨站/篡改的 CSRF 属于请求伪造，不泄露会话轮换信息）
        if write {
            crate::platform::write_guards(
                headers,
                &state.auth.public_url,
                &rec.csrf_token,
                header_token.is_some(),
            )?;
        }
        let owner = crate::platform::owner_id(&rec.user)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())?;
        Ok(SessionAuth { token, rec, owner })
    })
    .await
}

// ------------------------------------------------------------------ handlers

/// 验活：200 + JSON 状态体（自定结构，供部署探针/冒烟用）。
async fn healthz() -> Response {
    json_response(
        StatusCode::OK,
        json!({"ok": true, "service": "lug-worker", "phase": "wired"}),
    )
}

/// 未接线路由占位已全部退役——所有 /f/a/o/* 路由均为真实现；
/// 静态路径由 Workers Static Assets 承载（见 router() 注释）。

/// GET /f/a/o/operator：注册表生成的编码表（Bearer Key 鉴权）。
/// D1 绑定 future 非 Send（worker 绑定类型通病，tech-notes §1）：与官方
/// `#[worker::send]` 同构——handler 体包 `SendFuture`（Workers 单线程下安全）。
async fn operator_table(State(state): State<AppState>, headers: HeaderMap) -> Response {
    worker::send::SendFuture::new(async {
        if let Err(res) = require_api_key(&state, &headers).await {
            return res;
        }
        json_response(StatusCode::OK, operator_table_json())
    })
    .await
}

/// POST /f/a/o/calculate：唯一计算入口。
/// `{"expression": "...", "process": true?}` → JSON 或 SSE 过程流。
/// 流程与 lug-server calculate 逐段对齐：
/// 鉴权 → 解析/护栏 → 求值（panic_guard 包住）→ 先落账（用量+审计）→ 响应。
async fn calculate(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    worker::send::SendFuture::new(async {
        let rec = match require_api_key(&state, &headers).await {
            Ok(r) => r,
            Err(res) => return res,
        };

        let started = now_ms();
        let (expr, process) = match parse_calculate_request(&body, state.config.max_expr_len) {
            Ok(req) => (req.expression, req.process),
            Err(CalcRejection::Response(res)) => return res,
            Err(CalcRejection::TooLong {
                expression,
                message,
                response,
            }) => {
                // 错误不计费，但审计必须留痕
                state.audit.record(&storage::audit::AuditEntry {
                    key_id: &rec.id,
                    key_prefix: &rec.prefix,
                    request_id: "-",
                    expression: &expression,
                    kind: "error",
                    outcome: &message,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    duration_ms: elapsed_ms(started),
                }).await;
                return response;
            }
        };

        // ---- 求值（panic_guard 包住引擎调用：处理任意用户输入，wasm32 stable
        // panic=abort 下无害无效果，nightly --panic-unwind 后真正生效）----
        let evaluated = panic_guard::catch_unwind_async(async {
            lug_engine::evaluate_with(&expr, &state.config.ctx())
        })
        .await;
        let output = match evaluated {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                // 错误不计费，但审计留痕（kind=error，outcome=错误消息）
                state.audit.record(&storage::audit::AuditEntry {
                    key_id: &rec.id,
                    key_prefix: &rec.prefix,
                    request_id: "-",
                    expression: &expr,
                    kind: "error",
                    outcome: &e.message,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    duration_ms: elapsed_ms(started),
                }).await;
                return engine_error_response(&e);
            }
            Err(_) => {
                const MSG: &str = "internal panic during expression evaluation";
                state.audit.record(&storage::audit::AuditEntry {
                    key_id: &rec.id,
                    key_prefix: &rec.prefix,
                    request_id: "-",
                    expression: &expr,
                    kind: "error",
                    outcome: MSG,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    duration_ms: elapsed_ms(started),
                }).await;
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    MSG,
                    "internal_error",
                    None,
                );
            }
        };
        let request_id = random_hex(16);

        // §3.7：先落账（用量 + 审计），后播流/响应（断连不影响账本）；
        // record_usage 失败仅记日志不阻断（容错同原 `let _ = atomic_write`）。
        if let Err(err) = state
            .keys
            .record_usage(
                &rec.id,
                output.usage.prompt_tokens as u64,
                output.usage.completion_tokens as u64,
                storage::audit::unix_now(),
            )
            .await
        {
            worker::console_error!("record_usage failed (best-effort, not blocking): {err}");
        }
        state.audit.record(&storage::audit::AuditEntry {
            key_id: &rec.id,
            key_prefix: &rec.prefix,
            request_id: &request_id,
            expression: &expr,
            kind: "ok",
            outcome: &lug_engine::value::format_num_value(&output.result).to_display(),
            prompt_tokens: output.usage.prompt_tokens as u64,
            completion_tokens: output.usage.completion_tokens as u64,
            duration_ms: elapsed_ms(started),
        }).await;

        if process {
            return crate::sse::sse_axum_response(crate::sse::build_events(&expr, &output));
        }
        json_response(
            StatusCode::OK,
            calculate_ok_json(&request_id, unix_now_secs(), &expr, &output),
        )
    })
    .await
}

// ------------------------------------------------------------------ 平台控制台 API

/// 平台响应纯面（golden 对照测试直测；handlers 只做会话/存储胶水）。

/// GET /f/a/o/platform/auth：登录选项（公开）。
pub fn auth_status_json(github_enabled: bool) -> serde_json::Value {
    json!({"github_enabled": github_enabled})
}

/// GET /f/a/o/platform/keys：当前用户的 Key 列表（不含哈希/属主）。
pub fn keys_list_json(keys: &[storage::keystore::KeyPublic]) -> serde_json::Value {
    json!({
        "object": "list",
        "data": keys.iter().map(storage::keystore::KeyPublic::to_public_json).collect::<Vec<_>>(),
    })
}

/// POST /f/a/o/platform/keys：签发响应（明文仅本次返回；字段序同 lug-server）。
pub fn key_created_json(rec: &storage::keystore::KeyRec, raw: &str) -> serde_json::Value {
    json!({
        "id": rec.id, "name": rec.name, "key": raw,
        "prefix": rec.prefix, "created_at": rec.created_at,
        "note": "Save this key now; it will not be shown again.",
    })
}

/// DELETE /f/a/o/platform/keys/{id}：撤销响应。
pub fn key_revoked_json(id: &str, status: &str) -> serde_json::Value {
    json!({"id": id, "status": status})
}

/// GET /f/a/o/platform/auth：登录选项（公开）。
async fn platform_auth_status(State(state): State<AppState>) -> Response {
    json_response(StatusCode::OK, auth_status_json(state.flow.enabled()))
}

/// GET /f/a/o/platform/me：当前会话（用户 + CSRF）。
async fn platform_me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    worker::send::SendFuture::new(async {
        let s = match auth_session(&state, &headers, false).await {
            Ok(s) => s,
            Err(r) => return r,
        };
        json_response(
            StatusCode::OK,
            json!({"user": s.rec.user.to_json(), "csrf_token": s.rec.csrf_token}),
        )
    })
    .await
}

/// POST /f/a/o/platform/logout：撤销会话 + 清 Cookie。
async fn platform_logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    worker::send::SendFuture::new(async {
        let s = match auth_session(&state, &headers, true).await {
            Ok(s) => s,
            Err(r) => return r,
        };
        // 撤销尽力而为（KV delete 失败不阻断登出；传播 ≤60s 见 persistence.md R1）
        if let Err(err) = state.sessions.revoke(&s.token).await {
            worker::console_error!("session revoke failed (best-effort): {err}");
        }
        let cleared = crate::platform::cookie_header(
            &state.auth.public_url,
            &headers,
            crate::platform::SESSION_COOKIE,
            "",
            0,
        );
        let mut res = json_response(StatusCode::OK, json!({"ok": true}));
        res.headers_mut()
            .append("Set-Cookie", cleared.parse().unwrap());
        res
    })
    .await
}

/// GET /f/a/o/platform/keys：当前用户的 Key 列表（不含哈希/属主）。
async fn platform_keys_list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    worker::send::SendFuture::new(async {
        let s = match auth_session(&state, &headers, false).await {
            Ok(s) => s,
            Err(r) => return r,
        };
        match state.keys.list_public(&s.owner).await {
            Ok(keys) => json_response(StatusCode::OK, keys_list_json(&keys)),
            Err(err) => {
                worker::console_error!("keys list failed: {err}");
                storage_error_response()
            }
        }
    })
    .await
}

/// POST /f/a/o/platform/keys：签发新 Key（明文仅本次返回）。
async fn platform_keys_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    worker::send::SendFuture::new(async {
        let s = match auth_session(&state, &headers, true).await {
            Ok(s) => s,
            Err(r) => return r,
        };
        // 请求体校验错误消息与 calculate 同源（lug-server 逐字节一致）
        if body.is_empty() {
            return reject_empty_body();
        }
        let parsed: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => return reject_invalid_json(),
        };
        let Some(obj) = parsed.as_object() else {
            return reject_not_object();
        };
        let name = obj.get("name").and_then(|v| v.as_str()).map(str::to_string);
        // 明文 `sk-` key（36B base64url）+ 12 字符 id（6B hex）：
        // lug-server KeyStore::create 的随机半段上移到 handler，走平台 CSPRNG。
        let raw = format!(
            "sk-{}",
            crate::oauth::util::base64url(&crate::oauth::random::token_bytes(36))
        );
        let kid = crate::oauth::util::hex_encode(&crate::oauth::random::token_bytes(6));
        match state
            .keys
            .create(
                &kid,
                &raw,
                name.as_deref(),
                &s.owner,
                storage::audit::unix_now(),
            )
            .await
        {
            Ok(rec) => json_response(StatusCode::OK, key_created_json(&rec, &raw)),
            // lug-server 把持久化失败映射为 500 invalid_request_error（消息透传）
            Err(err) => api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &err.to_string(),
                "invalid_request_error",
                None,
            ),
        }
    })
    .await
}

/// DELETE /f/a/o/platform/keys/{id}：撤销当前用户自己的 Key。
async fn platform_key_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(kid): axum::extract::Path<String>,
) -> Response {
    worker::send::SendFuture::new(async {
        let s = match auth_session(&state, &headers, true).await {
            Ok(s) => s,
            Err(r) => return r,
        };
        match state.keys.revoke_scoped(&kid, &s.owner).await {
            Ok(Some(rec)) => {
                // Key 生命周期审计（§10.7 审计不能省）。kind 取值差异：
                // lug-server 写 "key_revoked"，但 D1 schema CHECK 只收
                // ok|error（persistence.md §2.2，T5 冻结）——写 "key_revoked"
                // 会被 CHECK 拒绝、审计行整个丢失。取 kind="ok"（expression
                // 前缀 `key:revoke:` + outcome=revoked 保留全部事件信息），
                // 保证行落库。deviation 记录见交付报告。
                state.audit.record(&storage::audit::AuditEntry {
                    key_id: "-",
                    key_prefix: &rec.prefix,
                    request_id: "-",
                    expression: &format!("key:revoke:{}", rec.id),
                    kind: "ok",
                    outcome: &rec.status,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    duration_ms: 0,
                }).await;
                json_response(StatusCode::OK, key_revoked_json(&rec.id, &rec.status))
            }
            // RETURNING 0 行 = 未命中/他人 Key/已撤销 → 404（IDOR 语义）
            Ok(None) => api_error(
                StatusCode::NOT_FOUND,
                &format!("Unknown key id: {kid}"),
                "invalid_request_error",
                None,
            ),
            Err(err) => {
                worker::console_error!("key revoke failed: {err}");
                storage_error_response()
            }
        }
    })
    .await
}

/// GET /f/a/o/platform/usage：当前用户的用量汇总。
async fn platform_usage(State(state): State<AppState>, headers: HeaderMap) -> Response {
    worker::send::SendFuture::new(async {
        let s = match auth_session(&state, &headers, false).await {
            Ok(s) => s,
            Err(r) => return r,
        };
        match state.keys.list_public(&s.owner).await {
            Ok(keys) => json_response(StatusCode::OK, usage_report_json(&keys)),
            Err(err) => {
                worker::console_error!("usage report failed: {err}");
                storage_error_response()
            }
        }
    })
    .await
}

// ------------------------------------------------------------------ OAuth 回调查询参数

/// parse_qs 等价：按 '&' 分隔、'+' 视作空格、百分号解码、忽略空值。
/// 平移自 lug-server routes.rs::parse_query（契约源只读）。
pub fn parse_query(q: &str) -> Vec<(String, String)> {
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

// ------------------------------------------------------------------ GitHub OAuth 路由

/// GET /f/a/o/platform/auth/github：发起 GitHub 登录
/// （state 生成 → D1 登记 → state Cookie → 302 authorize URL）。
async fn github_login(State(state): State<AppState>, headers: HeaderMap) -> Response {
    worker::send::SendFuture::new(async {
        match state.flow.begin().await {
            Ok((st, url)) => {
                let cookie = crate::platform::cookie_header(
                    &state.auth.public_url,
                    &headers,
                    crate::platform::STATE_COOKIE,
                    &st,
                    crate::platform::STATE_TTL as i64,
                );
                crate::platform::redirect(&url, vec![cookie])
            }
            // lug-server 把 begin 的全部失败（未配置 / state 落库失败）统一
            // 重定向到同一提示——错误码不区分，防探测。
            Err(_) => {
                crate::platform::redirect("/platform?login_error=github_not_configured", vec![])
            }
        }
    })
    .await
}

/// GET /f/a/o/platform/auth/github/callback：OAuth 回调。
/// state 校验（Cookie 常数时间比较 + D1 一次性消费）→ 换取身份 →
/// 换发会话（KV put + 有界回读确认）→ 302 /platform。
/// 分支顺序与 lug-server github_callback 逐段一致。
async fn github_callback(
    State(state): State<AppState>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
    headers: HeaderMap,
) -> Response {
    worker::send::SendFuture::new(async {
        // state Cookie 清除头先行（任何失败分支都带回）
        let cleared = crate::platform::cookie_header(
            &state.auth.public_url,
            &headers,
            crate::platform::STATE_COOKIE,
            "",
            0,
        );
        let fail = |e: &oauth::OAuthError, mut cookies: Vec<String>| {
            cookies.push(cleared.clone());
            crate::platform::redirect(&format!("/platform?login_error={e}"), cookies)
        };

        if !state.flow.enabled() {
            return fail(&oauth::OAuthError::GithubNotConfigured, vec![]);
        }
        // 查询参数解析（parse_qs 语义：忽略空值；重复参数视为非法）
        let pairs = parse_query(raw.as_deref().unwrap_or(""));
        let states: Vec<&str> = pairs
            .iter()
            .filter(|(k, _)| k == "state")
            .map(|(_, v)| v.as_str())
            .collect();
        if states.len() != 1 {
            return fail(&oauth::OAuthError::InvalidOauthState, vec![]);
        }
        let browser_state = crate::platform::cookie_value(&headers, crate::platform::STATE_COOKIE)
            .unwrap_or_default();
        // 一次性消费：常数时间比较 + D1 DELETE…RETURNING（T6 flow 内部串联）
        let verifier = match state
            .flow
            .consume(states[0].to_string(), browser_state)
            .await
        {
            Ok(v) => v,
            Err(e) => return fail(&e, vec![]),
        };
        if pairs.iter().any(|(k, _)| k == "error") {
            return fail(&oauth::OAuthError::GithubCancelled, vec![]);
        }
        let codes: Vec<&str> = pairs
            .iter()
            .filter(|(k, _)| k == "code")
            .map(|(_, v)| v.as_str())
            .collect();
        if codes.len() != 1 || codes[0].len() > 1024 {
            return fail(&oauth::OAuthError::GithubUnavailable, vec![]);
        }
        let user = match state.flow.exchange(codes[0].to_string(), verifier).await {
            Ok(u) => u,
            Err(e) => return fail(&e, vec![]),
        };
        // 换发会话：旧会话立即失效（尽力而为，同 lug-server）
        if let Some(old) = crate::platform::cookie_value(&headers, crate::platform::SESSION_COOKIE)
        {
            if let Err(err) = state.sessions.revoke(&old).await {
                worker::console_error!("old session revoke failed (best-effort): {err}");
            }
        }
        // 不透明 token：43 字符 base64url（token_urlsafe(32)），csrf 同规格
        let token = oauth::util::token_urlsafe(32);
        let csrf_token = oauth::util::token_urlsafe(32);
        let user_rec = storage::sessions::SessionUser {
            provider: user.provider,
            id: user.id,
            login: user.login,
        };
        // put + 有界回读确认（登录闪断缓解，persistence.md §3.2）：
        // 写失败 → github_unavailable；回读 3 次 miss（极罕见跨 PoP 最坏情况）
        // → 放行跳转（会话已落，同 PoP 首读通常即命中）。
        if let Err(err) = state
            .sessions
            .put_confirmed(&token, &user_rec, &csrf_token, storage::audit::unix_now())
            .await
        {
            worker::console_error!("session put failed: {err}");
            return fail(&oauth::OAuthError::GithubUnavailable, vec![]);
        }
        let session_cookie = crate::platform::cookie_header(
            &state.auth.public_url,
            &headers,
            crate::platform::SESSION_COOKIE,
            &token,
            storage::sessions::SESSION_TTL as i64,
        );
        crate::platform::redirect("/platform", vec![cleared, session_cookie])
    })
    .await
}

// ------------------------------------------------------------------ 兜底

/// 全部未匹配路由：OPTIONS → 204 + CORS；其余 → OpenAI 风格 404
/// （文案列出 v1 实际可用路由——文档写对原则，平移自 lug-server）。
async fn fallback(method: axum::http::Method, req: axum::extract::Request) -> Response {
    let path = req.uri().path().to_string();
    let mut res = match method {
        axum::http::Method::OPTIONS => (StatusCode::NO_CONTENT, "").into_response(),
        axum::http::Method::POST => api_error(
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body as AxumBody;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use lug_engine::Value;
    use tower_service::Service;

    /// 表级行为的无状态测试路由：以哑 handler 占位真实 handler（行为面 =
    /// 路由表形状 + CORS 中间件 + fallback，与生产 router 相同的装配方式；
    /// 真实 Router 需要 AppState，而 D1/KV 绑定无法在 host 构造——
    /// 见 tech-notes §1 的每请求装配模型）。
    fn mini_router() -> Router {
        Router::new()
            .route("/healthz", get(healthz))
            .route("/f/a/o/calculate", post(|| async { "handler" }))
            .route("/f/a/o/platform/me", get(|| async { "handler" }))
            .fallback(fallback)
            .method_not_allowed_fallback(fallback)
            .layer(axum::middleware::from_fn(cors_middleware))
    }

    async fn send(method: &str, uri: &str) -> Response {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .body(AxumBody::empty())
            .unwrap();
        let mut svc = mini_router();
        svc.call(req).await.unwrap()
    }

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn healthz_returns_200_json_ok() {
        let res = send("GET", "/healthz").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["content-type"], "application/json");
        let v = body_json(res).await;
        assert_eq!(v["ok"], true);
        assert_eq!(v["service"], "lug-worker");
    }

    #[tokio::test]
    async fn public_api_gets_open_cors_headers() {
        // OPTIONS 打在仅 POST 的路由上 → method_not_allowed_fallback → 204 + CORS
        let res = send("OPTIONS", "/f/a/o/calculate").await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(res.headers()["Access-Control-Allow-Origin"], "*");
        assert_eq!(
            res.headers()["Access-Control-Allow-Headers"],
            "Authorization, Content-Type"
        );
        assert_eq!(
            res.headers()["Access-Control-Allow-Methods"],
            "GET, POST, OPTIONS"
        );
    }

    #[tokio::test]
    async fn platform_routes_get_no_store_no_referrer() {
        let res = send("GET", "/f/a/o/platform/me").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()["Cache-Control"], "no-store");
        assert_eq!(res.headers()["Referrer-Policy"], "no-referrer");
        assert!(res.headers().get("Access-Control-Allow-Origin").is_none());
    }

    #[tokio::test]
    async fn unknown_route_is_contract_404_with_cors() {
        let res = send("GET", "/nope").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(res.headers()["Access-Control-Allow-Origin"], "*");
        let v = body_json(res).await;
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Unknown request URL (GET /nope)"));
    }

    #[tokio::test]
    async fn unknown_post_route_lists_calculate_hint() {
        let res = send("POST", "/nope").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let v = body_json(res).await;
        assert_eq!(
            v["error"]["message"],
            "Unknown request URL (POST /nope). Only POST /f/a/o/calculate is available."
        );
    }

    // ---------------- calculate 请求解析纯面 ----------------

    fn rejection_of(body: &[u8], max_expr_len: usize) -> Response {
        match parse_calculate_request(body, max_expr_len) {
            Err(CalcRejection::Response(res)) => res,
            Err(CalcRejection::TooLong { response, .. }) => response,
            Ok(_) => panic!("expected rejection"),
        }
    }

    async fn resp_json(res: Response) -> serde_json::Value {
        let bytes = BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn calculate_rejections_match_contract_messages() {
        let v = resp_json(rejection_of(b"", 1024)).await;
        assert_eq!(v["error"]["message"], "Request body is empty.");
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert!(v["error"]["param"].is_null());
        assert!(v["error"]["code"].is_null());

        let v = resp_json(rejection_of(b"not json", 1024)).await;
        assert_eq!(v["error"]["message"], "Invalid JSON in request body.");

        let v = resp_json(rejection_of(b"[1,2]", 1024)).await;
        assert_eq!(v["error"]["message"], "Request body must be a JSON object.");

        let v = resp_json(rejection_of(b"{}", 1024)).await;
        assert_eq!(
            v["error"]["message"],
            "Input must be an expression string, e.g. {\"expression\": \"3+5*2**(1+1)\"}"
        );

        // 空白表达式同 missing
        let v = resp_json(rejection_of(br#"{"expression":"   "}"#, 1024)).await;
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Input must be an expression string"));

        // 超长 → expression_error + 确切消息
        let long = format!("{{\"expression\":\"{}\"}}", "1".repeat(1025));
        let v = resp_json(rejection_of(long.as_bytes(), 1024)).await;
        assert_eq!(
            v["error"]["message"],
            "Expression too long (max 1024 characters)"
        );
        assert_eq!(v["error"]["code"], "expression_error");

        // 1024 字符恰好放行
        let ok = format!("{{\"expression\":\"{}\"}}", "1".repeat(1024));
        assert!(parse_calculate_request(ok.as_bytes(), 1024).is_ok());
        // 按**字符**计数（多字节不劈半）
        let cjk = format!("{{\"expression\":\"{}\"}}", "键".repeat(1025));
        assert!(parse_calculate_request(cjk.as_bytes(), 1024).is_err());
        let cjk_ok = format!("{{\"expression\":\"{}\"}}", "键".repeat(1024));
        assert!(parse_calculate_request(cjk_ok.as_bytes(), 1024).is_ok());
    }

    #[tokio::test]
    async fn calculate_extracts_and_trims_expression() {
        let req = parse_calculate_request(br#"{"expression":"  1+2*3  "}"#, 1024).unwrap();
        assert_eq!(req.expression, "1+2*3");
        assert!(!req.process);
    }

    #[test]
    fn process_truthy_follows_python_semantics() {
        let t = |s: &str| process_truthy(Some(&serde_json::from_str(s).unwrap()));
        assert!(!process_truthy(None));
        assert!(!t("null"));
        assert!(t("true"));
        assert!(!t("false"));
        assert!(!t("0")); // 非零数才真
        assert!(!t("0.0"));
        assert!(t("1"));
        assert!(t("-2"));
        assert!(!t("\"\"")); // 非空串才真
        assert!(t("\"yes\""));
        assert!(t("[]")); // 其它类型（数组/对象）一律真
        assert!(t("{}"));
    }

    // ---------------- operator 表 / usage 汇总 / ok 响应 ----------------

    #[test]
    fn operator_table_shape_and_freeze_zone() {
        let v = operator_table_json();
        assert_eq!(v["object"], "operator_code_table");
        let data = v["data"].as_array().unwrap();
        assert_eq!(data.len(), lug_engine::registry::REGISTRY.len());
        // 首条与 REGISTRY[0] 对齐（0–6 冻结区）
        let first = &data[0];
        assert_eq!(first["code"], lug_engine::registry::REGISTRY[0].code);
        assert_eq!(first["symbol"], lug_engine::registry::REGISTRY[0].symbol);
        assert_eq!(first["name"], lug_engine::registry::REGISTRY[0].name);
        // aliases：WORD_ALIASES 中指向该符号的全部别名，sorted（Python sorted 序）
        let mut expected: Vec<&str> = lug_engine::registry::WORD_ALIASES
            .iter()
            .filter(|(_, s)| *s == lug_engine::registry::REGISTRY[0].symbol)
            .map(|(a, _)| *a)
            .collect();
        expected.sort_unstable();
        assert_eq!(first["aliases"], json!(expected));
        // 字段序（preserve_order 下 = 输出序）
        let keys: Vec<String> = first.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, vec!["code", "symbol", "name", "aliases"]);
    }

    fn keypub(id: &str, reqs: u64, p: u64, c: u64, t: u64) -> storage::keystore::KeyPublic {
        storage::keystore::KeyPublic {
            id: id.into(),
            name: "default".into(),
            prefix: "sk-x...y".into(),
            created_at: 1_700_000_000,
            status: "active".into(),
            last_used: None,
            usage_requests: reqs,
            usage_prompt_tokens: p,
            usage_completion_tokens: c,
            usage_total_tokens: t,
        }
    }

    #[test]
    fn usage_report_folds_totals_over_keys() {
        let keys = vec![keypub("a", 1, 10, 20, 30), keypub("b", 2, 100, 200, 300)];
        let v = usage_report_json(&keys);
        assert_eq!(v["object"], "usage_report");
        assert_eq!(v["total"]["requests"], 3);
        assert_eq!(v["total"]["prompt_tokens"], 110);
        assert_eq!(v["total"]["completion_tokens"], 220);
        assert_eq!(v["total"]["total_tokens"], 330);
        assert_eq!(v["keys"].as_array().unwrap().len(), 2);
        // 字段序
        let top: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(top, vec!["object", "total", "keys"]);
        // 空 keys → 全零 total
        let v = usage_report_json(&[]);
        assert_eq!(
            v["total"],
            json!({"requests": 0, "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0})
        );
    }

    #[test]
    fn calculate_ok_json_field_order_and_shape() {
        let output = lug_engine::evaluate("1+2").unwrap();
        let v = calculate_ok_json("abc123", 1_700_000_000, "1+2", &output);
        let keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                "id",
                "object",
                "created",
                "model",
                "expression",
                "result",
                "usage"
            ]
        );
        assert_eq!(v["id"], "eval-abc123");
        assert_eq!(v["object"], "evaluation");
        assert_eq!(v["model"], "calculator-1");
        assert_eq!(v["result"], 3);
        assert_eq!(v["usage"]["total_tokens"], 5);
        assert_eq!(
            v["usage"]["completion_tokens_details"]["reasoning_tokens"],
            1
        );
    }

    #[test]
    fn engine_error_response_carries_code() {
        let err = lug_engine::evaluate("1+").unwrap_err();
        let res = engine_error_response(&err);
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let err2 = lug_engine::evaluate("1/0").unwrap_err();
        assert_eq!(err2.code, lug_engine::ExprErrorCode::ComputationError);
    }

    #[test]
    fn num_to_json_triad_and_display_semantics() {
        assert_eq!(num_to_json(&Value::Int(-4)), json!(-4));
        let out = lug_engine::evaluate("2**100").unwrap();
        assert_eq!(
            num_to_json(&out.result),
            json!(1267650600228229401496703205376u128)
        );
        // 整值浮点整数化、0.0 → 0
        assert_eq!(num_to_json(&Value::Float(0.0)), json!(0));
        assert_eq!(num_to_json(&Value::Float(-2.5)), json!(-2.5));
    }

    #[test]
    fn bearer_key_extraction_matches_legacy() {
        let hm = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert("Authorization", HeaderValue::from_str(v).unwrap());
            h
        };
        assert_eq!(bearer_key(&hm("Bearer sk-abc")), "sk-abc");
        assert_eq!(bearer_key(&hm("Bearer   sk-abc  ")), "sk-abc"); // trim
        assert_eq!(bearer_key(&hm("bearer sk-abc")), ""); // 大小写敏感
        assert_eq!(bearer_key(&hm("Basic xyz")), "");
        let empty = HeaderMap::new();
        assert_eq!(bearer_key(&empty), "");
    }

    // ---------------- platform 响应纯面 ----------------

    #[test]
    fn platform_response_builders_field_order() {
        // auth status
        let v = auth_status_json(false);
        assert_eq!(v, json!({"github_enabled": false}));
        // keys list
        let v = keys_list_json(&[keypub("a", 1, 10, 20, 30)]);
        assert_eq!(v["object"], "list");
        assert_eq!(v["data"].as_array().unwrap().len(), 1);
        assert_eq!(v["data"][0]["id"], "a");
        assert!(v["data"][0].get("key_hash").is_none());
        assert!(v["data"][0].get("owner_id").is_none());
        // key created（字段序 = lug-server platform_keys_create）
        let rec = storage::keystore::KeyRec {
            id: "a1b2c3".into(),
            owner_id: "github:1".into(),
            name: "default".into(),
            key_hash: "h".into(),
            prefix: "sk-x...y".into(),
            created_at: 1_700_000_000,
            status: "active".into(),
            last_used: None,
            usage_requests: 0,
            usage_prompt_tokens: 0,
            usage_completion_tokens: 0,
            usage_total_tokens: 0,
        };
        let v = key_created_json(&rec, "sk-plaintext");
        let keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys,
            vec!["id", "name", "key", "prefix", "created_at", "note"]
        );
        assert_eq!(v["key"], "sk-plaintext");
        assert_eq!(v["note"], "Save this key now; it will not be shown again.");
        // key revoked
        assert_eq!(
            key_revoked_json("a1b2c3", "revoked"),
            json!({"id": "a1b2c3", "status": "revoked"})
        );
    }

    // ---------------- OAuth 回调查询参数（ lug-server parse_query 平移） ----------------

    #[test]
    fn parse_query_matches_parse_qs_semantics() {
        assert_eq!(
            parse_query("state=abc&code=xyz"),
            vec![
                ("state".to_string(), "abc".to_string()),
                ("code".to_string(), "xyz".to_string())
            ]
        );
        // 空值忽略（keep_blank_values=False）
        assert_eq!(
            parse_query("state=&code=xyz"),
            vec![("code".to_string(), "xyz".to_string())]
        );
        assert_eq!(parse_query(""), Vec::<(String, String)>::new());
        // 无 '=' 的裸键：值视为空 → 忽略
        assert_eq!(parse_query("state"), Vec::<(String, String)>::new());
        // '+' 视作空格、百分号解码
        assert_eq!(
            parse_query("a=a+b%21&code=c%2Fd"),
            vec![
                ("a".to_string(), "a b!".to_string()),
                ("code".to_string(), "c/d".to_string())
            ]
        );
        // 不完整 % 序列原样保留（与 lug-server percent_decode 一致）
        assert_eq!(
            parse_query("a=%zz"),
            vec![("a".to_string(), "%zz".to_string())]
        );
        // 重复参数保留两条（回调侧按 count 判非法）
        assert_eq!(
            parse_query("state=1&state=2"),
            vec![
                ("state".to_string(), "1".to_string()),
                ("state".to_string(), "2".to_string())
            ]
        );
    }
}
