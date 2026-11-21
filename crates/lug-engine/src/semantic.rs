//! 语义规则层（L1，MoO 第二层增强）：中文数字解析 + 算子词别名接词法。
//!
//! 设计边界（§10.4）：**规则先行，模型兜底**——本模块全部为确定性规则，
//! 零模型零延迟；神经解析（Module A）只处理规则失败后的模糊尾巴。
//!
//! 中文数字文法（标准口语口径）：
//! ```text
//! 数字 := 零〇一二两三四五六七八九（连续数字串接：一二三 = 123）
//! 小节 := [数字]百 [数字]十 [数字]（≤9999；"十五"起头补一）
//! 大节 := [小节] 万/亿 （可链式："二亿三千万" = 2.3e8）
//! 小数 := 整数部分 点 数字+
//! 续接 ：ASCII 整数后紧贴 万/亿/千（"3万5千" = 35000）
//! ```
//! 例子：`三十五` 35 · `一百零五` 105 · `三点五` 3.5 · `十万` 100000 ·
//! `二亿三千万` 230000000 · `3万5千` 35000 · `负一百`（一元负号组合）−100。
//!
//! 算子别名（registry::WORD_ALIASES）按**最长匹配**接词法：
//! `3 加 5` `2乘以3` `10整除3` `7 取余 2`；`abc` 等非别名字母串仍走
//! 原型的 "Unrecognized character" 错误路径（差分兼容）。

use crate::value::Value;
use num_bigint::BigInt;
use num_traits::ToPrimitive;

/// 中文数字字符 → 数值。
fn cn_digit(c: char) -> Option<i128> {
    Some(match c {
        '零' | '〇' => 0,
        '一' => 1,
        '二' => 2,
        '两' => 2,
        '三' => 3,
        '四' => 4,
        '五' => 5,
        '六' => 6,
        '七' => 7,
        '八' => 8,
        '九' => 9,
        // 混合形态："3万5千" 中间的 ASCII 数字
        '0'..='9' => (c as u8 - b'0') as i128,
        _ => return None,
    })
}

fn cn_small_unit(c: char) -> Option<i128> {
    Some(match c {
        '十' => 10,
        '百' => 100,
        '千' => 1000,
        _ => return None,
    })
}

fn cn_big_unit(c: char) -> Option<i128> {
    Some(match c {
        '万' => 10_000,
        '亿' => 100_000_000,
        _ => return None,
    })
}

/// 解析中文数字短语（可带小数）。
///
/// * `seed`：续接场景下前置 ASCII 整数值（如 "3万" 中的 3），作为待挂的数字；
///   普通场景传 `None`。
/// * 返回 `(值, 消耗字符数)`；不是合法中文数字时返回 `None`（调用方回退）。
///
/// 算法（标准口语口径）：`result`（已完成大节）+ `current`（当前小节 ≤9999）
/// + `number`（最近读到的数字）；万/亿 把小节并入大节；十 起头补一。
pub fn parse_cn_number(chars: &[char], start: usize, seed: Option<i128>) -> Option<(Value, usize)> {
    let mut i = start;
    let mut result: i128 = 0;
    let mut current: i128 = 0;
    let mut number: i128 = seed.unwrap_or(0);
    let mut has_pending = seed.is_some();
    let mut seen_any = has_pending;

    while i < chars.len() {
        let c = chars[i];
        if let Some(d) = cn_digit(c) {
            number = number * 10 + d;
            seen_any = true;
            i += 1;
        } else if let Some(u) = cn_small_unit(c) {
            // "十五" 起头的十补一；其余单位前必须有数字
            let n = if number == 0 && c == '十' && !has_pending && current == 0 && result == 0 {
                1
            } else {
                number
            };
            if n == 0 && c != '十' {
                // "零百" 之类非法序列
                return None;
            }
            current += n * u;
            number = 0;
            seen_any = true;
            i += 1;
        } else if let Some(u) = cn_big_unit(c) {
            if current == 0 && number == 0 && result == 0 {
                return None; // 孤立的 万/亿 不是合法数字
            }
            result += (current + number) * u;
            current = 0;
            number = 0;
            has_pending = false;
            seen_any = true;
            i += 1;
        } else if c == '点' && seen_any {
            // 小数：点后至少一位数字，否则不消费（点 落入未识别路径）
            let frac_start = i + 1;
            let mut j = frac_start;
            let mut frac: f64 = 0.0;
            let mut scale: f64 = 1.0;
            while j < chars.len() {
                match cn_digit(chars[j]) {
                    Some(d) => {
                        frac = frac * 10.0 + d as f64;
                        scale *= 10.0;
                        j += 1;
                    }
                    None => break,
                }
            }
            if j == frac_start {
                break; // 点后无数字：整数部分到此为止，交由上层处理 点
            }
            let int_part = result + current + number;
            let value = int_part as f64 + frac / scale;
            return Some((Value::Float(value), j - start));
        } else {
            break;
        }
    }

    if !seen_any {
        return None;
    }
    let total = result + current + number;
    let value = if total.unsigned_abs() <= i64::MAX as u128 {
        Value::Int(total as i64)
    } else {
        Value::Big(BigInt::from(total))
    };
    Some((value, i - start))
}

/// 数字续接解析（"3万5千" / "2亿3千万"）：seed = 紧贴前方的整数值。
pub fn continue_cn_number(chars: &[char], prev: &Value) -> Option<(Value, usize)> {
    let seed = match prev {
        Value::Int(v) => *v as i128,
        Value::Big(b) => b.to_i128()?, // 超出 i128 的巨型数字不参与续接
        Value::Float(_) => return None,
    };
    parse_cn_number(chars, 0, Some(seed))
}

/// 最长匹配算子词别名（registry::WORD_ALIASES），返回 (规范符号, 消耗字符数)。
pub fn match_word_alias(chars: &[char], start: usize) -> Option<(&'static str, usize)> {
    let mut best: Option<(&'static str, usize)> = None;
    for (word, symbol) in crate::registry::WORD_ALIASES {
        let wc: Vec<char> = word.chars().collect();
        if chars[start..].starts_with(&wc[..]) && best.is_none_or(|(_, len)| wc.len() > len) {
            best = Some((symbol, wc.len()));
        }
    }
    best
}
