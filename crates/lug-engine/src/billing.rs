//! 计费口径（§3.6 计费模型：**一个计算步骤 = 1 token**，差异表 D2）。
//!
//! - `prompt_tokens` = 表达式词法 token 数（与原型一致）；
//! - `reasoning_tokens` = 步骤数（每个 emit 的 step 恰好 1 token；
//!   一元符号/括号不产生步骤、不计费）；
//! - `completion_tokens` = reasoning + 1（最终答案 1 token）；
//! - 三元同一性：计费 token 数 == SSE step 事件数 == 用户可见步骤数。

use crate::lexer;
use crate::steps::Step;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
    pub total_tokens: usize,
    pub reasoning_tokens: usize,
}

pub fn build_usage(expr: &str, steps: &[Step]) -> Usage {
    let prompt = count_tokens(expr);
    let reasoning = steps.len();
    Usage {
        prompt_tokens: prompt,
        completion_tokens: reasoning + 1,
        total_tokens: prompt + reasoning + 1,
        reasoning_tokens: reasoning,
    }
}

/// 表达式词法 token 数（解析失败返回 0，与原型 count_tokens 一致）。
pub fn count_tokens(expr: &str) -> usize {
    lexer::tokenize(expr).map(|t| t.len()).unwrap_or(0)
}
