//! panic 防护工具（tech-notes 附-4：wasm32 默认 panic=abort 会杀死 isolate）。
//!
//! - stable wasm32：panic=abort 下 `catch_unwind` 捕不到（调用无害无效果）；
//!   切 nightly + `worker-build --panic-unwind`（重编 std，panic=unwind）后
//!   真正生效——取舍记录见 wrangler.toml `[build]` 注释。
//! - lug-engine 处理任意用户输入，T7 起所有引擎调用统一经
//!   [`catch_unwind_async`] 包装（workers-rs README "Panic Recovery"）。

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;

/// 包装任意 future：panic → `Err(payload)`，正常完成 → `Ok(output)`。
///
/// axum `Router`/`Request` 等类型不满足 `UnwindSafe`，`AssertUnwindSafe`
/// 语义在此函数内化，调用方无需重复包裹。
pub async fn catch_unwind_async<F: Future>(
    fut: F,
) -> Result<F::Output, Box<dyn Any + Send>> {
    AssertUnwindSafe(fut).catch_unwind().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passthrough_returns_ok() {
        let r = catch_unwind_async(async { 41 + 1 }).await;
        assert_eq!(r.unwrap(), 42);
    }

    #[tokio::test]
    async fn panicking_future_is_caught_as_err() {
        let r: Result<(), Box<dyn Any + Send>> =
            catch_unwind_async(async { panic!("boom") }).await;
        let err = r.unwrap_err();
        let msg = err
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| err.downcast_ref::<String>().map(String::as_str));
        assert_eq!(msg, Some("boom"));
    }
}
