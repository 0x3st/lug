//! 引擎单元测试：差分语料（differential.rs）之外的 v1 特有语义——
//! 运算预算护栏（D4）、Big 自动晋升（C 档）、双格式化边界、py repr。

use lug_engine::value::{self, Ctx};
use lug_engine::{evaluate, evaluate_with};

fn ctx_with_digits(n: usize) -> Ctx {
    Ctx { max_digits: n }
}

#[test]
fn budget_guard_blocks_nested_pow() {
    // (10**3000)**4000：step 1 先算出 10^3000（3001 位，预算内）；
    // step 2 幂运算前预估位数 = digits(a)×b = 3001×4000+1 > 预算 → 拦截（D4）。
    // 注意预估公式 digits(a)×exp 是安全上界（可能高估，绝不低估）。
    let err = evaluate("(10**3000)**4000").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert!(
        err.message.starts_with("Step 2 failed: 1000")
            && err
                .message
                .ends_with("**4000: Result would have 12004001 digits (budget: 10000000)"),
        "实际消息: {}",
        &err.message[..120]
    );
}

#[test]
fn budget_guard_configurable() {
    // 10**100 = 101 位：默认预算内；预算收紧到 50 位后拦截
    assert!(evaluate("10**100").is_ok());
    let err = evaluate_with("10**100", &ctx_with_digits(50)).unwrap_err();
    // 安全上界：digits(10)×100+1 = 201（实际 101 位；上界只多不少，保证不漏拦）
    assert_eq!(
        err.message,
        "Step 1 failed: 10**100: Result would have 201 digits (budget: 50)"
    );
}

#[test]
fn big_promotion_exact_ints() {
    // i64 溢出自动晋升 Big（C 档）：与 Python 任意精度一致
    let out = evaluate("9223372036854775807 + 1").unwrap();
    assert_eq!(
        value::format_num_value(&out.result).to_display(),
        "9223372036854775808"
    );
    // 阶乘域：20! 精确（i64 边界内的乘法链）
    let out = evaluate("2**100").unwrap();
    assert_eq!(
        value::format_num_value(&out.result).to_display(),
        "1267650600228229401496703205376"
    );
}

#[test]
fn float_non_finite_is_error_not_infinity() {
    // D3：原型会输出 Infinity（非法 JSON）；v1 报 Numeric overflow
    // 状态渲染把 10.0 整数化为 10（format_num 语义），错误信息随之
    let err = evaluate("10.0**309").unwrap_err();
    assert_eq!(err.message, "Step 1 failed: 10**309: Numeric overflow");
}

#[test]
fn double_formatting_quirks() {
    // 原型双重格式化的三个边界：
    // ① compute 已 format_num（2/3 → 0.666666666667 反馈进后续计算）
    assert_eq!(
        lug_engine::value::format_num_value(&evaluate("2/3*3").unwrap().result).to_display(),
        "2.000000000001"
    );
    // ② round12 归零的浮点在响应层整数化（10.0**-308 → 0.0 → 0）
    assert_eq!(
        lug_engine::value::format_num_value(&evaluate("10.0**-308").unwrap().result).to_display(),
        "0"
    );
    // ③ round(1.9999999999999998, 12) = 2.0（float，不回整）
    assert_eq!(
        lug_engine::value::format_num_value(&evaluate("1.9999999999999998").unwrap().result)
            .to_display(),
        "2.0"
    );
}

#[test]
fn py_repr_matches_python_str() {
    use value::py_float_repr;
    assert_eq!(py_float_repr(4.0), "4.0");
    assert_eq!(py_float_repr(0.1), "0.1");
    assert_eq!(py_float_repr(1e21), "1e+21");
    assert_eq!(py_float_repr(1.5e16), "1.5e+16");
    assert_eq!(py_float_repr(1e-5), "1e-05");
    assert_eq!(py_float_repr(0.0001), "0.0001");
    assert_eq!(py_float_repr(-0.0), "-0.0");
    assert_eq!(py_float_repr(100000.0), "100000.0");
}

#[test]
fn python_floor_and_mod_semantics() {
    // floor 除向 -∞、模随除数符号（Rust 原生语义相反，必须走 Python 路径）
    for (expr, expect) in [
        ("-7//2", "-4"),
        ("7//-2", "-4"),
        ("-7%2", "1"),
        ("7%-2", "-1"),
        ("-10%-3", "-1"),
    ] {
        let out = evaluate(expr).unwrap();
        assert_eq!(
            value::format_num_value(&out.result).to_display(),
            expect,
            "{expr}"
        );
    }
}

#[test]
fn prefix_tokens_are_registry_encoded() {
    // 先序 IR 编码：0=+ 1=- 2=* 3=/ 4=// 5=% 6=**（冻结承诺）
    let out = evaluate("3+5*2**(1+1)").unwrap();
    assert_eq!(out.prefix_display, "+ 3 * 5 ** 2 + 1 1");
}

#[test]
fn usage_is_step_counted() {
    // 计费口径 D2：prompt = 词法 token 数；reasoning = 步骤数；completion = +1
    let out = evaluate("3+5*2**(1+1)").unwrap();
    assert_eq!(out.usage.prompt_tokens, 11);
    assert_eq!(out.usage.reasoning_tokens, 4);
    assert_eq!(out.usage.completion_tokens, 5);
    assert_eq!(out.usage.total_tokens, 16);
}
