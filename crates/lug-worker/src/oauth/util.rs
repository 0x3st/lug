//! OAuth 纯函数：base64url / urlencode / 常数时间比较 / 随机 token。
//!
//! 逐行平移自 lug-server/src/platform.rs（只读源，语义不变）：
//! - [`base64url`] / [`urlencode`] / [`const_eq`] 与源逐字节一致；
//! - [`token_urlsafe`] 的随机源由 `rand::rng().fill_bytes` 换成
//!   [`crate::oauth::random::fill_bytes`]（wasm32 无默认 rand 后端，
//!   见 oauth/random.rs 的说明）。

use crate::oauth::random;

/// base64url（无 padding），等价 Python `token_urlsafe`。
/// 平移自 platform.rs::base64url（逐行一致）。
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHA[(n >> 18) as usize & 63] as char);
        out.push(ALPHA[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHA[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHA[n as usize & 63] as char);
        }
    }
    out
}

/// application/x-www-form-urlencoded（unreserved: A-Z a-z 0-9 - _ . ~）。
/// 平移自 platform.rs::urlencode（逐行一致）。
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 常数时间比较（等价 `hmac.compare_digest`）。
/// 平移自 platform.rs::const_eq（逐行一致）。
pub fn const_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// base64url 随机 token（等价 Python `token_urlsafe`）。
/// platform.rs 版本用 rand——wasm32 不可用，随机源改为平台 Web Crypto；
/// 仅限 wasm32 运行时调用（host 单测不触达，FFI 桩会 panic）。
pub fn token_urlsafe(nbytes: usize) -> String {
    base64url(&random::token_bytes(nbytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_matches_rfc4648_vectors_without_padding() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64url_uses_url_safe_alphabet() {
        // 0xff*3 的 6-bit 组全为 63，标准 base64 是 "////"，url-safe 映射为 "____"。
        assert_eq!(base64url(&[0xff, 0xff, 0xff]), "____");
        // 0xfb,0xef → 组 62,62,60 → "--8"（62='-'，60='8'；仅 2 字节无尾字符）。
        assert_eq!(base64url(&[0xfb, 0xef]), "--8");
        assert!(!base64url(&[0xff; 32]).contains('+'));
        assert!(!base64url(&[0xff; 32]).contains('/'));
        // 无 padding：32 字节 → 43 字符，48 字节 → 64 字符。
        assert_eq!(base64url(&[0u8; 32]).len(), 43);
        assert_eq!(base64url(&[0u8; 48]).len(), 64);
        assert!(!base64url(&[0u8; 32]).contains('='));
    }

    #[test]
    fn urlencode_keeps_unreserved_and_escapes_rest() {
        assert_eq!(urlencode(""), "");
        assert_eq!(urlencode("abcXYZ019-_.~"), "abcXYZ019-_.~");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("s+e"), "s%2Be");
        assert_eq!(urlencode("c/ode"), "c%2Fode");
        assert_eq!(urlencode("héllo"), "h%C3%A9llo"); // UTF-8 多字节逐字节转义
    }

    #[test]
    fn const_eq_matches_compare_digest_semantics() {
        assert!(const_eq("", ""));
        assert!(const_eq("lug_oauth_state", "lug_oauth_state"));
        assert!(!const_eq("a", "b"));
        assert!(!const_eq("a", "ab")); // 长度不同直接 false
        assert!(!const_eq("ab", "a"));
        assert!(!const_eq("A", "a")); // 大小写敏感
    }

    // token_urlsafe 不做 host 单测：随机源是 wasm FFI（非 wasm 桩 panic），
    // 其编码路径已由 base64url 向量覆盖，随机性由 Workers 平台保证。
}
