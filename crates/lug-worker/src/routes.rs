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

use crate::oauth::{self, WorkerAuthConfig, WorkerGithubFlow};
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
        // 静态页面：C8 改由 Workers Static Assets 承载（wrangler.toml [assets]，
        // run_worker_first 只拦 /f/a/o/*），届时此处整体删除。
        .route("/", get(stub))
        .route("/index.html", get(stub))
        .route("/docs", get(stub))
        .route("/platform", get(stub))
        .route("/assets/{name}", get(stub))
        // 计算 API
        .route("/f/a/o/operator", get(operator_table))
        .route("/f/a/o/calculate", post(calculate))
        // 控制台（GitHub OAuth + Key 管理）
        .route("/f/a/o/platform/auth", get(stub))
        .route("/f/a/o/platform/me", get(stub))
        .route("/f/a/o/platform/usage", get(stub))
        .route("/f/a/o/platform/auth/github", get(stub))
        .route("/f/a/o/platform/auth/github/callback", get(stub))
        .route("/f/a/o/platform/logout", post(stub))
        .route("/f/a/o/platform/keys", get(stub).post(stub))
        .route("/f/a/o/platform/keys/{id}", axum::routing::delete(stub))
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
pub const UNAUTHORIZED_MESSAGE: &str = "Missing or invalid API key. Create one in the platform console (GET /platform) and pass it in the 'Authorization: Bearer ***' header.";

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

// ------------------------------------------------------------------ handlers

/// 验活：200 + JSON 状态体（自定结构，供部署探针/冒烟用）。
async fn healthz() -> Response {
    json_response(
        StatusCode::OK,
        json!({"ok": true, "service": "lug-worker", "phase": "wired"}),
    )
}

/// 未接线路由占位（platform API / OAuth 在 C6/C7 替换；静态路由 C8 删除）。
async fn stub(req: axum::extract::Request) -> Response {
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        &format!(
            "{} is registered but its handler has not been migrated to Workers yet (T4 skeleton).",
            req.uri().path()
        ),
        "not_implemented_error",
        None,
    )
}

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
                });
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
                });
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
                });
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
        });

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
}
