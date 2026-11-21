//! Lug 结构化计算引擎（v1）。
//!
//! 流水线（§3）：表达式 ─tokenize→ tokens ─parse→ AST ─ast_to_prefix→ 先序 IR，
//! 双路求值：AST 后序遍历（步骤流）与先序栈式求值交叉验证（§3.5）。
//!
//! 本 crate 为**纯计算库**：零 I/O、零 tokio、零 serde（§2 分层规则）。
//! 序列化由 lug-server 完成；`Value`/`Formatted` 提供与 Python 原型一致的
//! 显示与 JSON 数字语义（`format_num`、py repr、Big 任意精度）。
//!
//! 语义基线：与 Python 原型（`legacy/structured_ai_api.py`）逐字节对齐，
//! 差异仅限设计文档 §5 差异表（D1 x 别名退役 / D2 步骤计费 / D3 非有限报错 /
//! D4 运算预算）。

pub mod ast;
pub mod billing;
pub mod error;
pub mod eval;
pub mod lexer;
pub mod parser;
pub mod prefix;
pub mod registry;
pub mod semantic;
pub mod steps;
pub mod value;

pub use billing::Usage;
pub use error::{ExprError, ExprErrorCode};
pub use lexer::Token;
pub use prefix::PrefixIr;
pub use steps::Step;
pub use value::{Ctx, Formatted, Value};

/// 完整求值输出。
#[derive(Clone, Debug)]
pub struct EvalOutput {
    /// 最终结果（原始值；显示/序列化时经 `format_num`）。
    pub result: Value,
    /// 逐步化简记录（SSE step 事件来源）。
    pub steps: Vec<Step>,
    /// 可读波兰式（SSE prefix 事件）。
    pub prefix_display: String,
    /// 计费用量（D2：1 步 = 1 token）。
    pub usage: Usage,
}

/// 完整流水线（默认预算 [`value::DEFAULT_MAX_DIGITS`]）。
pub fn evaluate(expr: &str) -> Result<EvalOutput, ExprError> {
    evaluate_with(expr, &Ctx::default())
}

/// 完整流水线（自定义预算）：解析 → 先序 IR → AST 逐步求值 → 先序栈式验算
/// （双路一致性校验，差异即内部错误）。
pub fn evaluate_with(expr: &str, ctx: &Ctx) -> Result<EvalOutput, ExprError> {
    let tokens = lexer::tokenize(expr)?;
    let root = parser::parse_tokens(&tokens)?;
    let PrefixIr {
        tokens: prefix_tokens,
        display,
    } = prefix::ast_to_prefix(&root);
    let tree = steps::evaluate_tree(&root, &tokens, ctx)?;
    // 用先序 IR 再算一遍，交叉验证（先 AST 后 prefix，顺序与原型一致）
    let check = prefix::evaluate_prefix(&prefix_tokens, ctx, &[])?;
    let f_res = value::format_num_value(&tree.result);
    let f_chk = value::format_num_value(&check);
    if !f_res.num_eq(&f_chk) {
        return Err(ExprError::computation(format!(
            "Internal consistency check failed: AST={}, prefix={}",
            f_res.to_display(),
            f_chk.to_display()
        )));
    }
    let usage = billing::build_usage(expr, &tree.steps);
    Ok(EvalOutput {
        result: tree.result,
        steps: tree.steps,
        prefix_display: display,
        usage,
    })
}
