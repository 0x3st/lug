//! T9a S4：位运算 & | << >> ~（v1 新增语义；`^` 已被幂别名占用，xor 不做）。
//!
//! 语义决策（本切片锁定）：
//! - 优先级按 Python 相对序插入现有刻度：|=6 < &=7 < <</>=8，均低于 add=10；
//! - `~` 为前缀一元（Function code 71），与一元正负号同层，~x = -x-1；
//! - 仅整数域（Int/Big），Float 操作数 → 类型错误；
//! - 负数按补码语义（num-bigint & | ~ 原生支持，>> 为算术右移）；
//! - 左移护栏：est = shift×log10(2)+1 复用 D4 位数预算（1<<10**9 拦截）；
//! - 负移位 → 'Shift count must be non-negative'；左移溢出自动晋升 Big。

use lug_engine::evaluate;

fn result_of(expr: &str) -> String {
    let out = evaluate(expr).unwrap();
    lug_engine::value::format_num_value(&out.result).to_display()
}

#[test]
fn bitwise_and_or_shift_basics() {
    assert_eq!(result_of("5&3"), "1");
    assert_eq!(result_of("5|3"), "7");
    assert_eq!(result_of("1<<10"), "1024");
    assert_eq!(result_of("1024>>3"), "128");
    assert_eq!(result_of("0&0"), "0");
    assert_eq!(result_of("255|0"), "255");
}

#[test]
fn bitwise_not() {
    // 补码语义：~x = -x-1
    assert_eq!(result_of("~5"), "-6");
    assert_eq!(result_of("~0"), "-1");
    assert_eq!(result_of("~-1"), "0");
    assert_eq!(result_of("~~5"), "5");
}

#[test]
fn precedence_python_relative_order() {
    // | 最松、& 次之、移位再次、add/mul 更紧（Python 相对序）
    assert_eq!(result_of("2&3|4"), "6"); // (2&3)|4
    assert_eq!(result_of("4|2&3"), "6"); // 4|(2&3) = 4|2 = 6
    assert_eq!(result_of("1<<2&3"), "0"); // (1<<2)&3 = 4&3
    assert_eq!(result_of("3&1<<2"), "0"); // 3&(1<<2)
    assert_eq!(result_of("1<<2+3"), "32"); // 1<<(2+3)，add 紧于移位
    assert_eq!(result_of("2*3&5"), "4"); // (2*3)&5，mul 紧于 &
    assert_eq!(result_of("5&2**2"), "4"); // 5&(2**2)，幂最紧
    assert_eq!(result_of("1<<2**3"), "256"); // 1<<(2**3)
    assert_eq!(result_of("~1<<2"), "-8"); // (~1)<<2，~ 与一元同层
    // 左结合
    assert_eq!(result_of("16>>2>>1"), "2");
    assert_eq!(result_of("1<<2<<3"), "32");
}

#[test]
fn float_operands_rejected() {
    let err = evaluate("5.0&3").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    // 步骤 expr 走 format_num 渲染：5.0 整数化为 5（现有约定，见 10.0**309 → 10**309）
    assert_eq!(
        err.message,
        "Step 1 failed: 5&3: Bitwise operations require integer operands"
    );
    assert_eq!(
        evaluate("5|2.5").unwrap_err().message,
        "Step 1 failed: 5|2.5: Bitwise operations require integer operands"
    );
    assert_eq!(
        evaluate("1.5<<2").unwrap_err().message,
        "Step 1 failed: 1.5<<2: Bitwise operations require integer operands"
    );
    assert_eq!(
        evaluate("~2.5").unwrap_err().message,
        "Step 1 failed: ~(2.5): Bitwise operations require integer operands"
    );
}

#[test]
fn negative_two_complement_semantics() {
    // Python 语义（无限符号扩展补码）
    assert_eq!(result_of("-5&3"), "3");
    assert_eq!(result_of("-5|3"), "-5");
    assert_eq!(result_of("-5>>1"), "-3"); // 算术右移（向 -∞）
    assert_eq!(result_of("-8>>1"), "-4");
    assert_eq!(result_of("-1>>10"), "-1");
    assert_eq!(result_of("~-5"), "4");
}

#[test]
fn big_promotion_and_precision() {
    // 左移溢出晋升 Big
    assert_eq!(result_of("1<<62"), "4611686018427387904");
    assert_eq!(result_of("1<<63"), "9223372036854775808");
    assert_eq!(
        result_of("1<<100"),
        "1267650600228229401496703205376"
    );
    // Big 操作数
    assert_eq!(result_of("(2**100)&255"), "0");
    assert_eq!(result_of("(2**100)|0"), result_of("2**100"));
    assert_eq!(result_of("(2**100)>>99"), "2");
    assert_eq!(result_of("~(2**100)"), result_of("-(2**100)-1"));
}

#[test]
fn shift_guards() {
    // 巨大移位护栏：1<<10**9 估算 301029996 位 > 10M 预算 → 拦截（D4 复用）
    let err = evaluate("1<<10**9").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert_eq!(
        err.message,
        "Step 2 failed: 1<<1000000000: Result would have 301029996 digits (budget: 10000000)"
    );
    // 移位量为天文 Big → est 饱和拦截
    assert!(evaluate("1<<(10**100)").is_err());
    // 负移位
    assert_eq!(
        evaluate("1<<-1").unwrap_err().message,
        "Step 1 failed: 1<<-1: Shift count must be non-negative"
    );
    assert_eq!(
        evaluate("8>>-1").unwrap_err().message,
        "Step 1 failed: 8>>-1: Shift count must be non-negative"
    );
    // 右移不放大，无护栏需求
    assert_eq!(result_of("(2**1000)>>999"), "2");
}

#[test]
fn xor_explicitly_not_supported() {
    // ^ 是幂别名（D1 保留），xor 明确不做
    assert_eq!(result_of("5^3"), "125");
    assert_eq!(result_of("2^10"), "1024");
}

#[test]
fn composition_with_existing_ops() {
    // 与既有运算/函数/聚合组合
    assert_eq!(result_of("max(5&3, 7|1)"), "7"); // max(1, 7)
    assert_eq!(result_of("gcd(12, 18)&15"), "6");
    assert_eq!(result_of("sum(i, 1, 4, 1<<i)"), "30");
    assert_eq!(result_of("(1<<4)%3"), "1");
    assert_eq!(result_of("50%2|0"), "0"); // (50%2)|0：位置区分保持二元取模
}

#[test]
fn steps_and_billing() {
    let out = evaluate("5&3").unwrap();
    assert_eq!(out.steps.len(), 1);
    let s = &out.steps[0];
    assert_eq!(s.expr, "5&3");
    assert_eq!(s.operator, "&");
    assert_eq!(s.operator_code, 7);
    assert_eq!(s.state, "1");
    // 计费 D2：prompt = 3；reasoning = 1；completion = 2；total = 5
    assert_eq!(out.usage.prompt_tokens, 3);
    assert_eq!(out.usage.reasoning_tokens, 1);
    assert_eq!(out.usage.completion_tokens, 2);
    assert_eq!(out.usage.total_tokens, 5);
    // ~ 步骤（Function 形态，code 71；expr 为替换后渲染——现有 Call 约定）
    let out = evaluate("~5").unwrap();
    assert_eq!(out.steps[0].expr, "-6");
    assert_eq!(out.steps[0].operator, "~");
    assert_eq!(out.steps[0].operator_code, 71);
    assert_eq!(out.prefix_display, "~ 5");
    // << 步骤
    let out = evaluate("1<<10").unwrap();
    assert_eq!(out.steps[0].operator, "<<");
    assert_eq!(out.steps[0].operator_code, 9);
    assert_eq!(out.steps[0].state, "1024");
}
