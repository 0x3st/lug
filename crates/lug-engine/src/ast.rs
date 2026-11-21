//! AST 节点定义。span 为 token 下标区间 `[start, end)`，
//! 步骤化简时用 `substitute(span, value)` 把子树替换为数值 token。

use crate::lexer::Token;
use crate::value::{BinOp, Value};

pub type Span = (usize, usize);

#[derive(Clone, Debug)]
pub enum Node {
    /// 数字字面量（含词法期折叠的常量 pi/e/tau）。
    Num { value: Value, span: Span },
    /// 绑定变量（仅 sum/prod 体内合法；自由变量在求值期报错）。
    Var { name: String, span: Span },
    /// 一元 +/−（sign: 1 / -1）。不产生步骤、不计费（与原型一致）。
    Unary {
        sign: i32,
        child: Box<Node>,
        span: Span,
    },
    /// 后缀一元（`!` 阶乘）。产生一步（Always 计步）。
    Postfix {
        symbol: &'static str,
        operand: Box<Node>,
        span: Span,
    },
    Bin {
        op: BinOp,
        symbol: &'static str,
        left: Box<Node>,
        right: Box<Node>,
        span: Span,
    },
    /// 函数调用 / 聚合特殊形式。
    /// - 普通函数：`var = None`，args 为实参表达式；
    /// - sum/prod：`var = Some(绑定变量名)`，args = [from, to, body]。
    Call {
        func: &'static str,
        var: Option<String>,
        args: Vec<Node>,
        span: Span,
    },
}

impl Node {
    pub fn span(&self) -> Span {
        match self {
            Node::Num { span, .. }
            | Node::Var { span, .. }
            | Node::Unary { span, .. }
            | Node::Postfix { span, .. }
            | Node::Bin { span, .. }
            | Node::Call { span, .. } => *span,
        }
    }

    pub fn set_span(&mut self, span: Span) {
        match self {
            Node::Num { span: s, .. }
            | Node::Var { span: s, .. }
            | Node::Unary { span: s, .. }
            | Node::Postfix { span: s, .. }
            | Node::Bin { span: s, .. }
            | Node::Call { span: s, .. } => *s = span,
        }
    }
}

/// 求值工作区条目：求值过程中子树被替换为数值 token，其余原样保留。
/// 与 Python `work = list(tokens)` + `substitute` 的结构一一对应。
pub type WorkTok = Token;
