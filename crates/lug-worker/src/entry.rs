//! wasm32-only 胶水层：`#[event(fetch)]` 入口、panic 防护接线。
//!
//! 本模块只在 `target_arch = "wasm32"` 下编译（lib.rs cfg 分层）；
//! 纯逻辑一律放公共模块（routes/sse/panic_guard）。T4 骨架起 SSE 响应
//! 构造在下移后的 [`crate::sse`]（`Body::from_stream` 在 worker 0.9 下
//! host 亦可编译，见 crate Cargo.toml 注释）。
//!
//! T7.5 接线将在此追加 Env 绑定装配（AppState::from_env）。

use crate::{panic_guard, routes};
use tower_service::Service;
use worker::*;

/// panic 防护：wasm32 stable 默认 panic=abort 会杀 isolate；catch_unwind
/// 结构先就位（stable 下对 abort 无害无效果），切换 nightly +
/// `worker-build --panic-unwind`（见 wrangler.toml 注释）后自动生效。
async fn guarded_call(req: HttpRequest) -> axum::response::Response {
    // Router::call 的 Output 是 Result<Response, Infallible>（tower Service），
    // catch_unwind 后需剥两层：外层 panic、内层 Infallible。
    let mut svc = routes::router();
    match panic_guard::catch_unwind_async(svc.call(req)).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(never)) => match never {},
        Err(_) => routes::api_error(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "internal panic during request handling",
            "internal_error",
            None,
        ),
    }
}

/// fetch 入口（官方 axum 模板写法，tech-notes §1）：
/// `http::Request<worker::Body>` 进 axum `Router::call`，
/// `http::Response` 直接回给 runtime。
#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    _env: Env,
    _ctx: Context,
) -> Result<axum::http::Response<axum::body::Body>> {
    Ok(guarded_call(req).await)
}
