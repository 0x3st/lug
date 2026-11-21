//! 错误类型：携带响应用的 `code`，与 Python 原型 `ExprError` 语义一致。

use std::fmt;

/// 错误类别：语法类（expression_error）| 计算类（computation_error）。
/// 解析前端的 `ambiguous_input` 属于新类别，待 Module A 落地时扩展。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprErrorCode {
    ExpressionError,
    ComputationError,
}

impl ExprErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExprErrorCode::ExpressionError => "expression_error",
            ExprErrorCode::ComputationError => "computation_error",
        }
    }
}

/// 表达式解析/求值错误。`message` 与 Python 原型逐字节一致（golden 仲裁）。
#[derive(Clone, Debug)]
pub struct ExprError {
    pub message: String,
    pub code: ExprErrorCode,
}

impl ExprError {
    pub fn expression(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: ExprErrorCode::ExpressionError,
        }
    }

    pub fn computation(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: ExprErrorCode::ComputationError,
        }
    }
}

impl fmt::Display for ExprError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ExprError {}
