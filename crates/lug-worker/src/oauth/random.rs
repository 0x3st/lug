//! CSPRNG 辅助：Workers 平台的熵源是内建 Web Crypto（`crypto.getRandomValues`）。
//!
//! 为何不用 rand/getrandom（skeleton 分支 6c77725 已撤销，见 crate Cargo.toml 注释）：
//! - `wasm32-unknown-unknown` 没有 OS 熵源，getrandom 在该 target 下没有默认
//!   后端（需手工开 `wasm_js` feature 并注入 JS 熵初始化），rand 的
//!   `thread_rng()` 依赖 getrandom，随整个 rand 栈一起不可用；
//! - Workers 运行时的正确熵源是全局 Web Crypto：CSPRNG 由平台保证，
//!   经 web-sys 的 `Crypto::get_random_values_with_u8_array` 直接写入
//!   wasm 线性内存中的 `&mut [u8]` —— 即 `rand::RngCore::fill_bytes` 等价物。
//!
//! 非 wasm 目标（host 单测）上 wasm-bindgen 的导入桩在运行时 panic，
//! 单测不触达本模块（纯函数测试在 `super::util`）。

/// Web Crypto 规范：单次 getRandomValues 至多 65536 字节，超过按块调用。
const GET_RANDOM_VALUES_MAX: usize = 65536;

/// 用平台 CSPRNG 填充 `dest`（`rand::RngCore::fill_bytes` 等价物）。
///
/// 仅限 wasm32 运行时调用（Workers / 浏览器）。
pub fn fill_bytes(dest: &mut [u8]) {
    if dest.is_empty() {
        return;
    }
    let crypto = global_crypto();
    for chunk in dest.chunks_mut(GET_RANDOM_VALUES_MAX) {
        // `&mut [u8]` 即 wasm 线性内存切片，JS 侧原地写入，调用返回后即生效。
        crypto
            .get_random_values_with_u8_array(chunk)
            .expect("crypto.getRandomValues failed");
    }
}

/// `n` 字节随机数。
pub fn token_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    fill_bytes(&mut buf);
    buf
}

/// Workers / 浏览器通用的全局 `crypto` 对象。
///
/// `js_sys::global()` 返回 globalThis：Workers 下是 WorkerGlobalScope，
/// 浏览器下是 Window，两者都暴露 `crypto`。用 `unchecked_into` 绕过原型链
/// 检查（workerd 全局对象的类型标记不完整）；属性缺失时由 `crypto()` 的
/// catch 绑定返回 Err，经 expect 显式失败。
fn global_crypto() -> web_sys::Crypto {
    use worker::wasm_bindgen::JsCast;
    let global: web_sys::WorkerGlobalScope = worker::js_sys::global().unchecked_into();
    global
        .crypto()
        .expect("globalThis.crypto is unavailable (CSPRNG required)")
}
