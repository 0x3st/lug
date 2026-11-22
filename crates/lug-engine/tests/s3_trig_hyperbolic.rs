//! T9a S3：反三角 asin/acos/atan/atan2 与双曲 sinh/cosh/tanh
//! （v1 新增语义，f64 路径，输出格式与现有 Float 一致）。
//!
//! 语义决策（本切片锁定）：
//! - asin/acos 前置域检查（出域报错，不返回 NaN）：|x| > 1 → 域错误；
//! - atan2(y, x) 象限完整；atan2(0, 0) = 0（与 Python math.atan2 一致）；
//! - sinh/cosh 溢出 → "Numeric overflow"（D3 家族：非有限即报错）；
//! - 函数计步：每调用 1 个 Always 步。

use lug_engine::evaluate;

fn result_of(expr: &str) -> String {
    let out = evaluate(expr).unwrap();
    lug_engine::value::format_num_value(&out.result).to_display()
}

#[test]
fn inverse_trig_values() {
    assert_eq!(result_of("asin(0.5)"), "0.523598775598");
    assert_eq!(result_of("asin(1)"), "1.570796326795");
    assert_eq!(result_of("asin(-1)"), "-1.570796326795");
    assert_eq!(result_of("asin(0)"), "0");
    assert_eq!(result_of("acos(0.5)"), "1.047197551197");
    assert_eq!(result_of("acos(1)"), "0");
    assert_eq!(result_of("acos(-1)"), "3.14159265359");
    assert_eq!(result_of("atan(1)"), "0.785398163397");
    assert_eq!(result_of("atan(0)"), "0");
    assert_eq!(result_of("atan(-1)"), "-0.785398163397");
}

#[test]
fn inverse_trig_domain_errors() {
    // 决策：前置域检查（符合 lug D3 风格：报错而非 NaN/Infinity）
    let err = evaluate("asin(2)").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert_eq!(
        err.message,
        "Step 1 failed: asin(2): asin(x): x must be in [-1, 1]"
    );
    assert_eq!(
        evaluate("asin(-1.5)").unwrap_err().message,
        "Step 1 failed: asin(-1.5): asin(x): x must be in [-1, 1]"
    );
    assert_eq!(
        evaluate("acos(2)").unwrap_err().message,
        "Step 1 failed: acos(2): acos(x): x must be in [-1, 1]"
    );
    assert_eq!(
        evaluate("acos(-2)").unwrap_err().message,
        "Step 1 failed: acos(-2): acos(x): x must be in [-1, 1]"
    );
}

#[test]
fn atan2_quadrants() {
    assert_eq!(result_of("atan2(1, 1)"), "0.785398163397");      // π/4
    assert_eq!(result_of("atan2(1, -1)"), "2.356194490192");     // 3π/4
    assert_eq!(result_of("atan2(-1, -1)"), "-2.356194490192");   // −3π/4
    assert_eq!(result_of("atan2(-1, 1)"), "-0.785398163397");    // −π/4
    assert_eq!(result_of("atan2(0, -1)"), "3.14159265359");      // π
    assert_eq!(result_of("atan2(0, 1)"), "0");
    // 决策：atan2(0, 0) = 0（与 Python math.atan2(0, 0) 一致）
    assert_eq!(result_of("atan2(0, 0)"), "0");
}

#[test]
fn hyperbolic_values() {
    assert_eq!(result_of("sinh(1)"), "1.175201193644");
    assert_eq!(result_of("cosh(1)"), "1.543080634815");
    assert_eq!(result_of("tanh(1)"), "0.761594155956");
    assert_eq!(result_of("sinh(0)"), "0");
    assert_eq!(result_of("cosh(0)"), "1");
    assert_eq!(result_of("tanh(0)"), "0");
    // tanh 饱和 → 1.0 整数化（format_num 口径）
    assert_eq!(result_of("tanh(100)"), "1");
    // 奇偶性
    assert_eq!(result_of("sinh(-2)"), result_of("-sinh(2)"));
    assert_eq!(result_of("cosh(-2)"), result_of("cosh(2)"));
}

#[test]
fn hyperbolic_overflow_is_error() {
    // D3：非有限浮点报错（sinh(1000) → inf）
    let err = evaluate("sinh(1000)").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert_eq!(
        err.message,
        "Step 1 failed: sinh(1000): Numeric overflow"
    );
    assert_eq!(
        evaluate("cosh(1000)").unwrap_err().message,
        "Step 1 failed: cosh(1000): Numeric overflow"
    );
}

#[test]
fn arity_and_nesting() {
    // atan2 恰好两参
    assert_eq!(
        evaluate("atan2(1)").unwrap_err().message,
        "Step 1 failed: atan2(1): atan2() expects exactly 2 arguments, got 1"
    );
    // 套娃：asin(sin(0.5)) ≈ 0.5（round12 往返）
    assert_eq!(result_of("asin(sin(0.5))"), "0.5");
    // π 恒等：2*asin(1) = π
    assert_eq!(result_of("2*asin(1)"), result_of("pi"));
    // 聚合 body 中可用：tanh(1)+tanh(2)+tanh(3)
    assert_eq!(result_of("sum(i, 1, 3, tanh(i))"), "2.720676489719");
}

#[test]
fn steps_and_billing() {
    let out = evaluate("asin(0.5)").unwrap();
    assert_eq!(out.steps.len(), 1);
    let s = &out.steps[0];
    assert_eq!(s.expr, "0.523598775598");
    assert_eq!(s.operator, "asin");
    assert_eq!(s.operator_code, 64);
    assert_eq!(s.state, "0.523598775598");
    assert_eq!(out.prefix_display, "asin 0.5");
    // 计费 D2：prompt = 4（asin / ( / 0.5 / )）；reasoning = 1；completion = 2；total = 6
    assert_eq!(out.usage.prompt_tokens, 4);
    assert_eq!(out.usage.reasoning_tokens, 1);
    assert_eq!(out.usage.completion_tokens, 2);
    assert_eq!(out.usage.total_tokens, 6);
    // atan2 步骤
    let out = evaluate("atan2(1, 1)").unwrap();
    assert_eq!(out.steps[0].operator, "atan2");
    assert_eq!(out.steps[0].operator_code, 67);
}
