//! L1 语义规则层测试：算子词别名 + 中文数字 + 数字续接。
//! 语义输入是 v1-only 能力（Python 原型无法解析），故不入差分语料，独立断言。

use lug_engine::evaluate;

fn result_of(expr: &str) -> String {
    let out = evaluate(expr).unwrap();
    lug_engine::value::format_num_value(&out.result).to_display()
}

#[test]
fn word_aliases_operate() {
    for (expr, expect) in [
        ("3 加 5", "8"),
        ("3加5", "8"),
        ("2乘以3", "6"),
        ("10整除3", "3"),
        ("7 取余 2", "1"),
        ("3 plus 4", "7"),
        ("10 minus 3", "7"),
        ("2 幂 3", "8"),
        ("9乘8", "72"),
        ("2 乘方 3", "8"),
    ] {
        assert_eq!(result_of(expr), expect, "{expr}");
    }
}

#[test]
fn chinese_numerals() {
    for (expr, expect) in [
        ("三十五", "35"),
        ("一百零五", "105"),
        ("十", "10"),
        ("十五", "15"),
        ("二十三", "23"),
        ("一万零一", "10001"),
        ("十万", "100000"),
        ("二亿三千万", "230000000"),
        ("一百二十三万四千", "1234000"),
        ("两千万", "20000000"),
        ("零", "0"),
        ("负一百", "-100"),
    ] {
        assert_eq!(result_of(expr), expect, "{expr}");
    }
}

#[test]
fn chinese_decimal_numerals() {
    assert_eq!(result_of("两点五"), "2.5");
    assert_eq!(result_of("三点一四"), "3.14");
    assert_eq!(result_of("零点五"), "0.5");
    assert_eq!(result_of("三点五加一"), "4.5");
}

#[test]
fn ascii_digit_continuation() {
    // 紧贴续接：ASCII 整数后直接跟 万/亿/千
    for (expr, expect) in [
        ("3万", "30000"),
        ("3万5千", "35000"),
        ("2亿3千万", "230000000"),
        ("5千", "5000"),
        ("3万5千+1", "35001"),
    ] {
        assert_eq!(result_of(expr), expect, "{expr}");
    }
    // 有空格不算续接（万 落入未识别路径，与孤立 万 一致）
    assert!(evaluate("3 万").is_err());
}

#[test]
fn colloquial_full_pipeline() {
    // 口语输入走完整流水线：解析 → 步骤流 → 双路校验 → 计量
    let out = evaluate("三加五乘二").unwrap();
    assert_eq!(
        lug_engine::value::format_num_value(&out.result).to_display(),
        "13"
    );
    assert_eq!(out.prefix_display, "+ 3 * 5 2");
    assert_eq!(out.steps.len(), 2);
    assert_eq!(out.steps[0].expr, "5*2");
    // 计量（D2）：词法 token 数 = 5（三/+/五/*/二）
    assert_eq!(out.usage.prompt_tokens, 5);
    assert_eq!(out.usage.reasoning_tokens, 2);
    assert_eq!(out.usage.total_tokens, 8);
}

#[test]
fn unknown_words_still_rejected() {
    // D5：字母序列现为标识符 → 未定义变量（差分语料 expected_diff 已对齐）
    let err = evaluate("abc").unwrap_err();
    assert_eq!(err.message, "undefined variable 'abc'");
    assert_eq!(err.code.as_str(), "expression_error");
    // 非 ASCII 字母的汉字串仍走未识别路径
    assert!(evaluate("你好").is_err());
    // D1：x 依旧不是乘号（解析为多余标识符 token）
    assert!(evaluate("2x3").is_err());
}

#[test]
fn mixed_chinese_and_ascii() {
    // 中文数字与 ASCII 混合：规范语义一致
    assert_eq!(result_of("一百 + 1"), "101");
    assert_eq!(result_of("3 + 四"), "7");
    assert_eq!(result_of("两点五 * 2"), "5");
}

#[test]
fn function_calls_and_aggregates() {
    // L2：函数/常量/阶乘/聚合（逗号分隔符是词法关键路径）
    for (expr, expect) in [
        ("log(8, 2)", "3"),
        ("sqrt(2)", "1.414213562373"),
        ("sqrt(4)", "2"),
        ("5!", "120"),
        ("sum(i, 1, 10, i**2)", "385"),
        ("prod(i, 1, 4, i)", "24"),
        ("max(三加五, pi*2)", "8"),
        ("min(3, 1, 2)", "1"),
        ("abs(负一百)", "100"),
        ("round(2.5)", "3"),
        ("floor(2.9)", "2"),
        ("pi", "3.14159265359"),
        ("e", "2.718281828459"),
        ("2**10", "1024"),
    ] {
        assert_eq!(result_of(expr), expect, "{expr}");
    }
}

#[test]
fn aggregate_is_opaque_single_step() {
    // sum = Opaque 单步：body 不展开，计费可预期
    let out = evaluate("sum(i, 1, 10, i**2)").unwrap();
    assert_eq!(out.steps.len(), 1);
    // expr 为 span 渲染：385 = format_num 后的 state/结果形态（Opaque 步骤展示计算后状态）
    assert_eq!(out.steps[0].operator, "sum");
    assert_eq!(
        lug_engine::value::format_num_value(&out.steps[0].result).to_display(),
        "385"
    );
    // prompt = 词法 token 数（sum ( i , 1 , 10 , i ** 2 ) = 12）
    assert_eq!(out.usage.prompt_tokens, 12);
}

#[test]
fn aggregate_guards() {
    // 迭代上限 / 边界类型 / 空聚合
    let err = evaluate("sum(i, 1, 1000002, i)").unwrap_err(); // span = 1000001 > 上限
    assert!(err.message.contains("range too large"), "{err}");
    assert!(evaluate("sum(i, 1, 0, i)").is_ok()); // 空聚合 = 0
    assert!(evaluate("sum(x, 1, 10, y)").is_err()); // 自由变量 y 未绑定 → 报错
    assert!(evaluate("sqrt(-1)").is_err()); // 域错误
}

#[test]
fn nested_aggregates() {
    // 嵌套：sum(i,1,3,sum(j,1,i,j)) = 1 + (1+2) + (1+2+3) = 10
    assert_eq!(result_of("sum(i, 1, 3, sum(j, 1, i, j))"), "10");
    // Opaque 语义：外层单步，内层（含嵌套聚合）全部静默
    let out = evaluate("sum(i, 1, 3, sum(j, 1, i, j))").unwrap();
    assert_eq!(out.steps.len(), 1);
    assert_eq!(out.steps[0].operator, "sum");
}

#[test]
fn arbitrary_nesting_matrix() {
    // 套娃全组合（§10.6 Env 栈 + 递归子 IR 的验收矩阵）
    for (expr, expect) in [
        ("sqrt(log(8, 2))", "1.732050807569"),       // 函数套函数
        ("sum(i, 1, 10, sin(i))", "1.411188371219"), // 函数在聚合 body
        ("max(sum(i,1,10,i**2), prod(i,1,5,i), sqrt(2)*10)", "385"), // 聚合在函数实参
        ("3!!", "720"),                              // 阶乘链（Big 中转）
        ("4!!", "620448401733239439360000"),         // 24! = Big
        ("sqrt(4)!", "2"),                           // 函数结果阶乘
        ("sum(i, 1, 5, i)!", "1307674368000"),       // 聚合结果阶乘
        ("(2**3)!", "40320"),                        // 幂结果阶乘
        ("sum(i, 1, 3, sum(i, 1, 2, i*10))", "90"),  // 内层变量遮蔽外层
        ("log(sum(i,1,9,i), 2)", "5.49185309633"),   // 聚合作函数实参
        ("((((1+2))))", "3"),                        // 括号套娃
    ] {
        assert_eq!(result_of(expr), expect, "{expr}");
    }
    // 深度压力：200 层括号（表达式长度上限内）
    let deep = format!("{}1+2{}", "(".repeat(200), ")".repeat(200));
    assert_eq!(result_of(&deep), "3");
}
