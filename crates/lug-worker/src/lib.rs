//! Lug CF Workers 入口。
//!
//! 架构：`#[event(fetch)]` 收标准 `http::Request` → axum `Router::call` →
//! `http::Response` 直接回给 runtime（workers-rs http feature）。
//! axum 必须 `default-features = false`（Workers 无 tokio reactor，
//! 见 lug-migration/notes/workers-rs-tech-notes.md §1/§2）。
//!
//! T4 骨架：/healthz 验活 + 其余路由 501 stub（T7 接业务逻辑）。
//! 持久化（D1/KV）由 T5 接入，OAuth 由 T6 接入——均经 fetch 的 `env` 取绑定。

pub mod sse;
pub mod storage;

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::FutureExt;
use serde_json::json;
use tower_service::Service;
use worker::*;

// ------------------------------------------------------------------ CORS

/// CORS/安全头公共实现（平移自 lug-server/routes.rs，语义逐行一致）：
/// 平台路由收紧（no-store / no-referrer），计算 API 公开 CORS。
fn apply_cors(path: &str, headers: &mut HeaderMap) {
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

/// 注意：axum 0.8 的 `method_not_allowed_fallback` 不经过 `Router::layer`，
/// fallback 响应需自行调用 [`apply_cors`]（与 lug-server 同坑同解）。
async fn cors_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;
    apply_cors(&path, res.headers_mut());
    res
}

// ------------------------------------------------------------------ 错误结构

/// OpenAI 风格错误结构（`{"error": {message, type, param, code}}`），
/// 与 lug-server/Python 原型逐字节契约一致。
fn api_error(status: StatusCode, message: &str, err_type: &str, code: Option<&str>) -> Response {
    let body = json!({
        "error": {"message": message, "type": err_type, "param": null, "code": code}
    });
    (status, Json(body)).into_response()
}

// ------------------------------------------------------------------ handlers

async fn healthz() -> &'static str {
    "ok"
}

/// T7 集成前的占位：路由表已就位，业务逻辑待迁移。
async fn stub() -> Response {
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        "handler pending migration (T7 integration)",
        "internal_error",
        None,
    )
}

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
    apply_cors(&path, res.headers_mut());
    res
}

// ------------------------------------------------------------------ router

fn router() -> Router {
    Router::new()
        // 验活
        .route("/healthz", get(healthz))
        // 计算 API
        .route("/f/a/o/operator", get(stub))
        .route("/f/a/o/calculate", post(stub))
        // 控制台（GitHub OAuth + Key 管理）
        .route("/f/a/o/platform/auth", get(stub))
        .route("/f/a/o/platform/me", get(stub))
        .route("/f/a/o/platform/usage", get(stub))
        .route("/f/a/o/platform/auth/github", get(stub))
        .route("/f/a/o/platform/auth/github/callback", get(stub))
        .route("/f/a/o/platform/logout", post(stub))
        .route("/f/a/o/platform/keys", get(stub).post(stub))
        .route("/f/a/o/platform/keys/{id}", axum::routing::delete(stub))
        // 静态页面（/ /docs /platform /assets/*）由 Workers Static Assets
        // 承载（run_worker_first 只拦 /f/a/o/*），axum 不再重复路由。
        .fallback(fallback)
        .method_not_allowed_fallback(fallback)
        .layer(axum::middleware::from_fn(cors_middleware))
}

// ------------------------------------------------------------------ fetch 入口

/// panic 防护：wasm32 默认 panic=abort 会杀 isolate；catch_unwind 结构
/// 先就位（stable 下对 abort 无害无效果），切换 nightly +
/// `worker-build --panic-unwind`（见 wrangler.toml 注释）后自动生效。
async fn guarded_call(req: HttpRequest) -> Response {
    // Router::call 的 Output 是 Result<Response, Infallible>（tower Service），
    // catch_unwind 后需剥两层：外层 panic、内层 Infallible。
    match std::panic::AssertUnwindSafe(router().call(req)).catch_unwind().await {
        Ok(Ok(resp)) => resp,
        Ok(Err(never)) => match never {},
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal panic during request handling",
            "internal_error",
            None,
        ),
    }
}

#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    _env: Env,
    _ctx: Context,
) -> Result<axum::http::Response<axum::body::Body>> {
    Ok(guarded_call(req).await)
}
