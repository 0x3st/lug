//! 差分测试（§6 golden corpus）：逐用例比对 Rust 引擎与 Python 原型。
//!
//! 语料由 `legacy/export_golden.py` 生成（Python 原型为可执行规格）：
//! - `cases.json`：成功用例比对 result/prefix/steps（逐字节）+ usage（按
//!   计费口径 D2 由导出器重算）；错误用例比对 message/code（逐字节）；
//! - `expected_diff.json`：§5 差异表已声明的 v1 预期行为（D1/D3），
//!   断言"与原型不同"且"与声明的 v1 期望一致"。
//!
//! 序列化对齐说明：语料中的 result 是 Python `json.dumps(format_num(v))`
//! 的字面量。Int/Big → 十进制数字串；Float → Python repr。二者分别与
//! Rust `Formatted::to_display()` 的输出一致（ryu 最短往返 == Python repr），
//! 因此字符串直接比对即可。

use lug_engine::{evaluate, value::Formatted};

fn display_of(f: &Formatted) -> String {
    f.to_display()
}

#[test]
fn differential_vs_python_prototype() {
    let cases: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/golden/cases.json"
        ))
        .expect("先运行 legacy/export_golden.py 生成语料"),
    )
    .unwrap();

    let mut pass = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for case in cases.as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        match evaluate(input) {
            Ok(out) => {
                if kind != "ok" {
                    failures.push(format!("{input:?}: 预期错误但成功"));
                    continue;
                }
                let fmt_result = lug_engine::value::format_num_value(&out.result);
                let result_lit = case["result"].as_str().unwrap();
                if display_of(&fmt_result) != result_lit {
                    failures.push(format!(
                        "{input:?}: result 不一致\n  原型: {result_lit}\n  rust: {}",
                        display_of(&fmt_result)
                    ));
                    continue;
                }
                if out.prefix_display != case["prefix"].as_str().unwrap() {
                    failures.push(format!(
                        "{input:?}: prefix 不一致\n  原型: {}\n  rust: {}",
                        case["prefix"].as_str().unwrap(),
                        out.prefix_display
                    ));
                    continue;
                }
                let golden_steps = case["steps"].as_array().unwrap();
                if golden_steps.len() != out.steps.len() {
                    failures.push(format!(
                        "{input:?}: 步骤数不一致 {} vs {}",
                        golden_steps.len(),
                        out.steps.len()
                    ));
                    continue;
                }
                let mut step_ok = true;
                for (golden, mine) in golden_steps.iter().zip(out.steps.iter()) {
                    let mine_result = lug_engine::value::format_num_value(&mine.result);
                    let fields_ok = golden["expr"].as_str() == Some(mine.expr.as_str())
                        && golden["operator"].as_str() == Some(mine.operator)
                        && golden["operator_code"].as_u64() == Some(mine.operator_code as u64)
                        && golden["result"].as_str() == Some(display_of(&mine_result).as_str())
                        && golden["state"].as_str() == Some(mine.state.as_str());
                    if !fields_ok {
                        failures.push(format!(
                            "{input:?}: step {} 不一致\n  原型: {}\n  rust: expr={} result={} state={}",
                            mine.step,
                            serde_json::to_string(golden).unwrap(),
                            mine.expr,
                            display_of(&mine_result),
                            mine.state
                        ));
                        step_ok = false;
                        break;
                    }
                }
                if !step_ok {
                    continue;
                }
                // 计费口径 D2（1 步 = 1 token）
                let usage = case["usage"].as_object().unwrap();
                let u_ok = usage["prompt_tokens"].as_u64() == Some(out.usage.prompt_tokens as u64)
                    && usage["completion_tokens"].as_u64()
                        == Some(out.usage.completion_tokens as u64)
                    && usage["total_tokens"].as_u64() == Some(out.usage.total_tokens as u64)
                    && usage["completion_tokens_details"]["reasoning_tokens"].as_u64()
                        == Some(out.usage.reasoning_tokens as u64);
                if !u_ok {
                    failures.push(format!("{input:?}: usage 不一致 rust={:?}", out.usage));
                    continue;
                }
                pass += 1;
            }
            Err(err) => {
                if kind != "error" {
                    failures.push(format!("{input:?}: 预期成功但错误: {err}"));
                    continue;
                }
                let msg_ok = err.message == case["message"].as_str().unwrap();
                let code_ok = err.code.as_str() == case["code"].as_str().unwrap();
                if !msg_ok || !code_ok {
                    failures.push(format!(
                        "{input:?}: 错误不一致\n  原型: {} [{}]\n  rust: {} [{}]",
                        case["message"].as_str().unwrap(),
                        case["code"].as_str().unwrap(),
                        err.message,
                        err.code.as_str()
                    ));
                    continue;
                }
                pass += 1;
            }
        }
    }
    println!("差分通过 {pass} / {}", cases.as_array().unwrap().len());
    assert!(
        failures.is_empty(),
        "差分失败 {} 项:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// 预期差异（D1/D3）：与原型不同、与声明的 v1 期望一致。
#[test]
fn expected_deviations() {
    let diffs: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/golden/expected_diff.json"
        ))
        .expect("先运行 legacy/export_golden.py 生成语料"),
    )
    .unwrap();
    for case in diffs.as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let v1 = &case["v1"];
        match (v1["kind"].as_str().unwrap(), evaluate(input)) {
            ("error", Ok(_)) => panic!("{input:?}: 预期差异为错误但成功"),
            ("error", Err(err)) => {
                assert_eq!(err.message, v1["message"].as_str().unwrap(), "{input:?}");
                assert_eq!(err.code.as_str(), v1["code"].as_str().unwrap(), "{input:?}");
            }
            ("ok", Ok(out)) => {
                let f = lug_engine::value::format_num_value(&out.result);
                assert_eq!(f.to_display(), v1["result"].as_str().unwrap(), "{input:?}");
            }
            ("ok", Err(err)) => panic!("{input:?}: 预期差异为成功但错误: {err}"),
            _ => unreachable!("未知的差异类别"),
        }
    }
}
