//! 后序遍历逐步求值：实际计算顺序，逐步化简并记录步骤（SSE 过程流的来源）。
//!
//! 与 Python `evaluate_tree` 一致：工作区 = 原始 token 列表，子树求值完成后
//! 用 `substitute(span, value)` 替换为数值 token，`state` 为整表渲染。
//! 步骤的字段（expr/operator/operator_code/result/state）与原型逐字节一致。

use crate::ast::{Node, WorkTok};
use crate::error::ExprError;
use crate::eval;
use crate::lexer::Token;
use crate::value::{self, Ctx, Value};

/// 单个化简步骤。
#[derive(Clone, Debug)]
pub struct Step {
    /// 步骤序号（1 起）。
    pub step: usize,
    /// 被化简的子表达式文本（含括号剥离，见下）。
    pub expr: String,
    /// 规范运算符符号。
    pub operator: &'static str,
    /// 注册表编码。
    pub operator_code: u16,
    /// 该步结果（已 format_num 整数化，与原型一致）。
    pub result: Value,
    /// 化简后的整式状态。
    pub state: String,
}

pub struct TreeEval {
    pub result: Value,
    pub steps: Vec<Step>,
}

/// 后序求值入口。
pub fn evaluate_tree(root: &Node, tokens: &[Token], ctx: &Ctx) -> Result<TreeEval, ExprError> {
    let work = tokens.iter().cloned().map(Some).collect();
    let mut st = Evaluator {
        work,
        steps: Vec::new(),
        ctx: *ctx,
        env: Vec::new(),
        suppress: false,
    };
    let result = st.eval_node(root)?;
    Ok(TreeEval {
        result,
        steps: st.steps,
    })
}

struct Evaluator {
    work: Vec<Option<WorkTok>>,
    steps: Vec<Step>,
    ctx: Ctx,
    /// 绑定变量栈（sum/prod 迭代作用域；后进先出，支持嵌套遮蔽）。
    env: Vec<(String, Value)>,
    /// Opaque 抑制标志：sum/prod body 求值期间不发射步骤（整体单步，§3.6）。
    suppress: bool,
}

impl Evaluator {
    /// 把 span 区间替换为数值 token：起点放值，其余置 None（渲染时跳过）。
    fn substitute(&mut self, span: (usize, usize), value: Value) {
        let (s, e) = span;
        self.work[s] = Some(Token::Num(value));
        for w in self.work.iter_mut().take(e).skip(s + 1) {
            *w = None;
        }
    }

    /// 渲染工作区（跳过 None）：数字走 format_num，其余原样。
    fn render(&self, range: Option<(usize, usize)>) -> String {
        let mut parts = Vec::new();
        let slice = match range {
            Some((s, e)) => &self.work[s..e],
            None => &self.work[..],
        };
        for t in slice.iter().flatten() {
            match t {
                Token::Num(v) => parts.push(value::format_num_value(v).to_display()),
                Token::Op(s) => parts.push((*s).to_string()),
                Token::Paren(c) => parts.push(c.to_string()),
                Token::Ident(name) => parts.push(name.clone()),
            }
        }
        parts.join("")
    }

    /// 函数/聚合求值。sum/prod 为 Opaque：body 求值不产生步骤，
    /// 整个调用只产生一步（expr = 调用源文本，§3.6 计费可预期）。
    fn eval_call(
        &mut self,
        func: &str,
        var: Option<&str>,
        args: &[Node],
    ) -> Result<Value, ExprError> {
        let step_fail = |st: &Self, text: String, err: String| {
            ExprError::computation(format!(
                "Step {} failed: {}: {}",
                st.steps.len() + 1,
                text,
                err
            ))
        };
        match var {
            None => {
                let mut vals = Vec::with_capacity(args.len());
                for (idx, arg) in args.iter().enumerate() {
                    let v = self.eval_node(arg).map_err(|e| {
                        step_fail(self, format!("{}() 实参 #{}", func, idx + 1), e.message)
                    })?;
                    vals.push(v);
                }
                eval::apply_function(func, &vals, &self.ctx)
                    .map_err(|err| step_fail(self, self.render_call_source(func, args), err))
            }
            Some(var_name) => {
                // sum(var, from, to, body) / prod(...)
                if args.len() != 3 {
                    return Err(ExprError::computation(format!(
                        "{func}() expects (var, from, to, body)"
                    )));
                }
                let from_v = self.eval_node(&args[0])?;
                let to_v = self.eval_node(&args[1])?;
                let (from, to) = eval::aggregate_bounds(&from_v, &to_v, func)
                    .map_err(|err| step_fail(self, self.render_call_source(func, args), err))?;
                let mut acc = eval::aggregate_start(func);
                self.suppress = true; // Opaque：body 内部步骤不发射
                for k in from..=to {
                    self.env.push((var_name.to_string(), Value::Int(k)));
                    let item = self.eval_node(&args[2]);
                    self.env.pop();
                    let item = item.map_err(|e| {
                        step_fail(self, self.render_call_source(func, args), e.message)
                    })?;
                    acc = eval::aggregate_acc(func, &acc, &item, &self.ctx)
                        .map_err(|err| step_fail(self, self.render_call_source(func, args), err))?;
                }
                Ok(acc)
            }
        }
    }

    /// 调用源的文本渲染（用于 Opaque 步骤的错误信息；正常步骤用 span 渲染）。
    fn render_call_source(&self, func: &str, args: &[Node]) -> String {
        let inner = args
            .iter()
            .map(|a| self.render(Some(a.span())))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{func}({inner})")
    }

    fn eval_node(&mut self, node: &Node) -> Result<Value, ExprError> {
        match node {
            Node::Num { value, .. } => Ok(value.clone()),
            Node::Var { name, .. } => self
                .env
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| ExprError::expression(format!("undefined variable '{name}'"))),
            Node::Unary { sign, child, .. } => {
                // Python: sign * eval(child)；一元不产生步骤、不计费
                let v = self.eval_node(child)?;
                match sign {
                    1 => Ok(v),
                    _ => value::neg(&v, &self.ctx).map_err(|e| {
                        ExprError::computation(format!(
                            "Step {} failed: {}: {}",
                            self.steps.len() + 1,
                            self.render(Some(node.span())),
                            e
                        ))
                    }),
                }
            }
            Node::Postfix {
                symbol,
                operand,
                span,
            } => {
                let v = self.eval_node(operand)?;
                let expr_text = self.render(Some(*span));
                let res = eval::apply_function(symbol, &[v], &self.ctx).map_err(|err| {
                    ExprError::computation(format!(
                        "Step {} failed: {}: {}",
                        self.steps.len() + 1,
                        expr_text,
                        err
                    ))
                })?;
                self.substitute(*span, res.clone());
                let code = crate::registry::by_symbol(symbol)
                    .map(|d| d.code)
                    .unwrap_or_else(|| panic!("注册表缺少符号 {symbol}"));
                if !self.suppress {
                    self.steps.push(Step {
                        step: self.steps.len() + 1,
                        expr: expr_text,
                        operator: symbol,
                        operator_code: code,
                        result: formatted_to_value(&value::format_num_value(&res)),
                        state: self.render(None),
                    });
                }
                Ok(res)
            }
            Node::Call {
                func,
                var,
                args,
                span,
            } => {
                let parent_suppress = self.suppress; // 内层 Opaque 不得污染外层 emit
                let res = self.eval_call(func, var.as_deref(), args)?;
                self.substitute(*span, res.clone());
                let code = crate::registry::by_symbol(func)
                    .map(|d| d.code)
                    .unwrap_or_else(|| panic!("注册表缺少符号 {func}"));
                // 聚合特殊形式 = Opaque 单步（§3.6：body 不展开，费用可预期）；
                // 普通函数 = Always 单步。
                if !parent_suppress {
                    self.steps.push(Step {
                        step: self.steps.len() + 1,
                        expr: self.render(Some(*span)),
                        operator: func,
                        operator_code: code,
                        result: formatted_to_value(&value::format_num_value(&res)),
                        state: self.render(None),
                    });
                }
                Ok(res)
            }
            Node::Bin {
                op,
                symbol,
                left,
                right,
                span,
            } => {
                let a = self.eval_node(left)?;
                let b = self.eval_node(right)?;
                let expr_text = {
                    let t = self.render(Some(*span));
                    // 展示时剥掉最外层括号（原型同款朴素判断）
                    if t.starts_with('(') && t.ends_with(')') {
                        t[1..t.len() - 1].to_string()
                    } else {
                        t
                    }
                };
                let res = value::compute(&a, *op, &b, &self.ctx).map_err(|err| {
                    ExprError::computation(format!(
                        "Step {} failed: {}: {}",
                        self.steps.len() + 1,
                        expr_text,
                        err
                    ))
                })?;
                self.substitute(*span, res.clone());
                let code = crate::registry::by_symbol(symbol)
                    .map(|d| d.code)
                    .unwrap_or_else(|| panic!("注册表缺少符号 {symbol}"));
                // Opaque 抑制期（sum/prod body 内）不发射步骤
                if !self.suppress {
                    self.steps.push(Step {
                        step: self.steps.len() + 1,
                        expr: expr_text,
                        operator: symbol,
                        operator_code: code,
                        result: formatted_to_value(&value::format_num_value(&res)),
                        state: self.render(None),
                    });
                }
                Ok(res)
            }
        }
    }
}

fn formatted_to_value(f: &value::Formatted) -> Value {
    match f {
        value::Formatted::Int(v) => Value::Int(*v),
        value::Formatted::Big(b) => Value::Big(b.clone()),
        value::Formatted::Float(x) => Value::Float(*x),
    }
}
