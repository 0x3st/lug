//! T9a S2：组合数学 gcd/lcm/C(n,k)/P(n,k)（v1 新增语义，错误信息自洽）。
//!
//! 语义决策（本切片锁定）：
//! - gcd/lcm 整数域，负数取 abs（不报错）；变参（≥2 个实参）；gcd(0,x)=x，
//!   gcd(0,0)=0，lcm(0,x)=0；Float 实参 → 类型错误；
//! - C/P 精确 Big 域；C(0,0)=1；k>n 或负数 → 域错误；Float 实参 → 类型错误；
//! - 迭代护栏：min(k, n−k) > 10000 → 参数过大（镜像 MAX_FACTORIAL_N），
//!   结果位数预算复用 D4（"Result would have N digits (budget: M)"）；
//! - 函数计步：每函数调用 1 个 Always 步（与 sqrt 同口径）。

use lug_engine::evaluate;

fn result_of(expr: &str) -> String {
    let out = evaluate(expr).unwrap();
    lug_engine::value::format_num_value(&out.result).to_display()
}

#[test]
fn gcd_basics() {
    assert_eq!(result_of("gcd(12, 18)"), "6");
    assert_eq!(result_of("gcd(0, 5)"), "5");
    assert_eq!(result_of("gcd(5, 0)"), "5");
    assert_eq!(result_of("gcd(0, 0)"), "0");
    assert_eq!(result_of("gcd(7, 13)"), "1");
    // 负数取 abs（决策：不报错）
    assert_eq!(result_of("gcd(-4, 6)"), "2");
    assert_eq!(result_of("gcd(4, -6)"), "2");
    assert_eq!(result_of("gcd(-4, -6)"), "2");
}

#[test]
fn gcd_variadic_and_big() {
    // 变参（决策）：gcd(a, b, c) = gcd(gcd(a, b), c)
    assert_eq!(result_of("gcd(12, 18, 24)"), "6");
    assert_eq!(result_of("gcd(100, 75, 50, 25)"), "25");
    // Big 域精确（与引擎内恒等式交叉验证）
    assert_eq!(result_of("gcd(2**100, 2**50)"), result_of("2**50"));
    assert_eq!(result_of("gcd(2**100 * 3, 2**50 * 5)"), result_of("2**50"));
}

#[test]
fn gcd_errors() {
    // 函数错误按现有约定包裹："Step N failed: {调用源}: {核错误}"
    let err = evaluate("gcd(5)").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert_eq!(
        err.message,
        "Step 1 failed: gcd(5): gcd() expects at least 2 arguments, got 1"
    );
    // Float 实参 → 类型错误
    let err = evaluate("gcd(2.5, 5)").unwrap_err();
    assert_eq!(
        err.message,
        "Step 1 failed: gcd(2.5, 5): gcd(): arguments must be integers"
    );
    let err = evaluate("gcd(6, 1.5)").unwrap_err();
    assert_eq!(
        err.message,
        "Step 1 failed: gcd(6, 1.5): gcd(): arguments must be integers"
    );
    assert!(evaluate("gcd(2, 3, 4.5)").is_err());
}

#[test]
fn lcm_basics() {
    assert_eq!(result_of("lcm(4, 6)"), "12");
    assert_eq!(result_of("lcm(0, 5)"), "0");
    assert_eq!(result_of("lcm(7, 13)"), "91");
    // 负数取 abs（与 gcd 同决策）
    assert_eq!(result_of("lcm(-4, 6)"), "12");
    assert_eq!(result_of("lcm(4, -6)"), "12");
    // Big：不先算 a*b 再除（避免中间量爆炸），走 a/gcd*b
    assert_eq!(result_of("lcm(2**60, 2**59 * 3)"), result_of("3 * 2**60"));
}

#[test]
fn lcm_errors() {
    assert_eq!(
        evaluate("lcm(3)").unwrap_err().message,
        "Step 1 failed: lcm(3): lcm() expects at least 2 arguments, got 1"
    );
    assert_eq!(
        evaluate("lcm(1.5, 2)").unwrap_err().message,
        "Step 1 failed: lcm(1.5, 2): lcm(): arguments must be integers"
    );
}

#[test]
fn comb_values() {
    assert_eq!(result_of("C(5, 2)"), "10");
    assert_eq!(result_of("C(10, 3)"), "120");
    assert_eq!(result_of("C(0, 0)"), "1");
    assert_eq!(result_of("C(5, 0)"), "1");
    assert_eq!(result_of("C(5, 5)"), "1");
    // 大数精确（Big 域）：C(100, 50)
    assert_eq!(
        result_of("C(100, 50)"),
        "100891344545564193334812497256"
    );
    // 对称性：C(n, k) = C(n, n-k)
    assert_eq!(result_of("C(52, 5)"), result_of("C(52, 47)"));
    // Big n 支持：n 无上限（迭代数 = min(k, n-k)），与 floor 除恒等式交叉验证
    assert_eq!(result_of("C(2**64, 2)"), result_of("(2**64)*(2**64-1)//2"));
    // 恒等式交叉验证：C(n,1) = n；C(100,50) = 100!/(50!)²（用 // 整除保精确：
    // 本引擎 `/` 是真除法恒出浮点，大数会掉精度）
    assert_eq!(result_of("C(123456789, 1)"), "123456789");
    assert_eq!(result_of("C(100, 50)"), result_of("100!//(50!*50!)"));
}

#[test]
fn comb_domain_errors() {
    // k > n（函数错误按现有约定包裹）
    let err = evaluate("C(5, 6)").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert_eq!(
        err.message,
        "Step 1 failed: C(5, 6): C(n, k): k must not exceed n"
    );
    // 负数 → 域错误
    assert_eq!(
        evaluate("C(-1, 0)").unwrap_err().message,
        "Step 1 failed: C(-1, 0): C(n, k): arguments must be non-negative integers"
    );
    assert_eq!(
        evaluate("C(5, -2)").unwrap_err().message,
        "Step 1 failed: C(5, -2): C(n, k): arguments must be non-negative integers"
    );
    // Float 实参
    assert_eq!(
        evaluate("C(5.5, 2)").unwrap_err().message,
        "Step 1 failed: C(5.5, 2): C(n, k): arguments must be integers"
    );
    assert_eq!(
        evaluate("C(5, 2.5)").unwrap_err().message,
        "Step 1 failed: C(5, 2.5): C(n, k): arguments must be integers"
    );
}

#[test]
fn perm_values() {
    assert_eq!(result_of("P(5, 2)"), "20");
    assert_eq!(result_of("P(10, 3)"), "720");
    assert_eq!(result_of("P(5, 0)"), "1");
    assert_eq!(result_of("P(5, 5)"), "120");
    assert_eq!(result_of("P(0, 0)"), "1");
    // 与阶乘恒等：P(n, n) = n!
    assert_eq!(result_of("P(10, 10)"), result_of("10!"));
    // Big n，与乘积恒等式交叉验证
    assert_eq!(result_of("P(2**64, 2)"), result_of("(2**64)*(2**64-1)"));
}

#[test]
fn perm_domain_errors() {
    assert_eq!(
        evaluate("P(5, 6)").unwrap_err().message,
        "Step 1 failed: P(5, 6): P(n, k): k must not exceed n"
    );
    assert_eq!(
        evaluate("P(-1, 2)").unwrap_err().message,
        "Step 1 failed: P(-1, 2): P(n, k): arguments must be non-negative integers"
    );
    assert_eq!(
        evaluate("P(5, -1)").unwrap_err().message,
        "Step 1 failed: P(5, -1): P(n, k): arguments must be non-negative integers"
    );
    assert_eq!(
        evaluate("P(2.5, 1)").unwrap_err().message,
        "Step 1 failed: P(2.5, 1): P(n, k): arguments must be integers"
    );
}

#[test]
fn combinatorial_guards() {
    // 迭代护栏（镜像 MAX_FACTORIAL_N）：min(k, n-k) > 10000
    // （10**10 / 10**5 各占一步，C 报错在第 3 步）
    let err = evaluate("C(10**10, 10**5)").unwrap_err();
    assert_eq!(err.code.as_str(), "computation_error");
    assert_eq!(
        err.message,
        "Step 3 failed: C(10000000000, 100000): C(n, k): argument too large (max 10000)"
    );
    // P 迭代数 = k
    assert_eq!(
        evaluate("P(10**10, 10**5)").unwrap_err().message,
        "Step 3 failed: P(10000000000, 100000): P(n, k): argument too large (max 10000)"
    );
    // 位数预算（D4 家族）：估计位数超预算 → 前置拦截，结果不物化
    let err = evaluate("C(10**1000, 10000)").unwrap_err();
    assert!(err.message.contains("digits (budget: 10000000)"), "{err}");
}

#[test]
fn combinatorial_steps_and_billing() {
    // 每函数调用 1 个 Always 步（与 sqrt 同口径）；
    // Call 步的 expr = 替换后的 span 渲染（现有约定：展示计算后状态）
    let out = evaluate("gcd(12, 18)").unwrap();
    assert_eq!(out.steps.len(), 1);
    let s = &out.steps[0];
    assert_eq!(s.expr, "6");
    assert_eq!(s.operator, "gcd");
    assert_eq!(s.operator_code, 48);
    assert_eq!(
        lug_engine::value::format_num_value(&s.result).to_display(),
        "6"
    );
    assert_eq!(s.state, "6");
    assert_eq!(out.prefix_display, "gcd 12 18");
    // 计费 D2：prompt = 6 词法 token；reasoning = 1；completion = 2；total = 8
    assert_eq!(out.usage.prompt_tokens, 6);
    assert_eq!(out.usage.reasoning_tokens, 1);
    assert_eq!(out.usage.completion_tokens, 2);
    assert_eq!(out.usage.total_tokens, 8);
    // 函数套娃：C(5, 2)! 与 gcd(gcd(12, 18), 8)
    assert_eq!(result_of("C(5, 2)!"), "3628800");
    assert_eq!(result_of("gcd(gcd(12, 18), 8)"), "2");
    // sum body 中可用
    assert_eq!(result_of("sum(i, 1, 3, C(i, 1))"), "6");
}

#[test]
fn combinatorial_real_ids() {
    // 双路校验一致性：组合数与阶乘恒等式
    // C(10, 3) = 10! / (3! * 7!)
    assert_eq!(
        result_of("C(10, 3)"),
        result_of("10!/(3!*7!)")
    );
    // P(10, 3) = 10!/7!
    assert_eq!(result_of("P(10, 3)"), result_of("10!/7!"));
    // lcm(4,6) * gcd(4,6) = 4*6
    assert_eq!(result_of("lcm(4, 6)*gcd(4, 6)"), "24");
}
