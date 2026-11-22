//! Lug CF Workers 入口（workers-rs 0.9 + axum 0.8 `default-features = false`）。
//!
//! 分层（验收：wasm32 check 与 host 单测都要过，而 worker/worker-macros
//! 是 wasm-only crate）：
//! - 公共纯逻辑模块——路由表 [`routes`]、SSE 帧序列化 [`sse`]、panic 防护
//!   [`panic_guard`]——不依赖 `worker`，host 可编译可测；
//! - `#[cfg(target_arch = "wasm32")]` 的 [`entry`] 放 `#[event(fetch)]`
//!   入口、Env 绑定胶水与 `worker::Body::from_stream` SSE 胶水。
//!
//! 依据：lug-migration/notes/workers-rs-tech-notes.md §1/§2。

pub mod panic_guard;
pub mod routes;
pub mod sse;

#[cfg(target_arch = "wasm32")]
pub mod entry;
