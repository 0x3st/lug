#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""差分语料导出器（§6 golden corpus）。

从 Python 原型（structured_ai_api.evaluate_full）批量运行语料，生成
tests/golden/cases.json——Rust 侧差分测试逐字段比对：
  - 成功用例：result / prefix / steps（expr/operator/operator_code/result/state）
    与原型逐字节一致；usage 按打费口径 D2（1 步 = 1 token）由本脚本重算
    （引擎行为不变、计费口径为 v1 定稿）。
  - 错误用例：message / code 逐字节一致。

预期差异（expected_diff.json，§5 差异表已声明）单独列出并写明 v1 期望：
  - D1：`x`/`X` 不再是乘号别名；
  - D3：非有限浮点不再写进 JSON（原型会输出 Infinity / complex 崩溃）。
排除项：包含 `x`/`X` 的其它用例（D1）、原型自身会崩溃/挂起的极端嵌套幂
（D4 护栏只存在于 v1，无原型对照）。
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import structured_ai_api as sai  # noqa: E402

# 成功用例：全运算符 × 优先级/结合性 × 括号/一元 × 浮点格式化边界 × 大数
OK_CASES = [
    "3+5*2**(1+1)", "1+2*3-4/2", "10-4-3", "100/10/5", "2**3**2", "2**2**3",
    "-2**2", "-2**-2", "2**-2", "--5", "+-+5", "-(-5)", "(1+2)*3", "( 1 + 2 ) * 3",
    "10/4", "10//4", "-7//2", "7//-2", "-7%-2", "7%-2", "-10%-3", "10%-3",
    "10%3", "10.5%3", "5.5%2", "-5.5%2", "5.5%-2", "0.5//0.25", "7.5//2",
    "1/3", "1/7", "22/7", "2/3*3", "10**12/3", "(10**12)/3", "0.1+0.2",
    "1.9999999999999998", "0.30000000000000004", "2.675", "1.005", "0.125",
    "3.5*2", "8.0/2", "(8.0)/2", "8.0*2", "8.0", "-0.0", "(8/2)*2", "2/3", "6×7", "8÷2", "2^10", "2**0.5",
    "2**-1", "2.0**-1", "(2**31)**-1", "0**0", "0.0**0", "1**0", "2**100", "10.0**20",
    "2.0**100", "10.0**-308", "9223372036854775807 + 1",
    "9223372036854775807 * 2", "-9223372036854775808",
    "123456789123456789123456789+1", "99999999999999999999*2",
    "123456789012345678901234567890 * 2", "100000000000000000000",
    "  7  ", "3 + 4 * 2", "((1+2))*3", "(1+2)*(3+4)", "1+(2*3)",
]

# 错误用例：错误信息/类别与原型逐字节一致
ERROR_CASES = [
    "(3+4", "((1+2)", "3+4)", "()", "3 4", "1 2 3", "3.5.2", "", "   ",
    "3 @ 4", "1/0", "1//0", "1%0", "1.5/0", "1/0.0", "1//0.0",
    "1%0.0", "0**-1", "0.0**-1", "0**-1.0", "2**100000", "2**-100000",
    "10.0**309", "*3", "3 +", "+", "(1+2))",
]

# 预期差异：v1 行为与原型不同（差异表 D1/D3），写明 v1 期望供 Rust 断言
EXPECTED_DIFF = [
    {
        "input": "2x3",
        "reason": "D1: x/X 不再是乘号别名；D5 标识符可含数字（x3 为一个标识符）",
        "v1": {"kind": "error",
               "message": "Unexpected tokens after the end of expression: x3",
               "code": "expression_error"},
    },
    {
        "input": "10.0**308",
        "reason": "D8: IR 数值保真修复原型双路分叉（原型 Internal consistency 崩溃，"
                  "v1 两路一致并给出 int(double(1e308)) 的精确展开）",
        "v1": {"kind": "ok", "result": json.dumps(int(10.0**308))},
    },
    {
        "input": "(10.0**308)*2",
        "reason": "D8: 同上（2 × 精确展开，618 位）",
        "v1": {"kind": "ok", "result": json.dumps(2 * int(10.0**308))},
    },
    {
        "input": "abc",
        "reason": "D5: 标识符合法化 → 未定义变量（原型为 Unrecognized character）",
        "v1": {"kind": "error",
               "message": "undefined variable 'abc'",
               "code": "expression_error"},
    },
    {
        "input": "(-8)**0.5",
        "reason": "D3: 负底数分数指数（原型返回 complex，json 序列化崩溃/500）",
        "v1": {"kind": "error",
               "message": "Step 1 failed: (-8)**0.5: Numeric overflow",
               "code": "computation_error"},
    },
]


def run_ok(expr):
    """运行成功用例，产出与 Rust 侧可比对的字段（全部为字符串/整数）。"""
    result, steps, _prefix_tokens, prefix_str = sai.evaluate_full(expr)
    # 计费口径 D2：prompt = 词法 token 数；reasoning = 步骤数；completion = +1
    prompt = len(sai.tokenize_expr(expr))
    reasoning = len(steps)
    usage = {
        "prompt_tokens": prompt,
        "completion_tokens": reasoning + 1,
        "total_tokens": prompt + reasoning + 1,
        "completion_tokens_details": {"reasoning_tokens": reasoning},
    }
    return {
        "input": expr,
        "kind": "ok",
        # json.dumps(格式化值)：把 JSON 字面量固化为字符串，供 Rust 逐字节比对
        "result": json.dumps(sai.format_num(result)),
        "prefix": prefix_str,
        "steps": [
            {
                "step": st["step"],
                "expr": st["expr"],
                "operator": st["operator"],
                "operator_code": st["operator_code"],
                "result": json.dumps(st["result"]),
                "state": st["state"],
            }
            for st in steps
        ],
        "usage": usage,
    }


def run_error(expr):
    try:
        sai.evaluate_full(expr)
    except sai.ExprError as exc:
        return {"input": expr, "kind": "error",
                "message": str(exc), "code": exc.code}
    raise AssertionError(f"预期错误但成功: {expr}")


def main():
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                           "..", "tests", "golden")
    os.makedirs(out_dir, exist_ok=True)

    cases = [run_ok(e) for e in OK_CASES] + [run_error(e) for e in ERROR_CASES]
    with open(os.path.join(out_dir, "cases.json"), "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=1)
    with open(os.path.join(out_dir, "expected_diff.json"), "w", encoding="utf-8") as f:
        json.dump(EXPECTED_DIFF, f, ensure_ascii=False, indent=1)
    print(f"语料导出完成: {len(cases)} 用例 + {len(EXPECTED_DIFF)} 预期差异")


if __name__ == "__main__":
    main()
