//! 数值类型与运算语义。
//!
//! `Value` 三态表示（§3.1 决策）：`Int(i64)` 快路径、`Big(BigInt)` 溢出自动晋升
//! （C 档）、`Float(f64)`。运算语义与 Python 原型逐字节对齐（floor 除、模随除
//! 数符号、幂的指数上限等），差异仅限设计文档 §5 差异表中的 D3（非有限浮点
//! 报错）与 D4（运算预算护栏）。
//!
//! 所有二元运算经 [`compute`] 入口，错误信息字符串与 Python 原型逐字节一致
//! （golden 语料仲裁）。

use num_bigint::BigInt;
use num_traits::{FromPrimitive, One, ToPrimitive, Zero};
use std::fmt;

/// 默认单值位数预算（§10.7）：10^7 位 ≈ 10MB/值，可用 `LUG_MAX_VALUE_DIGITS` 覆盖。
pub const DEFAULT_MAX_DIGITS: usize = 10_000_000;

/// 求值上下文：目前仅携带运算预算（§3.1 值位数护栏）。
#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub max_digits: usize,
}

impl Default for Ctx {
    fn default() -> Self {
        Self {
            max_digits: DEFAULT_MAX_DIGITS,
        }
    }
}

/// 数值三态。
#[derive(Clone, Debug)]
pub enum Value {
    /// 快路径：绝大多数运算零堆分配。
    Int(i64),
    /// i64 溢出时自动晋升（C 档决策）。
    Big(BigInt),
    /// IEEE 754 双精度；非有限结果在运算层即报错（D3），不会流出到此。
    Float(f64),
}

/// 二元运算符（注册表符号的内部表示）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    /// T9a S4 位运算（仅整数域；Python 补码语义）。
    BitAnd,
    BitOr,
    BitShl,
    BitShr,
}

impl BinOp {
    /// 注册表符号（与 Python 原型 OPERATORS 键一致）。
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Mod => "%",
            BinOp::Pow => "**",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitShl => "<<",
            BinOp::BitShr => ">>",
        }
    }
}

/// 位运算类型错误（v1 新增语义，S4）。
const BITWISE_TYPE_ERR: &str = "Bitwise operations require integer operands";

/// 浮点收尾的便捷包装：非有限 → "Numeric overflow"（D3）。
fn finite(f: f64) -> Result<Value, String> {
    if f.is_finite() {
        Ok(Value::Float(f))
    } else {
        Err("Numeric overflow".to_string())
    }
}

/// 双操作数转 f64；Big 超出 f64 表示域时返回溢出错误
/// （对应 Python `int too large to convert to float` → v4 "Numeric overflow"）。
fn both_f64(a: &Value, b: &Value) -> Result<(f64, f64), String> {
    let err = || "Numeric overflow".to_string();
    Ok((a.to_f64().ok_or_else(err)?, b.to_f64().ok_or_else(err)?))
}

impl Value {
    pub fn is_zero(&self) -> bool {
        match self {
            Value::Int(v) => *v == 0,
            Value::Big(b) => b.is_zero(),
            Value::Float(f) => *f == 0.0, // 覆盖 -0.0，与 Python 1.0/(-0.0) 抛零除一致
        }
    }

    /// |self| > n（用于幂指数上限检查；i64::MIN 取 abs 需走无符号路径）。
    fn abs_gt_i64(&self, n: i64) -> bool {
        match self {
            Value::Int(v) => v.unsigned_abs() > n.unsigned_abs(),
            Value::Big(b) => b > &BigInt::from(n) || b < &BigInt::from(-n),
            Value::Float(f) => f.abs() > n as f64,
        }
    }

    pub fn to_big(&self) -> BigInt {
        match self {
            Value::Int(v) => BigInt::from(*v),
            Value::Big(b) => b.clone(),
            Value::Float(f) => BigInt::from_f64(*f).unwrap_or_default(),
        }
    }

    /// 转 f64；Big 超出 f64 表示域时返回 None（对应 Python OverflowError）。
    pub fn to_f64(&self) -> Option<f64> {
        match self {
            Value::Int(v) => Some(*v as f64),
            Value::Big(b) => b.to_f64(),
            Value::Float(f) => Some(*f),
        }
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Value::Float(_))
    }

    /// 任一操作数为 Float 即走浮点路径（与 Python 数值塔一致）。
    fn either_float(a: &Value, b: &Value) -> bool {
        a.is_float() || b.is_float()
    }
}

/// 位数估算：BigInt 的十进制位数（操作数已物化，代价可接受）。
fn big_digits(b: &BigInt) -> usize {
    b.to_string().len()
}

fn check_budget_big(b: &BigInt, ctx: &Ctx) -> Result<(), String> {
    let d = big_digits(b);
    if d > ctx.max_digits {
        Err(format!(
            "Result would have {} digits (budget: {})",
            d, ctx.max_digits
        ))
    } else {
        Ok(())
    }
}

/// 幂运算前的位数预估（§3.1：先按位数估算再计算，零成本拦截嵌套幂）。
fn check_budget_pow(base: &Value, exp: u32, ctx: &Ctx) -> Result<(), String> {
    let base_digits = match base {
        Value::Int(v) => v.unsigned_abs().to_string().len(),
        Value::Big(b) => big_digits(b),
        Value::Float(_) => return Ok(()), // 浮点路径不产生 Big
    };
    // est = base_digits * exp + 1（log10(a^b) ≈ digits(a) * b）
    let est = base_digits.saturating_mul(exp as usize).saturating_add(1);
    if est > ctx.max_digits {
        Err(format!(
            "Result would have {} digits (budget: {})",
            est, ctx.max_digits
        ))
    } else {
        Ok(())
    }
}

/// floor 除（Python 语义：向 -∞ 取整）。num-bigint 除法向零截断，
/// 余数非零且与除数异号时商再减一。
fn floor_div_big(a: &BigInt, b: &BigInt) -> BigInt {
    // num-bigint 的 Div/Rem 为向零截断，与 Python floor 语义差一次修正
    let q = a / b;
    let r = a % b;
    if !r.is_zero() && (r.sign() != b.sign()) {
        q - BigInt::one()
    } else {
        q
    }
}

/// Python 模语义的 Int 版本：余数符号随除数；溢出由调用方走 Big 路径。
fn mod_i64(a: i64, b: i64) -> Result<Value, String> {
    let mut r = a % b; // Rust 截断余数
    if r != 0 && (r < 0) != (b < 0) {
        r = r
            .checked_add(b)
            .ok_or_else(|| "Numeric overflow".to_string())?;
    }
    Ok(Value::Int(r))
}

fn big_pow_checked(base: &BigInt, exp: u32, ctx: &Ctx) -> Result<Value, String> {
    check_budget_pow(&Value::Big(base.clone()), exp, ctx)?;
    let r = base.pow(exp);
    check_budget_big(&r, ctx)?;
    Ok(Value::Big(r))
}

/// 二元运算统一入口（镜像 Python `compute`）。
///
/// 错误信息字符串与 Python 原型逐字节一致：
/// 除零三类 → "Division by zero" / "Floor division by zero" / "Modulo by zero"；
/// 指数超限 → "Exponent too large (|b| > 10000)"；浮点溢出 → "Numeric overflow"；
/// 其余（0 的负次幂）→ "Computation failed: …"。
pub fn compute(a: &Value, op: BinOp, b: &Value, ctx: &Ctx) -> Result<Value, String> {
    // 镜像原型：compute 的返回值先过 format_num（12 位舍入/整数化），
    // 该格式化值会反馈进后续计算（如 2/3 → 0.666666666667 → *3 = 2.000000000001）。
    // 响应/步骤展示再格式化一次（幂等，Float 整值 → Int），与原型双重格式化一致。
    let raw = compute_raw(a, op, b, ctx)?;
    let f = format_num_value(&raw);
    Ok(match f {
        Formatted::Int(v) => Value::Int(v),
        Formatted::Big(b) => Value::Big(b),
        Formatted::Float(x) => Value::Float(x),
    })
}

fn compute_raw(a: &Value, op: BinOp, b: &Value, ctx: &Ctx) -> Result<Value, String> {
    match op {
        BinOp::Add => {
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => match x.checked_add(*y) {
                    Some(v) => Ok(Value::Int(v)),
                    None => {
                        let r = a.to_big() + b.to_big();
                        check_budget_big(&r, ctx)?;
                        Ok(Value::Big(r))
                    }
                },
                // 浮点参与：Big 转换失败（超 f64 域）即溢出，与 Python 一致
                // Big 精确路径（Big+Float 走浮点，Big 转换失败即溢出）
                _ if Value::either_float(a, b) => {
                    let (fa, fb) = both_f64(a, b)?;
                    finite(fa + fb)
                }
                _ => {
                    let r = a.to_big() + b.to_big();
                    check_budget_big(&r, ctx)?;
                    Ok(Value::Big(r))
                }
            }
        }
        BinOp::Sub => {
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => match x.checked_sub(*y) {
                    Some(v) => Ok(Value::Int(v)),
                    None => {
                        let r = a.to_big() - b.to_big();
                        check_budget_big(&r, ctx)?;
                        Ok(Value::Big(r))
                    }
                },
                // Big 精确路径（Big+Float 走浮点，Big 转换失败即溢出）
                _ if Value::either_float(a, b) => {
                    let (fa, fb) = both_f64(a, b)?;
                    finite(fa - fb)
                }
                _ => {
                    let r = a.to_big() - b.to_big();
                    check_budget_big(&r, ctx)?;
                    Ok(Value::Big(r))
                }
            }
        }
        BinOp::Mul => {
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => match x.checked_mul(*y) {
                    Some(v) => Ok(Value::Int(v)),
                    None => {
                        let r = a.to_big() * b.to_big();
                        check_budget_big(&r, ctx)?;
                        Ok(Value::Big(r))
                    }
                },
                // Big 精确路径（Big+Float 走浮点，Big 转换失败即溢出）
                _ if Value::either_float(a, b) => {
                    let (fa, fb) = both_f64(a, b)?;
                    finite(fa * fb)
                }
                _ => {
                    let r = a.to_big() * b.to_big();
                    check_budget_big(&r, ctx)?;
                    Ok(Value::Big(r))
                }
            }
        }
        BinOp::Div => {
            // 真除法恒出浮点（Python int/int → float）；b==0 已前置
            if b.is_zero() {
                return Err("Division by zero".to_string());
            }
            let (fa, fb) = both_f64(a, b)?;
            finite(fa / fb)
        }
        BinOp::FloorDiv => {
            if b.is_zero() {
                return Err("Floor division by zero".to_string());
            }
            if Value::either_float(a, b) {
                let (fa, fb) = both_f64(a, b)?;
                return finite((fa / fb).floor());
            }
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => {
                    // MIN / -1 溢出 → Big 精确路径
                    match x.checked_div(*y) {
                        Some(q) => {
                            let r = x % *y;
                            let q = if r != 0 && (r < 0) != (*y < 0) {
                                q - 1
                            } else {
                                q
                            };
                            Ok(Value::Int(q))
                        }
                        None => {
                            let q = floor_div_big(&a.to_big(), &b.to_big());
                            check_budget_big(&q, ctx)?;
                            Ok(Value::Big(q))
                        }
                    }
                }
                _ => {
                    let q = floor_div_big(&a.to_big(), &b.to_big());
                    check_budget_big(&q, ctx)?;
                    Ok(Value::Big(q))
                }
            }
        }
        BinOp::Mod => {
            if b.is_zero() {
                return Err("Modulo by zero".to_string());
            }
            if Value::either_float(a, b) {
                // Python 浮点模：a - b*floor(a/b)，符号随除数
                let (fa, fb) = both_f64(a, b)?;
                return finite(fa - fb * (fa / fb).floor());
            }
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => mod_i64(*x, *y),
                _ => {
                    let (ba, bb) = (a.to_big(), b.to_big());
                    let r = &ba - &bb * floor_div_big(&ba, &bb);
                    check_budget_big(&r, ctx)?;
                    Ok(Value::Big(r))
                }
            }
        }
        BinOp::Pow => {
            // 指数上限（Python abs(b) > 10000 前置检查；Big 指数必超限）
            if b.abs_gt_i64(10_000) {
                return Err("Exponent too large (|b| > 10000)".to_string());
            }
            // 浮点参与：Python float ** 语义（0.0 负次幂 → ZeroDivisionError →
            // v4 包装为 "Computation failed: 0.0 cannot be raised to a negative power"）
            if Value::either_float(a, b) {
                let (fa, fb) = both_f64(a, b)?;
                if fa == 0.0 && fb < 0.0 {
                    // Python 3.11+ 消息（语料仲裁）：统一为 "zero to a negative power"
                    return Err("Computation failed: zero to a negative power".to_string());
                }
                return finite(fa.powf(fb));
            }
            let exp = match b {
                Value::Int(e) => *e,
                _ => unreachable!("Big 指数已被上限检查拦截"),
            };
            match a {
                Value::Int(x) => {
                    if exp >= 0 {
                        match x.checked_pow(exp as u32) {
                            Some(v) => Ok(Value::Int(v)),
                            None => big_pow_checked(&a.to_big(), exp as u32, ctx),
                        }
                    } else if *x == 0 {
                        // Python 0 ** -1 → ZeroDivisionError("0 cannot be raised to a negative power")
                        Err("Computation failed: zero to a negative power".to_string())
                    } else {
                        let fb = a.to_f64().ok_or_else(|| "Numeric overflow".to_string())?;
                        finite(fb.powf(exp as f64))
                    }
                }
                Value::Big(x) => {
                    if exp >= 0 {
                        big_pow_checked(x, exp as u32, ctx)
                    } else if x.is_zero() {
                        Err("Computation failed: zero to a negative power".to_string())
                    } else {
                        let fb = x.to_f64().ok_or_else(|| "Numeric overflow".to_string())?;
                        finite(fb.powf(exp as f64))
                    }
                }
                Value::Float(_) => unreachable!("浮点分支已在前处理"),
            }
        }
        BinOp::BitAnd | BinOp::BitOr => {
            // 仅整数域；负数走补码语义（num-bigint & | 与 Python 一致）
            if Value::either_float(a, b) {
                return Err(BITWISE_TYPE_ERR.to_string());
            }
            let is_and = op == BinOp::BitAnd;
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => {
                    Ok(Value::Int(if is_and { x & y } else { x | y }))
                }
                _ => {
                    let r = if is_and {
                        a.to_big() & b.to_big()
                    } else {
                        a.to_big() | b.to_big()
                    };
                    check_budget_big(&r, ctx)?;
                    Ok(value_to_int_or_big(r))
                }
            }
        }
        BinOp::BitShl | BinOp::BitShr => {
            if Value::either_float(a, b) {
                return Err(BITWISE_TYPE_ERR.to_string());
            }
            // 负移位 → 域错误（Python: negative shift count）
            let negative = match b {
                Value::Int(v) => *v < 0,
                Value::Big(b) => b.sign() == num_bigint::Sign::Minus,
                Value::Float(_) => unreachable!("浮点已拦截"),
            };
            if negative {
                return Err("Shift count must be non-negative".to_string());
            }
            // Big 移位量超出 u64 → 饱和（左移随即被位数预估拦截）
            let shift: u64 = match b {
                Value::Int(v) => *v as u64,
                Value::Big(b) => b.to_u64().unwrap_or(u64::MAX),
                Value::Float(_) => unreachable!("浮点已拦截"),
            };
            if op == BinOp::BitShl {
                // D4 复用：digits(1<<s) ≤ ceil(s·log10 2)，先预估后物化（防内存爆炸）
                let est = ((shift as f64 * std::f64::consts::LOG10_2).ceil() as usize)
                    .min(usize::MAX);
                if est > ctx.max_digits {
                    return Err(format!(
                        "Result would have {est} digits (budget: {})",
                        ctx.max_digits
                    ));
                }
                let r = a.to_big() << shift;
                check_budget_big(&r, ctx)?;
                Ok(value_to_int_or_big(r))
            } else {
                // 右移单调缩小，无需预算；结果转 Int（若放得下）
                let r = a.to_big() >> shift;
                Ok(value_to_int_or_big(r))
            }
        }
    }
}

/// BigInt 结果在 i64 范围内时降为 Int（与引擎快路径口径一致）。
fn value_to_int_or_big(r: BigInt) -> Value {
    match r.to_i64() {
        Some(v) => Value::Int(v),
        None => Value::Big(r),
    }
}

/// 一元负号（Python `-x`；Int::MIN 溢出自动晋升 Big）。
pub fn neg(a: &Value, ctx: &Ctx) -> Result<Value, String> {
    match a {
        Value::Int(v) => match v.checked_neg() {
            Some(v) => Ok(Value::Int(v)),
            None => {
                let r = -a.to_big();
                check_budget_big(&r, ctx)?;
                Ok(Value::Big(r))
            }
        },
        Value::Big(b) => Ok(Value::Big(-b)),
        Value::Float(f) => Ok(Value::Float(-f)),
    }
}

/// `format_num` 的三态输出（Python：整数化浮点 → int；其余 → round(x, 12)）。
#[derive(Clone, Debug)]
pub enum Formatted {
    Int(i64),
    Big(BigInt),
    Float(f64),
}

/// Python `round(x, 12)` 等价实现：对二进制真值做 12 位小数最近舍入
/// （ties-to-even）。Rust `{:.12}` 同为正确舍入 + ties-to-even，经 parse
/// 往返得到等值浮点。注意：Python `round(x, n)` 恒返回 float（即便结果
/// 整数化，如 round(1.9999999999999998, 12) → 2.0），此处保持一致。
fn round12(x: f64) -> f64 {
    let s = format!("{:.12}", x);
    s.parse::<f64>().unwrap_or(x)
}

/// Python `format_num`：整数化浮点显示为 int（避免 8.0 输出），其余 round 12 位。
/// 巨大整值浮点（如 1e20）转 Big（Python int(x) 为任意精度）。
pub fn format_num(x: f64) -> Formatted {
    if x.is_finite() && x.trunc() == x {
        // i64 范围判定：边界用精确浮点常量比较（x as i64 会饱和，必须先判）
        if (-9223372036854775808.0..9223372036854775808.0).contains(&x) {
            return Formatted::Int(x as i64);
        }
        return match BigInt::from_f64(x) {
            Some(b) => Formatted::Big(b),
            None => Formatted::Float(x),
        };
    }
    Formatted::Float(round12(x))
}

/// 对 `Value` 应用 format_num（仅 Float 受影响；Int/Big 原样）。
/// 双路校验与步骤结果渲染的统一起点。
pub fn format_num_value(v: &Value) -> Formatted {
    match v {
        Value::Int(i) => Formatted::Int(*i),
        Value::Big(b) => Formatted::Big(b.clone()),
        Value::Float(f) => format_num(*f),
    }
}

/// Python 风格的浮点 repr（str/repr 一致）：|x| ≥ 1e16 或 < 1e-4 走指数形式
/// （`1e+21`、`1.5e-05`），其余十进制展开且整值补 `.0`。
pub fn py_float_repr(x: f64) -> String {
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".into()
        } else if x > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let mag = x.abs();
    if !(1e-4..1e16).contains(&mag) {
        let s = format!("{:e}", x); // Rust 最短有效数字 + 'e'指数（无符号/无补零）
        let (mant, exp) = s.split_once('e').unwrap_or((s.as_str(), "0"));
        let e: i32 = exp.parse().unwrap_or(0);
        let sign = if e >= 0 { '+' } else { '-' };
        format!("{}e{}{:02}", mant, sign, e.abs())
    } else {
        let s = format!("{}", x);
        if s.contains('.') {
            s
        } else {
            format!("{}.0", s)
        }
    }
}

impl Formatted {
    /// Python `str(format_num(x))`：Int/Big → 十进制数字串；Float → py repr。
    pub fn to_display(&self) -> String {
        match self {
            Formatted::Int(v) => v.to_string(),
            Formatted::Big(b) => b.to_string(),
            Formatted::Float(f) => py_float_repr(*f),
        }
    }

    /// 数值相等（跨类型）：Python `3 == 3.0` 为真，双路校验依赖此语义。
    /// Float 与 Int/Big 比较经 `BigInt::from_f64` 精确转换（无精度损失）。
    pub fn num_eq(&self, other: &Formatted) -> bool {
        use Formatted::*;
        match (self, other) {
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Float(a), _) => num_eq_float_big(*a, other),
            (_, Float(b)) => num_eq_float_big(*b, self),
            (Int(a), Big(b)) | (Big(b), Int(a)) => BigInt::from(*a) == *b,
            (Big(a), Big(b)) => a == b,
        }
    }
}

fn num_eq_float_big(f: f64, other: &Formatted) -> bool {
    if !f.is_finite() {
        return false;
    }
    match BigInt::from_f64(f) {
        Some(b) => match other {
            Formatted::Int(i) => b == BigInt::from(*i),
            Formatted::Big(big) => b == *big,
            Formatted::Float(g) => f == *g,
        },
        None => false,
    }
}

impl fmt::Display for Value {
    /// Python `str(value)`：Int/Big → 数字串；Float → py repr（最短往返）。
    /// 用于错误信息中嵌入原始操作数（如 "Prefix evaluation failed: …"）。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{}", v),
            Value::Big(b) => write!(f, "{}", b),
            Value::Float(x) => write!(f, "{}", py_float_repr(*x)),
        }
    }
}
