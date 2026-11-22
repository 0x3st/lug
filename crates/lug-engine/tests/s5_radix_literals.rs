//! T9a S5：进制字面量 0x/0b/0o（lexer 数字字面量扩展，v1 新增语义）。
//!
//! 语义决策（本切片锁定）：
//! - 前缀大小写均接受（0x/0X/0b/0B/0o/0O，与 Python 一致）；
//! - 数字 run 内出现非法位 → 'Invalid number literal: {run}'（整 run 展示）；
//! - 裸前缀（无任何数字）→ 'Invalid number literal: 0x'；
//! - 值域 Int，溢出自动晋升 Big（BigInt::parse_bytes(digits, radix)）；
//! - 负号 -0xFF 由一元负号自然组合；与 Float 混合按现有数值塔；
//! - 词法 1 个 token（prompt 计量与十进制字面量同权）；
//! - 范围收缩：bin()/hex()/oct() 字符串转换函数不做（Value 无字符串类型）。

use lug_engine::evaluate;

fn result_of(expr: &str) -> String {
    let out = evaluate(expr).unwrap();
    lug_engine::value::format_num_value(&out.result).to_display()
}

#[test]
fn radix_literal_values() {
    assert_eq!(result_of("0xFF"), "255");
    assert_eq!(result_of("0XFF"), "255");
    assert_eq!(result_of("0xff"), "255");
    assert_eq!(result_of("0b1010"), "10");
    assert_eq!(result_of("0B11"), "3");
    assert_eq!(result_of("0o17"), "15");
    assert_eq!(result_of("0O17"), "15");
    assert_eq!(result_of("0x0"), "0");
    // 单字符边界：0xE = 14（十六进制含 e，避免与标识符混淆）
    assert_eq!(result_of("0xE"), "14");
    // 混合大小写位
    assert_eq!(result_of("0xAbCd"), "43981");
}

#[test]
fn radix_in_arithmetic() {
    assert_eq!(result_of("0xFF+1"), "256");
    assert_eq!(result_of("-0xFF"), "-255");
    assert_eq!(result_of("0xFF*0.5"), "127.5");
    assert_eq!(result_of("0x10-0x8"), "8");
    assert_eq!(result_of("0b11<<2"), "12");
    assert_eq!(result_of("0xFF%16"), "15");
    // 与常量/函数混合
    assert_eq!(result_of("log(0x100, 2)"), "8");
    assert_eq!(result_of("max(0xF, 0b1000)"), "15");
    // 聚合边界用进制字面量
    assert_eq!(result_of("sum(i, 0x1, 0b11, i)"), "6");
}

#[test]
fn radix_big_promotion() {
    // u64::MAX = 0xFFFFFFFFFFFFFFFF：超出 i64 → Big
    assert_eq!(result_of("0xFFFFFFFFFFFFFFFF"), "18446744073709551615");
    // 更长 run 仍是 Big
    assert_eq!(result_of("0xFFFFFFFFFFFFFFFFFF"), "4722366482869645213695");
    // 负号 + Big
    assert_eq!(result_of("-0xFFFFFFFFFFFFFFFF"), "-18446744073709551615");
    // 二进制/八进制长 run
    assert_eq!(result_of(&format!("0b{}", "1".repeat(64))), "18446744073709551615");
    assert_eq!(result_of("0o7777777777777777777777"), "73786976294838206463");
}

#[test]
fn radix_invalid_digits() {
    // run 内非法位 → 整 run 展示
    let err = evaluate("0xZZ").unwrap_err();
    assert_eq!(err.code.as_str(), "expression_error");
    assert_eq!(err.message, "Invalid number literal: 0xZZ");
    assert_eq!(
        evaluate("0b12").unwrap_err().message,
        "Invalid number literal: 0b12"
    );
    assert_eq!(
        evaluate("0o8").unwrap_err().message,
        "Invalid number literal: 0o8"
    );
    assert_eq!(
        evaluate("0x1G2").unwrap_err().message,
        "Invalid number literal: 0x1G2"
    );
    // 裸前缀
    assert_eq!(
        evaluate("0x").unwrap_err().message,
        "Invalid number literal: 0x"
    );
    assert_eq!(
        evaluate("0b").unwrap_err().message,
        "Invalid number literal: 0b"
    );
    // 前缀后跟运算符：0x+1 → run 为空 → 字面量错误（而非 0 + x）
    assert_eq!(
        evaluate("0x+1").unwrap_err().message,
        "Invalid number literal: 0x"
    );
}

#[test]
fn radix_step_and_billing() {
    // 进制字面量 = 1 个词法 token，与十进制同权；无步骤（原子）
    let out = evaluate("0xFF+1").unwrap();
    assert_eq!(out.steps.len(), 1);
    // 步骤 expr 走 format_num 渲染：字面量显示为十进制 255（现有约定，同常量折叠）
    assert_eq!(out.steps[0].expr, "255+1");
    assert_eq!(out.steps[0].operator, "+");
    assert_eq!(out.steps[0].state, "256");
    assert_eq!(out.prefix_display, "+ 255 1");
    // 计费 D2：prompt = 3；reasoning = 1；completion = 2；total = 5
    assert_eq!(out.usage.prompt_tokens, 3);
    assert_eq!(out.usage.reasoning_tokens, 1);
    assert_eq!(out.usage.completion_tokens, 2);
    assert_eq!(out.usage.total_tokens, 5);
    // 0x 前缀后紧贴中文数字续接仍然工作（255万）
    assert_eq!(result_of("0xFF万"), "2550000");
}

#[test]
fn radix_python_compatible_corners() {
    // 小写 b 与十六进制位 b 的边界：0b 后全为二进制位
    assert_eq!(result_of("0b101"), "5");
    // 0o 前缀与数字 0 的边界：单独 0 仍走十进制
    assert_eq!(result_of("0"), "0");
    assert_eq!(result_of("0123"), "123"); // 既有行为：前导零十进制（不改动）
    // 0x 后跟空白再数字：run 为空 → 报错（与 0x+1 同理）
    assert!(evaluate("0x 1").is_err());
}
