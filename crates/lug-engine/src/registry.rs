//! 运算注册表（§3.4 唯一扩展点）。
//!
//! 编码分区（承诺：**只增不改**，0–6 冻结）：
//! - 0–6   冻结二元算术
//! - 16    后缀一元（`!` 阶乘）
//! - 32–47 函数（sqrt/ln/log/三角/取整/min/max…）
//! - 56–63 常量（pi/e/tau；词法期折叠为字面量，编码仅用于 operator 表公示）
//! - 72–79 特殊形式（sum/prod：惰性参数 + 绑定变量）
//!
//! 新增运算 = 在 [`REGISTRY`] 追加 `OpDef` + 在 `eval.rs` 挂求值核 +
//! 差分/单元用例，路由/IR/步骤/编码表全部自动生效。
//!
//! T9a 扩展区（只增不改）：7–10 位运算二元；17 百分号后缀；
//! 48–55 组合数学；64–71 反三角/双曲/按位非。

use crate::value::BinOp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Assoc {
    Left,
    Right,
}

/// 参数个数约束（求值期校验；sum/prod 的首参为绑定变量名，另行处理）。
#[derive(Clone, Copy, Debug)]
pub enum Arity {
    Exact(usize),
    AtLeast(usize),
    /// 1 或 2（log(x) / log(x, b)）
    OneOrTwo,
}

impl Arity {
    pub fn contains(&self, n: usize) -> bool {
        match self {
            Arity::Exact(k) => n == *k,
            Arity::AtLeast(k) => n >= *k,
            Arity::OneOrTwo => n == 1 || n == 2,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Arity::Exact(k) => format!("exactly {k} argument{}", if *k == 1 { "" } else { "s" }),
            Arity::AtLeast(k) => format!("at least {k} argument{}", if *k == 1 { "" } else { "s" }),
            Arity::OneOrTwo => "1 or 2 arguments".to_string(),
        }
    }
}

/// 运算类别（决定文法位置与求值路径）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    /// 二元中缀（0–6 冻结区）。
    Binary,
    /// 后缀一元（`!`）。
    Postfix,
    /// 前缀函数调用 `name(args)`。
    Function,
    /// 词法期常量（折叠为字面量；无求值核）。
    Constant,
    /// 特殊形式（sum/prod：惰性求值 + 绑定变量，单步 Opaque 计步）。
    Special,
}

/// 单个运算的定义。
#[derive(Clone, Copy, Debug)]
pub struct OpDef {
    /// 先序 IR 编码。
    pub code: u16,
    /// 规范符号（函数/常量为名字，中缀为符号）。
    pub symbol: &'static str,
    /// 中文名（operator 表展示字段）。
    pub name: &'static str,
    pub kind: OpKind,
    /// 二元运算语义（仅 Binary 使用）。
    pub op: Option<BinOp>,
    /// 优先级：add=10, unary=15, mul=20, power=25, postfix=30。
    pub precedence: u8,
    pub associativity: Assoc,
    /// 词法别名（单字符/短词，如 `×`；单词别名见 WORD_ALIASES）。
    pub aliases: &'static [&'static str],
    /// 参数个数（Function/Special/Postfix 使用）。
    pub arity: Option<Arity>,
}

const fn binary(
    code: u16,
    symbol: &'static str,
    name: &'static str,
    op: BinOp,
    precedence: u8,
    assoc: Assoc,
    aliases: &'static [&'static str],
) -> OpDef {
    OpDef {
        code,
        symbol,
        name,
        kind: OpKind::Binary,
        op: Some(op),
        precedence,
        associativity: assoc,
        aliases,
        arity: None,
    }
}

/// 全部注册项（顺序 = 编码顺序；区段间留空洞以便插入）。
pub const REGISTRY: &[OpDef] = &[
    // ---- 0–6 冻结二元算术 ----
    binary(0, "+", "加法", BinOp::Add, 10, Assoc::Left, &[]),
    binary(1, "-", "减法", BinOp::Sub, 10, Assoc::Left, &[]),
    binary(2, "*", "乘法", BinOp::Mul, 20, Assoc::Left, &["×"]),
    binary(3, "/", "除法", BinOp::Div, 20, Assoc::Left, &["÷"]),
    binary(4, "//", "整除", BinOp::FloorDiv, 20, Assoc::Left, &[]),
    binary(5, "%", "取模", BinOp::Mod, 20, Assoc::Left, &[]),
    binary(6, "**", "幂运算", BinOp::Pow, 25, Assoc::Right, &["^"]),
    // ---- 7–10 位运算（T9a S4，Python 相对序：| < & < 移位 < add）----
    binary(7, "&", "按位与", BinOp::BitAnd, 7, Assoc::Left, &[]),
    binary(8, "|", "按位或", BinOp::BitOr, 6, Assoc::Left, &[]),
    binary(9, "<<", "左移", BinOp::BitShl, 8, Assoc::Left, &[]),
    binary(10, ">>", "右移", BinOp::BitShr, 8, Assoc::Left, &[]),
    // ---- 16–23 后缀一元 ----
    OpDef {
        code: 16,
        symbol: "!",
        name: "阶乘",
        kind: OpKind::Postfix,
        op: None,
        precedence: 30,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    // T9a S1：百分号后缀（与二元 `%` 取模同一符号，语法位置区分；
    // 可调用符号经 callable_by_symbol 查找，by_symbol 首个命中仍是二元取模）
    OpDef {
        code: 17,
        symbol: "%",
        name: "百分比",
        kind: OpKind::Postfix,
        op: None,
        precedence: 30,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    // ---- 32–47 函数 ----
    OpDef {
        code: 32,
        symbol: "sqrt",
        name: "平方根",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 33,
        symbol: "cbrt",
        name: "立方根",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 34,
        symbol: "abs",
        name: "绝对值",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 35,
        symbol: "exp",
        name: "指数（e^x）",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 36,
        symbol: "ln",
        name: "自然对数",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 37,
        symbol: "log2",
        name: "以 2 为底对数",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 38,
        symbol: "log10",
        name: "以 10 为底对数",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 39,
        symbol: "log",
        name: "对数（log(x)=ln；log(x,b) 指定底）",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::OneOrTwo),
    },
    OpDef {
        code: 40,
        symbol: "sin",
        name: "正弦",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 41,
        symbol: "cos",
        name: "余弦",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 42,
        symbol: "tan",
        name: "正切",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 43,
        symbol: "floor",
        name: "向下取整",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 44,
        symbol: "ceil",
        name: "向上取整",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 45,
        symbol: "round",
        name: "四舍五入（远离零）",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 46,
        symbol: "min",
        name: "最小值",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::AtLeast(1)),
    },
    OpDef {
        code: 47,
        symbol: "max",
        name: "最大值",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::AtLeast(1)),
    },
    // ---- 48–55 组合数学（T9a S2）----
    OpDef {
        code: 48,
        symbol: "gcd",
        name: "最大公约数",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::AtLeast(2)),
    },
    OpDef {
        code: 49,
        symbol: "lcm",
        name: "最小公倍数",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::AtLeast(2)),
    },
    OpDef {
        code: 50,
        symbol: "C",
        name: "组合数 C(n, k)",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(2)),
    },
    OpDef {
        code: 51,
        symbol: "P",
        name: "排列数 P(n, k)",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(2)),
    },
    // ---- 56–63 常量（词法期折叠；列出以公示编码）----
    OpDef {
        code: 56,
        symbol: "pi",
        name: "圆周率 π",
        kind: OpKind::Constant,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: None,
    },
    OpDef {
        code: 57,
        symbol: "e",
        name: "自然常数 e",
        kind: OpKind::Constant,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: None,
    },
    OpDef {
        code: 58,
        symbol: "tau",
        name: "2π",
        kind: OpKind::Constant,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: None,
    },
    // ---- 64–71 反三角/双曲（T9a S3）----
    OpDef {
        code: 64,
        symbol: "asin",
        name: "反正弦",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 65,
        symbol: "acos",
        name: "反余弦",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 66,
        symbol: "atan",
        name: "反正切",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 67,
        symbol: "atan2",
        name: "反正切（象限版 atan2(y, x)）",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(2)),
    },
    OpDef {
        code: 68,
        symbol: "sinh",
        name: "双曲正弦",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 69,
        symbol: "cosh",
        name: "双曲余弦",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    OpDef {
        code: 70,
        symbol: "tanh",
        name: "双曲正切",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    // T9a S4：按位非（前缀一元，与 +/− 同层；补码 ~x = −x−1）
    OpDef {
        code: 71,
        symbol: "~",
        name: "按位非",
        kind: OpKind::Function,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: Some(Arity::Exact(1)),
    },
    // ---- 72–79 特殊形式 ----
    OpDef {
        code: 72,
        symbol: "sum",
        name: "求和 Σ",
        kind: OpKind::Special,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: None,
    },
    OpDef {
        code: 73,
        symbol: "prod",
        name: "求积 Π",
        kind: OpKind::Special,
        op: None,
        precedence: 0,
        associativity: Assoc::Left,
        aliases: &[],
        arity: None,
    },
];

/// 单词别名 → 符号。注意：`sum`/`prod` 已升格为聚合函数，**不再**是 `+`/`*`
/// 的别名（差异表 D7）；`e` 是常量，同样从别名中排除。
pub const WORD_ALIASES: &[(&str, &str)] = &[
    ("add", "+"),
    ("plus", "+"),
    ("加", "+"),
    ("加上", "+"),
    ("sub", "-"),
    ("minus", "-"),
    ("subtract", "-"),
    ("减", "-"),
    ("减去", "-"),
    ("mul", "*"),
    ("times", "*"),
    ("multiply", "*"),
    ("乘", "*"),
    ("乘以", "*"),
    ("div", "/"),
    ("divide", "/"),
    ("除", "/"),
    ("除以", "/"),
    ("floordiv", "//"),
    ("整除", "//"),
    ("mod", "%"),
    ("modulo", "%"),
    ("取余", "%"),
    ("取模", "%"),
    ("pow", "**"),
    ("power", "**"),
    ("幂", "**"),
    ("乘方", "**"),
];

/// 词法期常量（名字 → f64 字面量；非负场景外的一元负号自然组合）。
pub const CONSTANTS: &[(&str, f64)] = &[
    ("pi", std::f64::consts::PI),
    ("e", std::f64::consts::E),
    ("tau", std::f64::consts::TAU),
];

pub fn by_symbol(symbol: &str) -> Option<&'static OpDef> {
    REGISTRY.iter().find(|d| d.symbol == symbol)
}

/// 可调用符号（后缀/函数/特殊形式）查找：`%` 同时注册为二元取模（code 5）
/// 与百分号后缀（code 17），求值核/步骤/IR 的函数侧查找必须命中后缀定义。
pub fn callable_by_symbol(symbol: &str) -> Option<&'static OpDef> {
    REGISTRY.iter().find(|d| {
        d.symbol == symbol
            && matches!(
                d.kind,
                OpKind::Postfix | OpKind::Function | OpKind::Special
            )
    })
}

pub fn by_code(code: u16) -> Option<&'static OpDef> {
    REGISTRY.iter().find(|d| d.code == code)
}

/// 指定优先级与结合性的二元运算符符号（解析器按层取符号集）。
pub fn symbols_at(precedence: u8, assoc: Assoc) -> Vec<&'static str> {
    REGISTRY
        .iter()
        .filter(|d| {
            d.kind == OpKind::Binary && d.precedence == precedence && d.associativity == assoc
        })
        .map(|d| d.symbol)
        .collect()
}
