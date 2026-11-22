//! 函数与聚合的求值核（Function/Postfix/Special 共用，tree 与 prefix 两条
//! 求值路径都汇入这里——单一实现，双路校验对特殊形式退化为"同核两次调用"，
//! 核的正确性由单元/属性测试保证，§3.5）。
//!
//! 语义口径（◇ 新运算，与 Python 的差异已文档化）：
//! - 函数走 f64 路径，结果统一过 format_num（整数化，如 sqrt(4) → 2）；
//! - 非有限结果 → "Numeric overflow"（D3 家族）；
//! - `round` 为远离零舍入（Rust f64::round），非 Python 的银行家舍入；
//! - 阶乘仅接受非负整数，参数上限 10000（超出走 Big 的组合交给 `**`）；
//! - sum/prod 边界须为整数，迭代上限 1,000,000；`to < from` 为空聚合
//!   （sum=0，prod=1，与 Python range 语义一致）。

use crate::registry;
use crate::value::{self, Ctx, Value};
use num_bigint::BigInt;
use num_traits::{One, Signed, ToPrimitive, Zero};

/// 聚合迭代上限（§3.1 费用护栏：费用随输入而非循环上界增长的前提）。
pub const MAX_AGGREGATE_ITERATIONS: i64 = 1_000_000;

/// 阶乘参数上限（n ≤ 10000 → 结果 ≤ 35660 位，预算内且耗时亚秒）。
pub const MAX_FACTORIAL_N: u64 = 10_000;

/// 函数应用统一入口：arity 校验 → 求值核 → format_num（与全引擎一致）。
/// 可调用符号（后缀/函数/特殊形式）经 callable_by_symbol 查找——`%` 的
/// 首个注册项是二元取模，函数侧必须命中百分号后缀定义（T9a S1）。
pub fn apply_function(symbol: &str, args: &[Value], ctx: &Ctx) -> Result<Value, String> {
    let def = registry::callable_by_symbol(symbol)
        .ok_or_else(|| format!("Unknown function '{symbol}'"))?;
    let arity = def
        .arity
        .ok_or_else(|| format!("'{symbol}' is not a function"))?;
    if !arity.contains(args.len()) {
        return Err(format!(
            "{}() expects {}, got {}",
            symbol,
            arity.describe(),
            args.len()
        ));
    }
    let raw = apply_raw(symbol, args, ctx)?;
    let f = value::format_num_value(&raw);
    Ok(match f {
        value::Formatted::Int(v) => Value::Int(v),
        value::Formatted::Big(b) => Value::Big(b),
        value::Formatted::Float(x) => Value::Float(x),
    })
}

fn finite(x: f64) -> Result<Value, String> {
    if x.is_finite() {
        Ok(Value::Float(x))
    } else {
        Err("Numeric overflow".to_string())
    }
}

/// 单参函数的 f64 取值（Big 超 f64 域 → 溢出错误）。
fn as_f64(v: &Value) -> Result<f64, String> {
    v.to_f64().ok_or_else(|| "Numeric overflow".to_string())
}

fn apply_raw(symbol: &str, args: &[Value], ctx: &Ctx) -> Result<Value, String> {
    match symbol {
        "!" => factorial(&args[0], ctx),
        "%" => Ok(match &args[0] {
            // 百分号后缀：除以 100，恒出 Float（整数化展示由 format_num 统一处理）
            Value::Int(v) => Value::Float(*v as f64 / 100.0),
            Value::Big(b) => match b.to_f64() {
                Some(f) => Value::Float(f / 100.0),
                None => return Err("Numeric overflow".to_string()),
            },
            Value::Float(f) => Value::Float(f / 100.0),
        }),
        "abs" => Ok(match &args[0] {
            Value::Int(v) => Value::Int(v.abs()),
            Value::Big(b) => Value::Big(b.clone().abs()),
            Value::Float(f) => Value::Float(f.abs()),
        }),
        "floor" => match &args[0] {
            Value::Float(f) => finite(f.floor()),
            v => Ok(v.clone()),
        },
        "ceil" => match &args[0] {
            Value::Float(f) => finite(f.ceil()),
            v => Ok(v.clone()),
        },
        "round" => match &args[0] {
            // 远离零舍入（◇ 新语义；Python round 为银行家舍入，用户适应项）
            Value::Float(f) => finite(f.round()),
            v => Ok(v.clone()),
        },
        "min" | "max" => {
            let mut best = args[0].clone();
            for arg in &args[1..] {
                let ord = cmp_values(arg, &best);
                let take = if symbol == "min" {
                    ord == std::cmp::Ordering::Less
                } else {
                    ord == std::cmp::Ordering::Greater
                };
                if take {
                    best = arg.clone();
                }
            }
            Ok(best)
        }
        "sqrt" => {
            let x = as_f64(&args[0])?;
            if x < 0.0 {
                return Err("sqrt(x): x must be non-negative".to_string());
            }
            finite(x.sqrt())
        }
        "cbrt" => finite(as_f64(&args[0])?.cbrt()),
        "exp" => finite(as_f64(&args[0])?.exp()),
        "ln" => {
            let x = as_f64(&args[0])?;
            if x <= 0.0 {
                return Err("ln(x): x must be positive".to_string());
            }
            finite(x.ln())
        }
        "log2" => {
            let x = as_f64(&args[0])?;
            if x <= 0.0 {
                return Err("log2(x): x must be positive".to_string());
            }
            finite(x.log2())
        }
        "log10" => {
            let x = as_f64(&args[0])?;
            if x <= 0.0 {
                return Err("log10(x): x must be positive".to_string());
            }
            finite(x.log10())
        }
        "log" => {
            let x = as_f64(&args[0])?;
            if x <= 0.0 {
                return Err("log(x, b): x must be positive".to_string());
            }
            if args.len() == 1 {
                return finite(x.ln());
            }
            let b = as_f64(&args[1])?;
            if b <= 0.0 || b == 1.0 {
                return Err("log(x, b): base must be positive and not 1".to_string());
            }
            finite(x.ln() / b.ln())
        }
        "sin" => finite(as_f64(&args[0])?.sin()),
        "cos" => finite(as_f64(&args[0])?.cos()),
        "tan" => finite(as_f64(&args[0])?.tan()),
        "gcd" | "lcm" => {
            let sig = format!("{symbol}()");
            for arg in args {
                int_arg(arg, &sig)?;
            }
            Ok(gcd_lcm(symbol, args)?)
        }
        "C" | "P" => {
            // 类型错误信息带签名：C(n, k) / P(n, k)（与域错误前缀一致）
            let sig = format!("{symbol}(n, k)");
            let n = int_arg(&args[0], &sig)?;
            let k = int_arg(&args[1], &sig)?;
            Ok(combinatorial(symbol, &n, &k, ctx)?)
        }
        _ => Err(format!("Unknown function '{symbol}'")),
    }
}

/// 整数实参提取（gcd/lcm/C/P 共用）；Float → 类型错误（v1 新增语义）。
/// `sig` 为完整签名前缀（如 "gcd()"、"C(n, k)"）。
fn int_arg(v: &Value, sig: &str) -> Result<BigInt, String> {
    match v {
        Value::Int(i) => Ok(BigInt::from(*i)),
        Value::Big(b) => Ok(b.clone()),
        Value::Float(_) => Err(format!("{sig}: arguments must be integers")),
    }
}

/// 欧几里得辗转相除（|a|、|b| 非负）。
fn big_gcd(a: &BigInt, b: &BigInt) -> BigInt {
    let (mut a, mut b) = (a.abs(), b.abs());
    while !b.is_zero() {
        let r = &a % &b;
        a = std::mem::replace(&mut b, r);
    }
    a
}

/// gcd/lcm：整数域，负数取 abs；变参折叠；gcd(0,x)=x，gcd(0,0)=0，lcm(0,x)=0。
fn gcd_lcm(symbol: &str, args: &[Value]) -> Result<Value, String> {
    // 单位元：gcd 折叠从 0 起（gcd(0,x)=|x|）；lcm 折叠从 1 起（lcm(1,x)=|x|，
    // lcm(0,x)=0 由折叠自然得出）
    let mut acc = if symbol == "gcd" {
        BigInt::zero()
    } else {
        BigInt::one()
    };
    for arg in args {
        let b = int_arg(arg, symbol)?.abs();
        if symbol == "gcd" {
            acc = big_gcd(&acc, &b);
        } else {
            // lcm(a, b) = |a|/gcd(|a|,|b|) × |b|（先除后乘，中间量不爆炸）
            let g = big_gcd(&acc, &b);
            acc = if g.is_zero() {
                BigInt::zero()
            } else {
                acc / g * b
            };
        }
    }
    Ok(Value::Big(acc))
}

/// 组合数/排列数：精确 Big 域；域错误前置；护栏 = 迭代上限（镜像阶乘 10000）
/// + 位数预算预估（D4 家族，镜像幂的预估先拦）。
fn combinatorial(who: &str, n: &BigInt, k: &BigInt, ctx: &Ctx) -> Result<Value, String> {
    use num_bigint::Sign;
    if n.sign() == Sign::Minus || k.sign() == Sign::Minus {
        return Err(format!("{who}(n, k): arguments must be non-negative integers"));
    }
    if k > n {
        return Err(format!("{who}(n, k): k must not exceed n"));
    }
    // 迭代数：C 取 k_eff = min(k, n−k)（对称性，等价改算 C(n, k_eff)）；P 取 k
    let k_eff = if who == "C" {
        let nk = n - k;
        if nk < *k { nk } else { k.clone() }
    } else {
        k.clone()
    };
    let iters = k_eff
        .to_u64()
        .ok_or_else(|| format!("{who}(n, k): argument too large (max {MAX_FACTORIAL_N})"))?;
    if iters > MAX_FACTORIAL_N {
        return Err(format!("{who}(n, k): argument too large (max {MAX_FACTORIAL_N})"));
    }
    // 位数预估：C(n,k) ≤ n^iters → digits ≤ iters × log10(n) + 1（上界，只多不少）
    if let Some(nf) = n.to_f64() {
        if nf > 0.0 {
            // inf/超界情形：as usize 饱和到 usize::MAX，saturating_add 防 +1 溢出
            let est = ((iters as f64 * nf.log10()).ceil() as usize).saturating_add(1);
            if est > ctx.max_digits {
                return Err(format!(
                    "Result would have {est} digits (budget: {})",
                    ctx.max_digits
                ));
            }
        }
    }
    let mut r = BigInt::one();
    if who == "C" {
        let base = n - k_eff;
        for i in 1..=iters {
            // 乘 (n−k_eff+i) 后除 i 恒整除（部分积 = C(n−k_eff+i, i)）
            r = r * (base.clone() + i) / i;
        }
    } else {
        for i in 0..iters {
            r *= n.clone() - i;
        }
    }
    let digits = r.to_string().len();
    if digits > ctx.max_digits {
        return Err(format!(
            "Result would have {digits} digits (budget: {})",
            ctx.max_digits
        ));
    }
    Ok(Value::Big(r))
}

/// 阶乘：仅非负整数；Big 参数必然超上限（2^63 ≫ 10000）。
fn factorial(n: &Value, ctx: &Ctx) -> Result<Value, String> {
    let reject = || "Factorial requires a non-negative integer".to_string();
    // Int/Big 皆可（3! 的结果经 format_num 可能是 Big(6)，套娃 3!! 必须工作）
    let u: u64 = match n {
        Value::Int(v) if *v >= 0 => *v as u64,
        Value::Big(b) => match (b.sign() != num_bigint::Sign::Minus, b.to_u64()) {
            (true, Some(u)) => u,
            _ => return Err(reject()),
        },
        _ => return Err(reject()),
    };
    if u > MAX_FACTORIAL_N {
        return Err(format!(
            "Factorial argument too large (max {MAX_FACTORIAL_N})"
        ));
    }
    let mut acc = BigInt::one();
    for k in 1..=u {
        acc *= BigInt::from(k);
    }
    // 预算复核（10000! ≈ 35660 位，默认预算内）
    let digits = acc.to_string().len();
    if digits > ctx.max_digits {
        return Err(format!(
            "Result would have {} digits (budget: {})",
            digits, ctx.max_digits
        ));
    }
    Ok(Value::Big(acc))
}

/// 聚合初值：sum → 0，prod → 1。
pub fn aggregate_start(symbol: &str) -> Value {
    if symbol == "prod" {
        Value::Int(1)
    } else {
        Value::Int(0)
    }
}

/// 聚合边界：须为整数，|to − from| ≤ 迭代上限；返回 (from, to)。
pub fn aggregate_bounds(from: &Value, to: &Value, symbol: &str) -> Result<(i64, i64), String> {
    let err_too_large = || {
        format!(
            "{} range too large (max {MAX_AGGREGATE_ITERATIONS} iterations)",
            if symbol == "prod" { "prod" } else { "sum" }
        )
    };
    let f = match from {
        Value::Int(v) => *v,
        _ => return Err("Aggregate bounds must be integers".to_string()),
    };
    let t = match to {
        Value::Int(v) => *v,
        _ => return Err("Aggregate bounds must be integers".to_string()),
    };
    let span = t.checked_sub(f).ok_or_else(err_too_large)?;
    if span.unsigned_abs() > MAX_AGGREGATE_ITERATIONS as u64 {
        return Err(err_too_large());
    }
    Ok((f, t))
}

/// 聚合累加一步：sum → acc + item；prod → acc × item（走统一 compute 语义）。
pub fn aggregate_acc(symbol: &str, acc: &Value, item: &Value, ctx: &Ctx) -> Result<Value, String> {
    if symbol == "prod" {
        value::compute(acc, crate::value::BinOp::Mul, item, ctx)
    } else {
        value::compute(acc, crate::value::BinOp::Add, item, ctx)
    }
}

/// 跨类型数值比较（min/max 用）：Int/Big 精确比较；Big 超 f64 域时按符号
/// 裁决（正大数 > 任何有限浮点，负大数 < 任何有限浮点）。
pub fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let both_integral = !a.is_float() && !b.is_float();
    if both_integral {
        return (a.to_big()).cmp(&b.to_big());
    }
    // 一方是超出 f64 的 Big（|x| > f64::MAX）：按符号裁决——
    // 正大数 > 任何有限浮点，负大数 < 任何有限浮点
    match (a.to_f64(), b.to_f64()) {
        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        (None, Some(_)) => {
            let big = a.to_big();
            if big > BigInt::zero() {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(_), None) => cmp_values(b, a).reverse(),
        (None, None) => a.to_big().cmp(&b.to_big()),
    }
}
