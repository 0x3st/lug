//! wasm32-only 胶水层：`#[event(fetch)]` 入口、panic 防护接线、
//! SSE 流式响应（`worker::Body::from_stream`，tech-notes §2 路线 B）。
//!
//! 本模块只在 `target_arch = "wasm32"` 下编译（lib.rs cfg 分层）；
//! 纯逻辑一律放公共模块（routes/sse/panic_guard）。

use crate::{panic_guard, routes};
use axum::http::StatusCode;
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
            StatusCode::INTERNAL_SERVER_ERROR,
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

/// SSE 回放响应（路线 B）：预生成帧 → ReadableStream。
/// 头部自设：from_stream 不自动补 Content-Type。
pub fn sse_response(frames: Vec<String>) -> http::Response<worker::Body> {
    let stream = futures::stream::iter(
        frames
            .into_iter()
            .map(|frame| Ok::<_, std::convert::Infallible>(frame.into_bytes())),
    );
    let body = worker::Body::from_stream(stream).expect("infallible stream conversion");
    http::Response::builder()
        .status(200)
        .header("Content-Type", "text/event-stream")
        .header("Cache-Control", "no-store")
        .body(body)
        .expect("static SSE response parts are valid")
}

/// axum handler 直用版：`http::Response<worker::Body>` → axum Response
/// （保状态码/头，body 经 `axum::body::Body::new` 适配 http_body trait）。
/// T7 的 calculate handler `process=true` 分支返回此类型。
pub fn sse_axum_response(frames: Vec<String>) -> axum::response::Response {
    sse_response(frames).map(axum::body::Body::new)
}
