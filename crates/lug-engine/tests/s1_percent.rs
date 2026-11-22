//! T9a S1：百分号后缀 `50%`（v1 新增语义，无 Python 原型仲裁，错误信息自洽）。
//!
//! 语义决策（本切片锁定）：
//! - `%` 同 token 双角色：后跟操作数起始 token（数字/标识符/`(`/带符号一元）时
//!   保持二元取模（与差分语料逐字节一致）；否则视为百分号后缀（除以 100）；
//! - 百分号后缀与 `!` 同居 postfix 层（prec=30，高于 `**`）；
//! - `50%!`：先 % 后 !（postfix 链左结合），0.5 非非负整数 → 阶乘域错误；
//! - `50%%` 连写 → 语法错误；
//! - 步进计量：百分号转换产生独立一步（与 `!` 同款 Always 步）。

use lug_engine::evaluate;

fn result_of(expr: &str) -> String {
    let out = evaluate(expr).unwrap();
    lug_engine::value::format_num_value(&out.result).to_display()
}

#[test]
fn standalone_percent_value() {
    assert_eq!(result_of("50%"), "0.5");
    assert_eq!(result_of("0%"), "0");
    // Float 展示：200% 整值浮点经 format_num 整数化（与 sqrt(4)→2 同口径）
    assert_eq!(result_of("200%"), "2");
    assert_eq!(result_of("12.5%"), "0.125");
    // Big 操作数走 f64；整值浮点按引擎口径经 format_num 整数化为 Big 展示
    assert_eq!(result_of("(10**20)%"), "1000000000000000000");
}

#[test]
fn percent_in_arithmetic() {
    assert_eq!(result_of("200*10%"), "20");
    assert_eq!(result_of("50%*2"), "1");
    assert_eq!(result_of("50%/2"), "0.25");
    assert_eq!(result_of("100-10%"), "99.9");
    assert_eq!(result_of("(10%)+(10%)"), "0.2");
    // 位置区分在起作用：% 后跟带符号一元 → 二元取模（10 % (10%)）
    assert_eq!(result_of("10%+10%"), "0");
    assert_eq!(result_of("max(50%, 1)"), "1");
    assert_eq!(result_of("min(50%, 1)"), "0.5");
}

#[test]
fn percent_next_to_factorial() {
    // 决策：先 % 后 !（postfix 链左结合）→ 0.5! 阶乘域错误；
    // % 是独立一步，故阶乘是 Step 2，expr 渲染为替换后的 0.5!
    let err = evaluate("50%!").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert_eq!(
        err.message,
        "Step 2 failed: 0.5!: Factorial requires a non-negative integer"
    );
}

#[test]
fn percent_with_unary_minus() {
    // 后缀优先于一元负号（与 -50! = -(50!) 同构）
    assert_eq!(result_of("-50%"), "-0.5");
    assert_eq!(result_of("-12.5%"), "-0.125");
    assert_eq!(result_of("-(50%)"), "-0.5");
}

#[test]
fn double_percent_is_syntax_error() {
    let err = evaluate("50%%").unwrap_err();
    assert_eq!(err.code.as_str(), "expression_error");
    assert_eq!(
        err.message,
        "Syntax error: unexpected '%' at position 2, expected a number or '('"
    );
    // 穿插 ! 后再连写同样拒绝
    assert!(evaluate("50%!%").is_err());
}

#[test]
fn percent_precedence_postfix_level() {
    // 与 ! 同级（prec=30，高于 **）：2**10% = 2**(10%)
    assert_eq!(result_of("2**10%"), "1.071773462536");
    assert_eq!(result_of("50%**2"), "0.25");
    assert_eq!(result_of("(1+1)%"), "0.02");
}

#[test]
fn binary_mod_unchanged_by_position() {
    // 词法同一 token，语法位置区分：后跟操作数 → 二元取模（差分语料语义不变）
    assert_eq!(result_of("50%3"), "2");
    assert_eq!(result_of("7%2"), "1");
    assert_eq!(result_of("50%+1"), "0"); // +1 是带符号操作数 → 二元取模
    assert_eq!(result_of("50%-2"), "0");
    assert_eq!(result_of("(100)%7"), "2");
    assert_eq!(result_of("10%(2+1)"), "1");
}

#[test]
fn percent_step_and_billing() {
    // 步进决策：百分号转换 = 独立一步（operator %, code 17）
    let out = evaluate("50%").unwrap();
    assert_eq!(out.steps.len(), 1);
    let s = &out.steps[0];
    assert_eq!(s.expr, "50%");
    assert_eq!(s.operator, "%");
    assert_eq!(s.operator_code, 17);
    assert_eq!(
        lug_engine::value::format_num_value(&s.result).to_display(),
        "0.5"
    );
    assert_eq!(s.state, "0.5");
    assert_eq!(out.prefix_display, "% 50");
    // 计费 D2：prompt = 2 词法 token；reasoning = 1 步；completion = 2；total = 4
    assert_eq!(out.usage.prompt_tokens, 2);
    assert_eq!(out.usage.reasoning_tokens, 1);
    assert_eq!(out.usage.completion_tokens, 2);
    assert_eq!(out.usage.total_tokens, 4);
    // 组合表达式步骤流：百分号独立一步 + 乘法一步（各 Always 计步）
    let out = evaluate("200*10%").unwrap();
    assert_eq!(out.steps.len(), 2);
    assert_eq!(out.steps[0].expr, "10%");
    assert_eq!(out.steps[0].operator, "%");
    assert_eq!(out.steps[1].expr, "200*0.1");
    assert_eq!(out.steps[1].state, "20");
}

#[test]
fn percent_no_division_by_zero_path() {
    // 除以常量 100，不存在零除路径；0% 恒为 0
    assert_eq!(result_of("0%"), "0");
    assert_eq!(result_of("(1-1)%"), "0");
}
