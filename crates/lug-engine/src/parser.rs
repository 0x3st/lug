//! 递归下降解析器：token 序列 → AST。
//!
//! 文法（§3.3 + L2 扩展，与 Python 原型优先级完全一致）：
//! ```text
//! add     := mul  (("+" | "-") mul)*                       prec=10 left
//! mul     := unary (("*" | "/" | "//" | "%") unary)*       prec=20 left
//! unary   := ("+" | "-") unary | postfix                   prec=15（低于 **：-2**2 = -4）
//! postfix := power ("!")*                                  prec=30（3!**2 = (3!)² = 36）
//! power   := postfix ("**" unary)?                         prec=25 right（指数可带符号）
//! atom    := NUMBER | CONST | IDENT | IDENT "(" args ")"
//!          | AGG "(" IDENT "," expr "," expr "," expr ")"
//!          | "(" add ")"
//! ```
//! 错误信息与 Python 原型逐字节一致（差分语料仲裁）；新语法的错误为 v1 新增。
//! 符号集/优先级/arity 查注册表（§3.4）。

use crate::ast::{Node, Span};
use crate::error::ExprError;
use crate::lexer::Token;
use crate::registry;

pub fn parse_tokens(tokens: &[Token]) -> Result<Node, ExprError> {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
    };
    let root = p.parse_add()?;
    if p.pos != tokens.len() {
        // Python: rest = " ".join(str(t[1]) for t in tokens[pos:])（原始值，非格式化）
        let rest = tokens[p.pos..]
            .iter()
            .map(raw_token_str)
            .collect::<Vec<_>>()
            .join(" ");
        return Err(ExprError::expression(format!(
            "Unexpected tokens after the end of expression: {rest}"
        )));
    }
    Ok(root)
}

/// Python `str(t[1])`：原始值渲染（Int/Big → 数字，Float → py repr，
/// Op/paren → 符号本身，Ident → 名字）。
fn raw_token_str(t: &Token) -> String {
    match t {
        Token::Num(v) => v.to_string(),
        Token::Op(s) => (*s).to_string(),
        Token::Paren(c) => c.to_string(),
        Token::Ident(name) => name.clone(),
    }
}

struct Parser<'a> {
    toks: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Token> {
        self.toks.get(self.pos)
    }

    fn advance(&mut self) -> Option<&'a Token> {
        let t = self.toks.get(self.pos);
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse_add(&mut self) -> Result<Node, ExprError> {
        let mut left = self.parse_mul()?;
        let syms = registry::symbols_at(10, registry::Assoc::Left);
        while let Some(Token::Op(s)) = self.peek() {
            if !syms.contains(s) {
                break;
            }
            let s = *s;
            self.advance();
            let right = self.parse_mul()?;
            let span: Span = (left.span().0, right.span().1);
            left = Node::Bin {
                op: bin_op_of(s),
                symbol: s,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Node, ExprError> {
        let mut left = self.parse_unary()?;
        let syms = registry::symbols_at(20, registry::Assoc::Left);
        while let Some(Token::Op(s)) = self.peek() {
            if !syms.contains(s) {
                break;
            }
            let s = *s;
            self.advance();
            let right = self.parse_unary()?;
            let span: Span = (left.span().0, right.span().1);
            left = Node::Bin {
                op: bin_op_of(s),
                symbol: s,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Node, ExprError> {
        if let Some(Token::Op(s)) = self.peek() {
            if *s == "+" || *s == "-" {
                let start = self.pos;
                let sign = if *s == "-" { -1 } else { 1 };
                self.advance();
                let child = self.parse_unary()?;
                let span: Span = (start, child.span().1);
                return Ok(Node::Unary {
                    sign,
                    child: Box::new(child),
                    span,
                });
            }
        }
        self.parse_power()
    }

    /// 后缀链：`3!!` = (3!)!；优先级高于 `**`（3!**2 = (3!)² = 36）。
    /// T9a S1：`%` 百分号后缀与 `!` 同层（prec=30）。同一 `%` token 的双角色
    /// 按语法位置区分：仅当其后不跟操作数起始 token（数字/标识符/`(`/带符号
    /// 一元）时视为后缀，否则保持二元取模（差分语料语义逐字节不变）。
    /// 连写 `%%` 报语法错误（先 % 后无意义）。
    fn parse_postfix(&mut self) -> Result<Node, ExprError> {
        let mut node = self.parse_atom()?;
        let mut percent_applied = false;
        loop {
            match self.peek() {
                Some(Token::Op("!")) => {
                    self.advance();
                    let span: Span = (node.span().0, self.pos);
                    node = Node::Postfix {
                        symbol: "!",
                        operand: Box::new(node),
                        span,
                    };
                }
                Some(Token::Op("%")) => {
                    if starts_operand(self.toks.get(self.pos + 1)) {
                        break; // 二元取模：交给 mul 层
                    }
                    if percent_applied {
                        return Err(ExprError::expression(format!(
                            "Syntax error: unexpected '%' at position {}, expected a number or '('",
                            self.pos
                        )));
                    }
                    self.advance();
                    let span: Span = (node.span().0, self.pos);
                    node = Node::Postfix {
                        symbol: "%",
                        operand: Box::new(node),
                        span,
                    };
                    percent_applied = true;
                }
                _ => break,
            }
        }
        Ok(node)
    }

    fn parse_power(&mut self) -> Result<Node, ExprError> {
        let base = self.parse_postfix()?;
        // ** 右结合：指数可带一元符号（2**-3 合法）
        if let Some(Token::Op("**")) = self.peek() {
            self.advance();
            let exp = self.parse_unary()?;
            let span: Span = (base.span().0, exp.span().1);
            return Ok(Node::Bin {
                op: bin_op_of("**"),
                symbol: "**",
                left: Box::new(base),
                right: Box::new(exp),
                span,
            });
        }
        Ok(base)
    }

    fn parse_atom(&mut self) -> Result<Node, ExprError> {
        match self.peek() {
            Some(Token::Num(_)) => {
                let value = match self.advance() {
                    Some(Token::Num(v)) => v.clone(),
                    _ => unreachable!(),
                };
                let span: Span = (self.pos - 1, self.pos);
                Ok(Node::Num { value, span })
            }
            Some(Token::Ident(name)) => {
                let name = name.clone();
                self.advance();
                // 函数调用 / 聚合特殊形式
                if matches!(self.peek(), Some(Token::Paren('('))) {
                    return self.parse_call(&name);
                }
                // 裸标识符 = 变量（自由变量在求值期报 undefined）
                let span: Span = (self.pos - 1, self.pos);
                Ok(Node::Var { name, span })
            }
            Some(Token::Paren('(')) => {
                let start = self.pos;
                self.advance();
                let mut node = self.parse_add()?;
                if !matches!(self.peek(), Some(Token::Paren(')'))) {
                    return Err(ExprError::expression("Mismatched parentheses: missing ')'"));
                }
                self.advance();
                // span 包含括号：化简时连同括号一起替换（与原型一致）
                node.set_span((start, self.pos));
                Ok(node)
            }
            other => {
                // Python: got = val if val is not None else "end of expression"；
                // 数字 → 裸值（repr 无引号），其余 → 带引号的 repr
                let got = match other {
                    None => "'end of expression'".to_string(),
                    Some(Token::Num(v)) => v.to_string(),
                    Some(Token::Op(s)) => format!("'{s}'"),
                    Some(Token::Paren(c)) => format!("'{c}'"),
                    Some(Token::Ident(name)) => format!("'{name}'"),
                };
                Err(ExprError::expression(format!(
                    "Syntax error: unexpected {got} at position {}, expected a number or '('",
                    self.pos
                )))
            }
        }
    }

    /// 函数调用与聚合特殊形式（假定 IDENT 已消费、当前指向 '('）。
    fn parse_call(&mut self, name: &str) -> Result<Node, ExprError> {
        let start = self.pos - 1; // IDENT 的下标
        let def = registry::by_symbol(name)
            .ok_or_else(|| ExprError::expression(format!("Unknown function '{name}'")))?;

        match def.kind {
            // ---- 特殊形式：sum(var, from, to, body) / prod(...) ----
            registry::OpKind::Special => {
                self.advance(); // 消耗 '('
                let var = match self.peek() {
                    Some(Token::Ident(v)) => {
                        let v = v.clone();
                        self.advance();
                        v
                    }
                    _ => {
                        return Err(ExprError::expression(format!(
                            "{name}() expects a variable name as its first argument"
                        )))
                    }
                };
                self.expect_comma(name)?;
                let from = self.parse_add()?;
                self.expect_comma(name)?;
                let to = self.parse_add()?;
                self.expect_comma(name)?;
                let body = self.parse_add()?;
                if !matches!(self.peek(), Some(Token::Paren(')'))) {
                    return Err(ExprError::expression(format!(
                        "{}() expects (var, from, to, body)",
                        name
                    )));
                }
                self.advance();
                let span: Span = (start, self.pos);
                Ok(Node::Call {
                    func: def.symbol,
                    var: Some(var),
                    args: vec![from, to, body],
                    span,
                })
            }
            // ---- 普通函数：name(arg, ...) ----
            _ => {
                self.advance(); // 消耗 '('
                let mut args = Vec::new();
                if !matches!(self.peek(), Some(Token::Paren(')'))) {
                    loop {
                        args.push(self.parse_add()?);
                        if matches!(self.peek(), Some(Token::Paren(')'))) {
                            self.advance();
                            break;
                        }
                        if matches!(self.peek(), Some(Token::Op(","))) {
                            self.advance();
                            continue;
                        }
                        return Err(ExprError::expression("Mismatched parentheses: missing ')'"));
                    }
                } else {
                    self.advance();
                }
                let span: Span = (start, self.pos);
                Ok(Node::Call {
                    func: def.symbol,
                    var: None,
                    args,
                    span,
                })
            }
        }
    }

    fn expect_comma(&mut self, name: &str) -> Result<(), ExprError> {
        if matches!(self.peek(), Some(Token::Op(","))) {
            self.advance();
            Ok(())
        } else {
            Err(ExprError::expression(format!(
                "{name}() expects (var, from, to, body)"
            )))
        }
    }
}

fn bin_op_of(symbol: &str) -> crate::value::BinOp {
    registry::by_symbol(symbol)
        .and_then(|d| d.op)
        .unwrap_or_else(|| panic!("注册表缺少二元符号 {symbol}"))
}

/// `%` 位置区分：这些 token 出现在 `%` 之后 → 二元取模的操作数起点；
/// 其余（运算符/`)`/`,`/结尾）→ 百分号后缀。
fn starts_operand(t: Option<&Token>) -> bool {
    match t {
        None => false,
        Some(Token::Num(_)) | Some(Token::Ident(_)) | Some(Token::Paren('(')) => true,
        Some(Token::Op(s)) => *s == "+" || *s == "-",
        Some(Token::Paren(_)) => false,
    }
}
