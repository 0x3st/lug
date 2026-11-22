//! 先序（preorder / 波兰式）中间表示：AST → 编码化 token 序列 + 栈式求值。
//!
//! token 形态与原型一致（运算符编码化、数字 format_num、neg/pos 特殊键），
//! L2 扩展：`Var`（绑定变量）、`Func`（函数调用，实参为子 IR）、
//! `Special`（sum/prod：var + from/to/body 三个子 IR）。
//!
//! 求值从右往左扫描：数字入栈、二元运算符弹栈计算；函数/特殊形式递归求值
//! 子 IR。双路校验（§3.5）：纯算术区两路独立；特殊形式两路共用同一聚合核，
//! body 分别走 AST 与子 IR 求值，仍具交叉验证意义。

use crate::ast::Node;
use crate::error::ExprError;
use crate::value::{self, Ctx, Value};

/// 先序 IR token（编码化）。
#[derive(Clone, Debug)]
pub enum PrefixTok {
    /// 二元运算符编码（0–6 冻结区）。
    Op(u16),
    /// 数字（构造时已经 `format_num` 整数化，与原型 ast_to_prefix 一致）。
    Num(Value),
    /// 一元负号（原型特殊键 "neg"）。
    Neg,
    /// 一元正号（原型特殊键 "pos"）。
    Pos,
    /// 绑定变量（sum/prod 作用域内）。
    Var(String),
    /// 函数/后缀调用：编码 + 实参子 IR（每个实参独立，按声明顺序）。
    Func(u16, Vec<Vec<PrefixTok>>),
    /// 特殊形式：编码(72/73)、绑定变量名、from / to / body 子 IR。
    Special(u16, String, Vec<PrefixTok>, Vec<PrefixTok>, Vec<PrefixTok>),
}

pub struct PrefixIr {
    pub tokens: Vec<PrefixTok>,
    /// 可读波兰式，如 `+ 3 * 5 ** 2 + 1 1`、`sum i 1 10 ** i 2`。
    pub display: String,
}

/// AST → 先序 IR（镜像原型 ast_to_prefix：数字在构造时即 format_num）。
pub fn ast_to_prefix(node: &Node) -> PrefixIr {
    let mut toks = Vec::new();
    let mut syms = Vec::new();
    walk(node, &mut toks, &mut syms);
    PrefixIr {
        tokens: toks,
        display: syms.join(" "),
    }
}

fn walk(node: &Node, toks: &mut Vec<PrefixTok>, syms: &mut Vec<String>) {
    match node {
        Node::Num { value, .. } => {
            // token 保留原始值（与 tree 路径一致，避免常量精度在 IR 构建期损失；
            // 差分语料中 format_num 为恒等，兼容不变）；显示串仍走 format_num。
            toks.push(PrefixTok::Num(value.clone()));
            syms.push(value::format_num_value(value).to_display());
        }
        Node::Var { name, .. } => {
            toks.push(PrefixTok::Var(name.clone()));
            syms.push(name.clone());
        }
        Node::Unary { sign, child, .. } => {
            let (tok, sym) = if *sign == -1 {
                (PrefixTok::Neg, "-")
            } else {
                (PrefixTok::Pos, "+")
            };
            toks.push(tok);
            syms.push(sym.to_string());
            walk(child, toks, syms);
        }
        Node::Postfix {
            symbol, operand, ..
        } => {
            // 后缀在前序 IR 中前置表示（! 5 / % 50）；编码取自可调用定义
            let code = crate::registry::callable_by_symbol(symbol)
                .map(|d| d.code)
                .unwrap_or_else(|| panic!("注册表缺少后缀符号 {symbol}"));
            let sub = ast_to_prefix(operand);
            toks.push(PrefixTok::Func(code, vec![sub.tokens]));
            syms.push(format!("{symbol} {}", sub.display));
        }
        Node::Bin {
            symbol,
            left,
            right,
            ..
        } => {
            toks.push(PrefixTok::Op(code_of(symbol)));
            syms.push((*symbol).to_string());
            walk(left, toks, syms);
            walk(right, toks, syms);
        }
        Node::Call {
            func, var, args, ..
        } => {
            let code = code_of(func);
            match var {
                None => {
                    syms.push((*func).to_string());
                    let mut arg_irs = Vec::new();
                    for a in args {
                        let sub = ast_to_prefix(a);
                        syms.push(sub.display);
                        arg_irs.push(sub.tokens);
                    }
                    toks.push(PrefixTok::Func(code, arg_irs));
                }
                Some(v) => {
                    // sum/prod：var + from + to + body
                    syms.push((*func).to_string());
                    syms.push(v.clone());
                    let subs: Vec<Vec<PrefixTok>> =
                        args.iter().map(ast_to_prefix).map(|ir| ir.tokens).collect();
                    for sub in &subs {
                        // display 侧：为子 IR 重建可读串（直接递归一次）
                        // —— 简化：子 IR 的 display 在此处展开为平铺 token 串
                        let sub_display = tokens_display(sub);
                        syms.push(sub_display);
                    }
                    toks.push(PrefixTok::Special(
                        code,
                        v.clone(),
                        subs[0].clone(),
                        subs[1].clone(),
                        subs[2].clone(),
                    ));
                }
            }
        }
    }
}

/// 子 IR 的平铺可读串（display 用）。
fn tokens_display(tokens: &[PrefixTok]) -> String {
    let mut parts = Vec::new();
    for t in tokens {
        match t {
            PrefixTok::Op(c) => parts.push(
                crate::registry::by_code(*c)
                    .map(|d| d.symbol.to_string())
                    .unwrap_or_default(),
            ),
            PrefixTok::Num(v) => parts.push(v.to_string()),
            PrefixTok::Neg => parts.push("-".into()),
            PrefixTok::Pos => parts.push("+".into()),
            PrefixTok::Var(n) => parts.push(n.clone()),
            PrefixTok::Func(code, args) => {
                let sym = crate::registry::by_code(*code)
                    .map(|d| d.symbol.to_string())
                    .unwrap_or_default();
                let inner = args
                    .iter()
                    .map(|a| tokens_display(a))
                    .collect::<Vec<_>>()
                    .join(" ");
                if *code == 16 {
                    parts.push(format!("! {inner}"));
                } else {
                    parts.push(format!("{sym} {inner}"));
                }
            }
            PrefixTok::Special(code, var, from, to, body) => {
                let sym = crate::registry::by_code(*code)
                    .map(|d| d.symbol.to_string())
                    .unwrap_or_default();
                parts.push(format!(
                    "{sym} {var} {} {} {}",
                    tokens_display(from),
                    tokens_display(to),
                    tokens_display(body)
                ));
            }
        }
    }
    parts.join(" ")
}

fn code_of(symbol: &str) -> u16 {
    crate::registry::by_symbol(symbol)
        .map(|d| d.code)
        .unwrap_or_else(|| panic!("注册表缺少符号 {symbol}"))
}

/// 栈式求值：从右往左扫——数字入栈，二元运算符弹栈计算（栈顶为左操作数）；
/// 函数/特殊形式递归求值子 IR。错误信息与原型逐字节一致
/// （"Prefix evaluation failed: …"）。`env` 为绑定变量作用域链。
pub fn evaluate_prefix(
    tokens: &[PrefixTok],
    ctx: &Ctx,
    env: &[(String, Value)],
) -> Result<Value, ExprError> {
    let mut stack: Vec<Value> = Vec::new();
    for tok in tokens.iter().rev() {
        match tok {
            PrefixTok::Num(v) => stack.push(v.clone()),
            PrefixTok::Neg => {
                let top = stack.pop().expect("neg 需要一个操作数");
                let v = value::neg(&top, ctx).map_err(ExprError::computation)?;
                stack.push(v);
            }
            PrefixTok::Pos => {} // Python pos 直接跳过
            PrefixTok::Var(name) => {
                let v = env
                    .iter()
                    .rev()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| ExprError::expression(format!("undefined variable '{name}'")))?;
                stack.push(v);
            }
            PrefixTok::Op(code) => {
                let def = crate::registry::by_code(*code)
                    .unwrap_or_else(|| panic!("先序 IR 中出现未注册编码 {code}"));
                let bin = def.op.expect("Op 编码必为二元运算");
                let left = stack.pop().expect("先序 IR 操作数不足");
                let right = stack.pop().expect("先序 IR 操作数不足");
                let res = value::compute(&left, bin, &right, ctx).map_err(|err| {
                    ExprError::computation(format!(
                        "Prefix evaluation failed: {left} {} {right}: {err}",
                        def.symbol
                    ))
                })?;
                stack.push(res);
            }
            PrefixTok::Func(code, args) => {
                let def = crate::registry::by_code(*code)
                    .unwrap_or_else(|| panic!("先序 IR 中出现未注册编码 {code}"));
                let symbol = def.symbol;
                // 实参子 IR 按声明顺序求值
                let mut vals = Vec::with_capacity(args.len());
                for sub in args.iter() {
                    vals.push(evaluate_prefix(sub.as_slice(), ctx, env)?);
                }
                let res = crate::eval::apply_function(symbol, &vals, ctx).map_err(|err| {
                    ExprError::computation(format!(
                        "Prefix evaluation failed: {symbol}({}): {err}",
                        vals.iter()
                            .map(|v| v.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
                stack.push(res);
            }
            PrefixTok::Special(code, var, from_t, to_t, body_t) => {
                let symbol = crate::registry::by_code(*code)
                    .map(|d| d.symbol.to_string())
                    .unwrap_or_default();
                let from_v = evaluate_prefix(from_t, ctx, env)?;
                let to_v = evaluate_prefix(to_t, ctx, env)?;
                let (from, to) = crate::eval::aggregate_bounds(&from_v, &to_v, &symbol)
                    .map_err(ExprError::computation)?;
                let mut acc = crate::eval::aggregate_start(&symbol);
                for k in from..=to {
                    let mut env_k: Vec<(String, Value)> = env.to_vec();
                    env_k.push((var.clone(), Value::Int(k)));
                    let item = evaluate_prefix(body_t, ctx, &env_k)?;
                    acc = crate::eval::aggregate_acc(&symbol, &acc, &item, ctx)
                        .map_err(ExprError::computation)?;
                }
                stack.push(acc);
            }
        }
    }
    if stack.len() != 1 {
        return Err(ExprError::computation("Invalid prefix token sequence"));
    }
    Ok(stack.pop().unwrap())
}
