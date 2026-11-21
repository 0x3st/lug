# Lug Rust 重写设计（v1 · 发布前实现切换）

> 目标：用 Rust 重写后端，引擎行为与 **Python 原型差分对齐**（旧语料逐字节），
> 运算体系重构为**注册表驱动**，为 `log`/`sigma`/阶乘等高阶运算预留体系。
> 引擎与服务器严格分层：`lug-engine` 纯计算零 I/O，`lug-server` 基于 tokio/axum。
>
> **版本状态**：产品为 **v1 发布前**，无存量用户契约，不存在 semver 意义上的
> breaking。Python 实现是参考原型（README 中的 v3/v4 变更笔记属研发期迭代
> 记录），Rust 实现即 v1 的正式承载；下文 v4 字样均指 Python 原型行为，
> 不是产品版本号。

---

## 1. 决策依据（为什么这次重写站得住）

| 论点 | 事实 |
|---|---|
| SSE 长连接是真瓶颈 | `_stream_eval` 每步 `sleep(0.05)`，过程流连接驻留秒级；`ThreadingHTTPServer` 每连接一线程，C1000 驻留连接 = 1000 线程。tokio 下这是免费起步的 |
| 扩展性债是真实的 | 运算符分散在 `OPERATORS` / `OP_ALIASES` / `OPERATOR_CODES` 三个全局 dict + `compute()` 的 if/else + 递归下降硬编码优先级。加 `log` 要改 5 处 |
| 内存安全与并发正确性 | 计费用量在锁内全量重写 JSON，Rust 的所有权模型让"谁持有写锁"在编译期可见 |
| 部署反而变简单 | 单静态二进制 + systemd，无 Python 版本矩阵；内存 ~10MB |
| 附加红利 | `lug-engine` 不依赖 tokio/serde，可直接编到 wasm 给前端做在线演算 playground |

**同时坦白**：表达式求值本身是微秒级工作，Rust 对"算得快"没有意义；重写价值在
**连接密度、扩展体系、部署形态** 三件事上。

### 1.1 定位备忘：精度不是护城河，组合才是

**纠偏**："做不到比其他更高精度"不成立——真正的竞品是 LLM API（Lug 的接口
形态本就对标它们），而那场比赛里我们的精度优势是**无穷大**：LLM 算多位乘法
都会错、`20!` 靠背、黑盒不可验证；Lug 每一位都确定且步进可审。对计算器类
对手（Google/Desmos），我们又多了 API 化与高精度 opt-in。只输 Wolfram，
而 Wolfram 输给我们价格、接入简单性和延迟。

| 对手 | 算术可靠性 | 步骤可审计 | 接入 | 计费 | 延迟 |
|---|---|---|---|---|---|
| LLM API | ❌ 会编造 | ❌ 黑盒 | 低 | 贵且不可预估 | 秒级 |
| Wolfram API | ✅ | 部分 | 中（专有查询语言） | 贵 | 数百 ms |
| 自建 mpmath | ✅ | 自建 | 高（写码+运维） | 无 | 自担 |
| **Lug** | ✅ 确定 | ✅ SSE 步进 + 双路校验 | 一个 endpoint | 先验精确（1 步 1 token） | 毫秒级 |

**真护城河是组合**：确定性 + 可审计步骤流 + OpenAI 风格接入 + 先验账单。
杀手场景是 **LLM function calling 的确定性计算工具**——agent 生态里"不会
编造数字、人类可审计、接入只是一行 base_url"的计算件是稀缺品。精度是
入场券，不是胜负手；单品可复制，"注册表驱动的运算交付速度"才是复利。

---

## 2. 总体架构

```text
lug/
├── Cargo.toml                  # workspace
├── crates/
│   ├── lug-engine/             # 纯计算库：零 I/O、零 tokio、零 serde
│   │   └── src/
│   │       ├── lib.rs          #   evaluate_full 流水线 + ExprError
│   │       ├── value.rs        #   Value / 数值语义 / format_num
│   │       ├── lexer.rs        #   词法：num / ident / op / paren / comma
│   │       ├── ast.rs          #   AST 节点 + span
│   │       ├── parser.rs       #   递归下降（优先级查注册表）
│   │       ├── registry.rs     #   ★ OpDef 注册表：唯一扩展点
│   │       ├── prefix.rs       #   波兰式 IR 编码 + 栈式求值
│   │       ├── steps.rs        #   后序逐步求值 + state 渲染
│   │       └── billing.rs      #   count_tokens / build_usage
│   └── lug-server/             # axum 二进制
│       └── src/
│           ├── main.rs         #   装配、优雅停机
│           ├── routes/         #   calculate / operator / platform/* / static
│           ├── auth/           #   config / oauth / session / csrf / apikey
│           ├── store/          #   KeyStore trait + JSON 实现（SQLite fast-follow）
│           └── sse.rs          #   过程流事件序列
├── legacy/                     # Python 实现整体迁入（golden 语料生成器 + 回滚备份）
└── tests/golden/               # ★ 契约语料：Python 生成，Rust 逐字节对齐
```

分层规则：
- `lug-engine` 不出现 `tokio` / `serde` / `std::fs`。输出纯 Rust 数据结构，序列化由 server 完成。
- `lug-server` 不出现解析/求值逻辑。HTTP、鉴权、持久化各归各位。
- 所有运算符/函数/常量只允许在 `registry.rs` 注册，其余代码只认 `OpDef`。

---

## 3. lug-engine 设计

### 3.1 数值类型与语义

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value { Int(i64), Float(f64) }
```

语义表（★ = 与 Python 显式对齐，◇ = 新语义，用户需适应）：

| 运算 | 语义 | 备注 |
|---|---|---|
| `+ - *`（Int,Int） | ★ i64 checked，溢出自动晋升 BigInt（值位数护栏内），语义同 Python | 快路径零堆分配；见下方表示决策 |
| `+ - *`（含 Float） | ★ f64 | |
| `/` | ★ 真除法，恒出 Float；`b == 0` → `Division by zero`（浮点 0.0 同样报错） | `8/2` → 结果 4，显示为 Int |
| `//` | ★ floor 除，符号向 -∞ | Rust `/` 是截断，须手写 |
| `%` | ★ 余数符号随除数（`-7 % 2 = 1`） | Rust `%` 是截断余数，须手写 |
| `**` | ★ Int^Int(neg) → Float；`0**0 = 1`；`\|exp\| > 10000` → `Exponent too large (\|b\| > 10000)` | 保持现有阈值 |
| `!`（阶乘） | ◇ 仅 Int ≥ 0；结果受值位数护栏约束（1000! ≈ 2568 位，可算） | 后缀，优先级高于 `**` |
| `sqrt/ln/log/...` | ◇ f64 域错误 → `computation_error: log(x): x must be positive` 等 | 见 §3.4 函数区 |
| `sum/prod` | ◇ 边界须 Int，`\|to-from\| ≤ 1_000_000`（费用护栏） | 见 §3.4 特殊形式 |

`format_num` 兼容规则（golden 语料仲裁）：
- Float 且有限且 `x == x.trunc()` → 输出 Int（避免 `8.0`）
- 其余 Float → 先做 Python `round(x, 12)` 等价实现（12 位小数去浮点噪声，如
  `0.1+0.2 → 0.3`），JSON 序列化用 shortest round-trip（serde_json 默认即与
  Python `repr` 一致）
- ⚠ 已知风险 R2：Python `round` 是 half-to-even，Rust 需手写同款，不能直接 `f64::round()`
- 浮点结果非有限（±inf/NaN）→ `computation_error: Numeric overflow`（对齐 v4 的
  OverflowError 路径）。注意 v4 的 `1e308+1e308` 会把 Python 的 `Infinity` 直接
  写进 JSON（实为非法 JSON）；Rust 版统一报错，语料预埋该用例，列入 §5 差异表 D3

**整数表示决策（定稿）：C 档 —— i64 快路径 + BigInt 自动晋升**

```rust
pub enum Value {
    Int(i64),              // 快路径：绝大多数运算零堆分配，性能与纯 i64 方案一致
    Big(Box<BigInt>),      // i64 溢出时自动晋升（num-bigint：纯算术、无 I/O）
    Float(f64),
}
```

- 晋升规则：i64 运算 checked，溢出 → 同运算以 BigInt 重试；Big 参与的整数运算 → BigInt；
- `//` / `%` 在 BigInt 上用 `div_floor` / `mod_floor`，保持 Python floor 语义；
- BigInt → Float 只发生在真除法与负指数幂：`to_f64` 不可表示 → `Numeric overflow`
  （对齐 Python `10**400/1` 的 OverflowError → v4 同文案）；
- `2**10000`（3011 位）、`100!`、`prod(i,1,500,i)` 照算——原型能算的大数全保；
- **运算预算（新增加固，修复 v4 隐患）**：v4 的 `\|exp\| ≤ 10000` 只限指数不限底数——
  `(10**3000)**4000`（10^1200 万位，约 12MB 数字）v4 能算出来，再嵌一层就是
  10^480 亿位 → 内存打爆。新规则：**语义上不设大数上限**，资源侧用运算预算——
  单值位数预算 `LUG_MAX_VALUE_DIGITS`（默认 10^7 位）+ 单请求硬超时
  `LUG_COMPUTE_TIMEOUT_MS`（默认 5000ms）；幂等运算**先按位数估算再计算**
  （result_digits ≈ base_digits × exp，零成本拦截），报错携带精确预估：
  `Result would have 12,000,001 digits (budget: 10,000,000)`；

定稿理由：计费口径（1 步 = 1 token）使值的**大小**与账单彻底无关，C 档的
主要代价消失；阶乘/组合数是高阶运算的天然场景，纯 i64/i128 的 `20!`/`33!`
天花板难看；快路径性能与纯 i64 方案完全一致，符合"延迟越小越好"。代价：
engine 引入 num-bigint/num-traits/num-integer（纯算术依赖，不破坏"引擎零 I/O"
性质）；结果 ≥ 2^53 的 JSON 数字在 JS 端失真——与 v4 行为一致，写入 /docs 说明。
golden 语料中大数用例（`2**10000` 等）为常规必须通过的用例。

**超大数与超精数路线（产品方向：专精计算）**：

- **超大数（已定）**：BigInt 语义无限；资源侧用**运算预算**而非 size 禁令
  （见上）。预算可配置、可按账户分级、未来可计费——对计算引擎，预算是
  **商品**不是限制（Wolfram 也有 timeout）。
- **超精数（建议阶段一随 v1 首发，阶段二 v1.x 续期）**：请求参数 `"precision": n`（有效数字位数）→ Float
  运算切换 BigDecimal 路径（bigdecimal crate，num-bigint 系，纯 Rust）：
  - 阶段一（v1，P3 顺带 +0.5–1 天）：`+ - * / // % ** sqrt` 高精度（sqrt 用 Newton 迭代）；
  - 阶段二（v1.x 后续）：exp/ln/sin/cos 等超越函数高精度（级数 + 论域约简；或引入
    rug/MPFR 绑定——C 依赖，届时标注取舍）；
  - 输出：precision ≤ 17 时 `result` 仍为 JSON number；> 17 时新增
    `result_text`（精确十进制串），`result` 保留 f64 近似；
  - f64 快路径不变：不带 precision 的请求性能零影响；
  - **为什么默认仍是 f64**：① 单指令 ~1ns、栈上 8 字节 vs 堆分配软件算法慢
    2–3 个数量级；② 差分语料要求默认行为与原型逐字节一致；③ 数学上不存在
    免费的"全精确"——`1/3` 在十进制里同样除不尽，BigDecimal 只是换成
    "用户定份数的舍入"，`sqrt(2)` 对任何有限表示都是无理数。正确的菜单是
    **默认快、按需精确**，与 Python（float 默认 + decimal 模块）、
    Wolfram（机器精度默认 + 任意精度 opt-in）同构；
  - 计费：precision 是输入参数，token 计费不变；未来可按精度分档定价（仍静态可算）。
  - **实现机制（精确发生在表示层，不是显示层）**：
    1. 词法：`"0.1"` → `BigDecimal(尾数=1, scale=1)`，**不经 f64**——f64 在解析
       那一刻就已变成 `0.1000000000000000055…`（二进制无限循环），显示层
       `format_num` 只是事后遮羞；
    2. 求值分派（在 OpDef 的 eval 内按操作数类型 + 精度上下文 match）：
       `+ - *` 十进制精确（结果位数在预算内不舍入，`0.1+0.2 = 3×10⁻¹` 精确）；
       `/` 除不尽 → 长除法到 `precision + 2` 位保护位，half-even 舍入；
       `sqrt` → f64 出初值 + Newton 迭代 `x ← (x + a/x)/2`（每次除法到
       保护位精度，二次收敛，千位仅需 6–7 轮）；Int/BigInt 参与时无损转入
       （整数本就是精确十进制）；
    3. 收尾：统一舍入到 precision 位有效数字，按 ≤17 / >17 分流 JSON number
       或 `result_text`；
    4. 对比：`1/3` 默认 12 位 `0.333333333333`，`precision: 30` →
       `0.333333333333333333333333333333`；`sqrt(2)` →
       `1.41421356237309504880168872421`；
    5. 阶段二难点 = **正确舍入**（table maker's dilemma：需多算保护位直到能
       证明末位舍入方向，困难情形理论上无界；实践用 Ziv 循环精度递增重试）
       + 大参数的论域约简（需要高精度 π）——这是超越函数另列阶段的原因。

### 3.2 词法变化（breaking changes 显式列出）

| 变化 | 旧（Python v4） | 新（Rust） |
|---|---|---|
| 标识符 | ❌ 一律报错 | ✅ `log` `sum` `pi` `e`…（查注册表；未知 ident 报错并给 Levenshtein 最近建议，如 `sqrl(2)` → `unknown function 'sqrl'; did you mean 'sqrt'?`） |
| `x`/`X` 作乘号 | ✅ `2x3 = 6` | ❌ **移除**（与标识符冲突）；`×` `*` 保留 |
| `^` | ✅ = `**` | ✅ 保留 |
| 科学计数法 | ❌ | ✅ 数字词法最长匹配：`2e3 → 2000`，`1.5E-2 → 0.015`；`e` 单独出现是常量 |
| 新增 token | — | ident、`,`（函数实参分隔）、`!`（后缀） |

### 3.3 文法（EBNF 全量，优先级查注册表）

```ebnf
expr    := add ;
add     := mul  (("+"|"-") mul)*                      ; prec=10 left
mul     := unary (("*"|"/"|"//"|"%") unary)*          ; prec=20 left
unary   := ("+"|"-") unary | postfix                  ; prec=15（低于 **：-2**2 = -4）
postfix := power ("!")*                               ; prec=30
power   := atom ("**" unary)?                         ; prec=25 right（指数可带符号）
atom    := NUMBER | CONST | IDENT "(" args ")" | AGG "(" IDENT "," args ")" | "(" expr ")" ;
args    := expr ("," expr)* ;
AGG     := "sum" | "prod" ;                            ; 特殊形式，第一个实参是绑定变量
```

### 3.4 运算注册表（唯一扩展点）

```rust
pub struct OpDef {
    pub code: u16,                    // IR 编码，稳定承诺：只增不改
    pub symbol: &'static str,         // "+", "**", "log", "sum"
    pub name: &'static str,           // 中文名（/operator 表用）
    pub kind: OpKind,
    pub precedence: u8,
    pub associativity: Assoc,
    pub aliases: &'static [&'static str],
    pub eval: OpEval,                 // 纯函数：Value/AST → Result<Value, EvalError>
    pub step: StepPolicy,             // Always | Opaque（见 §3.6）
}

pub enum OpKind {
    Binary, UnaryPrefix, UnaryPostfix,
    Function  { arity: Arity },        // Arity::Exact(n) | AtLeast(n)
    Constant,                          // 词法期折叠为 Value
    Special  { params: usize, body: usize },  // sum/prod：惰性参数 + 绑定变量
}

pub enum OpEval {
    Eager(fn(&[Value], &Env) -> Result<Value, EvalError>),   // 二元/一元/函数
    Lazy(fn(&[&AstNode], &Env) -> Result<Value, EvalError>), // sum/prod
}
```

**编码分区表**（`/f/a/o/operator` 直接由注册表生成，0–6 冻结）：

| 区间 | 用途 | 已定编码 |
|---|---|---|
| 0–6 | ★ 冻结：`+ - * / // % **` | 现状不动 |
| 8–15 | 二元扩展 | （预留：gcd, atan2…） |
| 16–23 | 后缀一元 | 16 `!` |
| 32–47 | 函数 | 32 sqrt, 33 cbrt, 34 abs, 35 exp, 36 ln, 37 log2, 38 log10, 39 log(1~2参, log(x)=ln), 40 sin, 41 cos, 42 tan, 43 floor, 44 ceil, 45 round(half-away-from-zero), 46 min(≥1参), 47 max(≥1参) |
| 56–63 | 常量 | 56 pi, 57 e, 58 tau |
| 72–79 | 特殊形式 | 72 sum(var, from, to, body), 73 prod(var, from, to, body) |

新增运算的完整动作 = 在 `registry.rs` 加一个 `OpDef` + 补 golden 用例 +
/docs 增一行。**路由、IR、operator 表、优先级、别名全部自动生效。**

### 3.5 IR 与双路求值

- 先序 IR 沿用现格式：`{"op": code} | {"num": v} | {"var": "i"}`，
  可读串如 `+ 3 * 5 ** 2 + 1 1`；函数/常量直接进编码：
  `log(8, 2)` → `log 8 2`；`sum(i, 1, 10, i**2)` → `sum i 1 10 ** i 2`。
- 栈式求值从右往左扫（与现实现一致）；`neg/pos` 特殊 token 保留。
- **双路一致性校验保留**，但诚实声明其边界：
  - 纯算术（0–23 区）：AST 后序遍历结果必须 == 先序栈式求值结果（现行为不变）；
  - 特殊形式（72+）：求值必须惰性，两路共用同一 `Lazy` 求值核；核的正确性由
    proptest 保证（`sum(i,a,b,body)` ≡ 对应显式循环展开的结果）。
  - 常量在 parse 后即折叠为 `{"num": v}`，不进 IR 变量区。
- `Env`：`sum` body 求值携带绑定变量链（支持嵌套：`sum(i,1,10,sum(j,1,i,j))`）；
  body 引用未绑定 ident → `expression_error: undefined variable 'x'`。

### 3.6 步骤渲染与计费

- 步骤产生规则与现实现一致：后序遍历，每个可归约节点产生一步，
  `state` 用 span 替换渲染（含括号剥离细节）。
- 函数节点步骤：`expr: "log(8, 2)"`，`operator: "log"`，`operator_code: 39`。
- `StepPolicy`：
  - `Always`（现行为，0–23 区 + 普通函数）；
  - `Opaque`（sum/prod）：**整体一步**，body 不展开。理由：`sum(i,1,1000000,i)`
    若逐项展开 = 百万步 = 费用与流爆炸；一步算完，`expr` 显示整个调用，
    计费可预期（见下）。
- 计费口径（**一个计算步骤 = 1 token**，v1 唯一口径）：
  - `prompt_tokens` = 词法 token 数（ident/`,`/`!` 均计 1，保持现状）；
  - `reasoning_tokens` = **步骤数**：每个 emit 的 step 恰好计 1 token。
    Opaque 的 sum/prod 整体 1 步 = 1 token；一元符号/括号不产生步骤、不计费；
  - `completion_tokens` = reasoning + 1（最终答案 1 token）；total = prompt + completion；
  - **三元同一性**：计费 token 数 == SSE step 事件数 == 用户可见步骤数。
    SSE 事件中 `cost_tokens` 字段保留、恒为 `1`（前端已验证只展示总数，无需改动；
    /docs 文案随 P3 更新）；
  - 错误请求不产生费用（保持现状）；
  - 未来若出现单步重负载运算，可按输入参数分级计费（如 sum 按区间长度分档）
    ——仍是 parse 期可算的静态量。

### 3.7 计费可预期与配额流水线

**定义**：费用必须是输入 `expr` 的函数，而不是运行时动态（求值走了多少步、
循环跑了多少次）的函数。由此推出三条可验证的保证：

1. **先验可算**：不求值就能算出费用的**精确值**（v5 计费模型下不再是上界）；
2. **响应有界**：SSE 事件数 = 步骤数 ≤ AST 可归约节点数 ≤ 输入 token 数；
3. **工作有界**：总求值工作量由输入长度决定（另设表达式长度上限，默认 1024 字符，
   `LUG_MAX_EXPR_LEN` 可配），"20 token 表达式 → 百万步"不存在。

**v4 的两条裂缝（重写后关闭）**：

| 裂缝 | v4 现状 | Rust 方案 |
|---|---|---|
| 聚合展开爆炸 | 若 sum 逐项产生步骤：`sum(i,1,10**6,i)` = 百万步，费用随**循环上界**而非输入增长；50ms/步的流要播 14 小时 | `StepPolicy::Opaque`：整体一步，步骤数回到 O(输入) |
| **断连漏计费（真实 bug）** | `_stream_eval` 里 `_record_usage` 在**全部步骤 emit 完之后**才执行；客户端读几步就断开 → `BrokenPipeError` → `pass` → 用量从未落账 | usage 在进流前即可完整算出（静态）→ **先落账、后播流**；断连只影响观感不影响账本 |

**精确静态计费（1 步 = 1 token 的直接推论）**：步骤数 = AST 可归约节点数，parse 完
即可**精确**算出 usage——不需要引理、不需要上界估计。附带一个解耦红利：计费
不再依赖渲染细节（format_num 怎么格式化、负数算几个 token、值替换后表达式
变长变短），**显示层可以自由演进而不动账本**；v4 的"对渲染文本重新词法"规则
随 v1 计费口径一并退役。

**配额流水线**（README"后续按用户配额"的落地）：

```text
parse → prompt_tokens + 步骤数（精确，parse 完即得）
  → preflight：预估费用 > 剩余额度 → 429 insufficient_quota（OpenAI 风格，未计算先拒绝）
  → 求值 → 实际 usage 落账（== 预估值）→（process 流：先落账，再播事件）
  → 可选 dry_run: true：只返回 usage 不计算——账单精确可算的零成本红利
```

- 错误请求不产生费用：保持 v4 行为；
- 配额 ≠ 限流：Caddy 层速率限制照旧，两者互补；
- fast-follow SQLite：加 request 级 usage 明细（request_id/expr/usage/时间），
  账单争议可对账；JSON 版只有累计计数器，维持现状。

---

## 4. lug-server 设计

### 4.1 选型与依赖

| 用途 | crate | 说明 |
|---|---|---|
| HTTP 框架 | `axum` + `tokio` | SSE 一等公民 |
| 中间件 | `tower-http` | ServeDir / trace |
| 序列化 | `serde` / `serde_json` | 仅 server 层 |
| GitHub HTTP | `reqwest`（rustls，禁默认特性） | 非阻塞，超时 10s、响应上限 64KB（对齐现实现） |
| 哈希/随机 | `sha2`, `rand`, `base64`, `hmac` | Key 哈希、state、PKCE |
| 时间 | `time` | Cookie Expires（Wdy, DD Mon YYYY GMT） |
| 错误 | `thiserror`（engine）+ 任意（server） | |
| 日志 | `tracing` + `tracing-subscriber` | 替代 print |
| 原子写 | `tempfile` | 同目录 temp + rename |
| dev | `proptest`, `criterion`, `wiremock` | 属性测试 / 基准 / 假 GitHub |

### 4.2 路由（与 v4 逐条对齐，404 消息含可用列表的文案也保持）

```text
GET  / /docs /platform /assets/*          静态（web/ 构建产物，ServeDir）
GET  /f/a/o/operator                       API Key → 注册表生成的编码表（增量扩容）
POST /f/a/o/calculate                      API Key → JSON 或 SSE
GET  /f/a/o/platform/auth|auth/github|auth/github/callback|me|keys|usage
POST /f/a/o/platform/keys|logout
DELETE /f/a/o/platform/keys/{id}
OPTIONS *                                  204 + CORS（现状保留）
```

`/calculate` 行为分支与现状一致：`process` 缺省 → 一次性 JSON；
`process: true` → SSE（`prefix → step×N → final → [DONE]`）。
**步进延迟：零**（产品决策：延迟越小越好）。事件即算即发，流的总时长 ≈ 网络
往返时间；"直播感"交给客户端渲染（前端演示本就是预设步骤 + 本地动画，
不依赖到达节奏）。协议层面 SSE 契约不变：事件序列、字段、[DONE] 完全一致，
只是它们会在毫秒内全部到达。规则：

- 默认 0，`LUG_STEP_DELAY_MS` 保留为可选旋钮（演示模式可调，>0 时按总预算
  ~4s 自适应封顶，避免长表达式播放数十秒）；
- 计费与延迟无关（按步骤数，不按时间）；
- 零延迟下 SSE 流变为**毫秒级短连接**，v4 的驻留连接问题从源头消失大半
  （见 §4.5）；
- `demo_client.py` 的到达时间戳将显示 ~0ms，随 P2 更新为演示事件结构而非节奏。

### 4.3 鉴权对齐清单（迁移时逐项打勾）

- [ ] OAuth：state 一次性 + 浏览器绑定 Cookie（`hmac.compare_digest` → 常量时间比较）+ PKCE S256 + state TTL 600s + 容量 2048
- [ ] 仅调 `GET /user`；access token 不持久化、不返回前端；错误归一为 `github_unavailable`
- [ ] 会话：token_urlsafe(32) 等价随机、TTL 7d、容量 4096、惰性清理、登出即撤销
- [ ] Cookie：`HttpOnly; SameSite=Lax; Path=/`，HTTPS 下 `Secure`
- [ ] CSRF：写操作校验 `X-CSRF-Token`；Same-Origin 校验逻辑保持
- [ ] Key：SHA-256 落库、prefix 展示、明文只返回一次、撤销即时生效、owner 隔离、他人 ID 一律 404
- [ ] 日志不落 query string（OAuth code/state 隐私）
- [ ] 旧密钥迁移：`LUG_LEGACY_OWNER_GITHUB_ID` 行为不变

### 4.4 持久化

```rust
#[async_trait]
pub trait KeyStore: Send + Sync {
    async fn create(&self, name: Option<String>, owner: &OwnerId) -> Result<(String, KeyRec)>;
    async fn validate(&self, raw: &str) -> Option<KeyRec>;       // 常量时间比较
    async fn record_usage(&self, kid: &KeyId, u: &Usage);
    async fn list(&self, owner: &OwnerId) -> Vec<KeyRec>;
    async fn revoke(&self, kid: &KeyId, owner: &OwnerId) -> Option<KeyRec>;
}
```

- v1：`JsonKeyStore` —— 兼容现有 `api_keys.json`（首次启动原样读取；**格式不变，
  可随时回滚 Python 版**）。写路径 `tokio::sync::Mutex` + temp+rename 原子替换。
- fast-follow：`SqliteKeyStore`（rusqlite 独立线程 + `spawn_blocking`），
  解决单写者限制，feature flag 切换， trait 已隔离。

### 4.5 并发模型

- 计算：`lug-engine` 纯 CPU、无锁，handler 内直接算（表达式极小，不需要
  `spawn_blocking`；如基准显示长尾再议）。
- 状态共享：`Arc<AppState>`（config / oauth / sessions / keystore）。
- 优雅停机：SIGTERM → axum `with_graceful_shutdown`，systemd 不变。

**SSE 并发细节（对应"驻留连接"痛点的逐项解法）**：

| v4（Python） | Rust（tokio/axum） |
|---|---|
| 每连接一个线程（默认栈 MB 级），万级驻留 = 万级线程 | 每流一个 async task（KB 级），单核数万驻留无压力 |
| `time.sleep(0.05)` 阻塞线程逐个播放步骤 | `tokio::time::interval` 定时器轮项，O(1)，不占线程 |
| `Connection: close` 强制断开，每条流都重新握手 | hyper keep-alive，Caddy ↔ Lug 连接池复用 |
| 客户端断开要到下一次 write 才发现，期间线程照睡 | 断开 → stream future drop → 资源立即回收 |
| 慢客户端阻塞写 + sleep，白占线程 | poll 型 Stream 天然背压 |
| 假流：步骤全部算完再 sleep 逐个播放 | 可选真增量流（逐节点归约边算边发），事件序列不变 |

P4 基准项：calculate 短请求吞吐（QPS）、process 流端到端 p99 延迟（默认 0 延迟下
应为毫秒级）、若启用 `LUG_STEP_DELAY_MS>0` 则另测驻留流数与每流内存。

诚实边界：零延迟后，v4 的"驻留连接"痛点从源头消失大半（流变短了），tokio 的
价值重心从"长连接驻留"转向"高请求率 + 慢客户端隔离 + 干净取消"；余量意义
不变——不必再为连接行为重构架构。

---

## 5. 与 Python 原型的差异（v1 定稿语义）

**项目处于 v1 发布前，无存量用户契约**——下表是 v1 相对 Python 原型的差异定稿，
用于 /docs 与发布说明，不是 semver 事件。原型作为可执行规格保留在 `legacy/`，
供差分测试与回滚。

**引擎行为对齐（差分语料强制）**：旧语法（纯算术 + 括号 + 一元符号 + `× ÷ ^`）
下的步骤序列（expr/state/result/operator_code）、错误 message/type/code、
operator 表 0–6 与原型逐字节一致；计费口径按 D2 全新生成。

| # | 类别 | 差异 | 理由 |
|---|---|---|---|
| D1 | 语义决策 | `x`/`X` 不再是乘号 | 与标识符冲突；`×` `*` 保留 |
| D2 | 语义决策 | 计费 = 步骤数（1 步 = 1 token）；SSE `cost_tokens` 恒为 1 | 账单先验精确、与显示解耦 |
| D3 | 修复 | 浮点非有限（±inf/NaN）→ `computation_error: Numeric overflow`（原型会把 `Infinity` 写进 JSON，实为非法 JSON） | 合法 JSON + 明确报错 |
| D4 | 修复 | 超预算中间值 → 运算预算报错（原型嵌套幂会打爆内存，见 §3.1） | 挂 → 明确报错 |
| D5 | 新增 | 标识符/函数/常量/`!`/`sum`/`prod` 语法 | 高阶运算体系 |
| D6 | 新增 | operator 表新增 16+ 编码；函数类错误文案（如 `log(x): x must be positive`） | 新运算新文案 |

---

## 6. 测试与质量

1. **Golden corpus（成败关键）**：`legacy/` 提供 `export_golden.py`，对全语料
   （每个运算符 × 优先级矩阵 × 错误表 × Unicode × 计费边界 × format_num 边界，
   约 200+ 用例）跑 Python 实现存成 `tests/golden/*.json`（含 SSE 逐事件）。
   Rust 侧测试逐字节比对 JSON——其中步骤序列（expr/state/result/operator_code）
   按 v4 原样对齐，usage 字段由导出脚本按 **v1 计费口径（步骤数，D2）** 生成，
   把"引擎与原型差分一致、计费为 v1 定稿"表达成可执行的断言。
   **corpus 只含旧语法 → 差分可证。**
2. **proptest**：随机表达式树 → render 成字符串 → 双路一致、
   `sum` ≡ 显式循环、溢出行为符合语义表。
3. **集成测试**：wiremock 假 GitHub（承担现 `tests/e2e_server.py` 角色），
   覆盖 OAuth 全流程、CSRF、双用户隔离、旧密钥迁移。
4. **前端 E2E 不改**：Playwright 直接指向 Rust 服务（零延迟已是默认，无需环境变量）。
5. **基准**：criterion 报告单核 calculate QPS + 并发 SSE 驻留连接数，
   作为"重写值得"的量化证据。

---

## 7. 部署与切换预案

- 构建：`cargo zigbuild --release --target x86_64-unknown-linux-gnu`（macOS 交叉），
  产物单二进制 ~10MB；服务器只替换文件。
- systemd：新增 `lug-rs.service`（端口 8001），Caddy upstream 一行切换；
  Python 版原样保留 2 周作为回滚（共享同一 `api_keys.json`，格式不变）。
- 切换检查单：golden 全绿 → Playwright 全绿 → 生产 8001 灰度（curl 冒烟 +
  控制台登录建 Key）→ Caddy 切流 → 观察 48h → 下线 Python。

---

## 8. 里程碑

| 阶段 | 内容 | 估时 |
|---|---|---|
| P0 | Python 原型迁入 `legacy/`，写 `export_golden.py` 生成差分语料（步骤序列按原型原样、usage 按 v1 计费口径）；冻结规格 | 0.5 天 |
| P1 | `lug-engine`：Value/lexer/parser/registry/prefix/steps/billing，golden + proptest 全绿 | 2–3 天 |
| P2 | `lug-server`：路由/鉴权/SSE/静态/JSON store，wiremock 集成测试全绿 | 2–3 天 |
| P3 | 新运算第一批：常量 + 17 个函数 + `!` + `sum/prod`；/docs、operator 表更新（含 >2^53 结果的 JS 精度说明，与 v4 行为一致）；可选：`precision` 参数阶段一（+0.5–1 天） | 1–2 天 |
| P4 | Playwright 全量、criterion 基准、交叉编译、v1 灰度切换 | 1 天 |

---

## 9. 风险表

| # | 风险 | 缓解 |
|---|---|---|
| R1 | f64 JSON 序列化与 Python `repr` 差异 | golden 语料覆盖边界；serde_json shortest repr 与 Python 一致性已被广泛验证 |
| R2 | `round(x, 12)` half-to-even 语义 | 手写同款实现 + 语料特判（`0.1+0.2`、`2.675` 等） |
| R3 | SSE 节奏变化（v4 每事件隔 50ms，Rust 版毫秒内全部到达） | **有意变更**：事件序列/字段逐字节对齐，节奏从未进契约；时序敏感的客户端（若有）本就无法正确消费流 |
| R4 | 特殊形式使"双路校验"弱化 | Lazy 核单独 proptest；纯算术区校验不变 |
| R5 | `api_keys.json` 并发写丢失更新 | v1 仍是单写者（与现状一致）；SQLite 为 fast-follow |
| R6 | ~~前端依赖 SSE 50ms 节奏~~ | 已消除：前端不消费实时流（预设步骤 + 本地动画），零延迟为默认 |
| R7 | 超大值计算的资源滥用（嵌套幂等，v4 真实隐患） | 运算预算：单值位数预算（默认 10^7 位）+ 硬超时（5s），幂运算前按位数估算拦截，报错携带预估位数；预算可配/可分级/可计费；属修复非 breaking |

---

## 10. 神经化路线研究（Mix of Operators, MoO）

**动机**：核心是否该用"修改 Transformer 架构的模型"替换或增强确定性引擎。

**研究现状（相关工作的能力边界）**：

| 工作 | 做到了什么 | 边界 |
|---|---|---|
| Abacus Embeddings（NeurIPS'24） | 按数字位位置编码，多位加法推到百位级、乘法大幅提升 | 长度泛化仍是悬崖（超训练长度即崩），位精确无保证 |
| Physics of Language Models 系列（Allen-Zhu） | 正确 tokenizer + 课程下四则可学到相当长度 | 训练动力学敏感，逐位正确仍需外挂验证 |
| Grokking 机制解释（Nanda'23） | 模加法被解释为傅里叶电路 | 封闭小域，不外推 |
| xVal（2023） | 数值连续编码 | 面向近似数值，非位精确 |
| Neural Algorithmic Reasoning（DeepMind） | 网络对齐经典算法的计算图（CLRS 基准） | 算法泛化的学术问题，无位精确承诺 |
| PAL / Toolformer（'22–'23） | LLM 写程序/调工具，解释器执行 | **恰是 Lug 的位置**：神经管语言，符号管算术 |

**判断**：位精确与神经网络是范畴错误——神经的价值是插值泛化，算术要求逐位
确定。让 Transformer 逐位正确执行乘法 = 用 GPU 慢速解释已有的算法，且对外
承诺精确仍必须配符号校验（否则不可卖），净收益为负。把内核换成神经模型，
Lug 就从"不会说谎的计算器"变成"又一个会算错的 AI"——丢掉的不是伪装，是灵魂。

**"Mix of Operators" 的三个版本**：

1. 字面 MoO：学习路由 + 每个算子一个神经执行器专家——学术有趣，产品负收益（见上）；
2. **MoO 作为现有注册表架构的品牌名（推荐）**：parser 做语法路由，每个 `OpDef`
   是一个"专家"，精确实现 by construction、零幻觉——把 MoE 的名字借来反着用；
3. 神经边缘层（真实有用）：① 模糊输入解析（自然语言/语音/OCR → 表达式，网络
   只做感知）；② 步骤的自然语言解释（引擎给事实，LLM 只做措辞、禁算术）；
   ③ neural estimate tier：毫秒近似 + 置信度 + 引擎验证回退。

**推荐实验（若做，scope 两周）**：20–50M 参数小模型 + digit tokenizer +
Abacus 风格位置编码，课程训练 1–12 位乘法；测长度泛化曲线与错误位分布，
与 Rust 引擎基线对比延迟/能耗。产出：一篇"我们试着用 Transformer 替换我们的
计算引擎"的实测内容（营销资产），并验证 estimate tier 可行性。

**明确不做**：GPU 上跑算术核心——违背确定性、延迟、先验账单三原则。

### 10.1 JEV 精读（typesafe.ai 博客，2026-09）

**关键纠偏：JEV 不是算术模型，是 System One 结构化决策模型**（Kahneman 命名）：

- 定位："frontier-intelligence function call"——非结构化状态进，**带校准概率的
  类型安全决策**出；classify / route / score / extract / branch
- 架构三件套：新模型架构 + **并行采样器（非自回归，单次查询出全部输出）** +
  RLCD（RL for Calibrated Decisions）——自造名，谱系：RLHF 优化"人类偏好文本"、
  RLVR 优化"可验证正确的输出"，RLCD 优化"**置信度诚实**"：模型声称的概率
  必须匹配经验准确率（说 90% 置信的事就要 90% 发生）。实现未公开，数学上
  即以 proper scoring rule（Brier/log score）为奖励的 RL；开源等价物 =
  监督训练 + conformal/温度缩放事后校准，无需 RL
- 放弃字符串生成，换来：类型安全（schema 预定义，"数学上不可能类型错误"）、
  校准置信度、70–500ms（vs LLM 3–329s）、输出 token 免费；基数上限 255
- 命名自 Jevons 悖论；创始人为前 OpenAI（RLHF/ChatGPT 方法背景）

**对 Lug 的映射**：

| JEV 组件 | 搬得动？ | Lug 版本 |
|---|---|---|
| 放弃字符串 → 结构化输出 | ✅ | MoO 输出头不是 token 序列，是对 **OpDef 注册表的路由分布**——注册表即 type schema，"幻觉出未注册算子"在构造上不可能 |
| 并行采样器 | ✅ | 数字位并行预测（每位一个 head，非自回归） |
| RLCD 校准决策 | ✅ | **引擎即奖励模型**：与引擎输出精确比对 = 可验证奖励；校准用引擎在留出集打标 |
| "can't hallucinate" | ✅ | 解析前端只输出文法合法的 parse tree（schema = 文法），否则拒 |
| 70ms vs LLM 3s 的速度优势 | ❌ | 算术的对手是 **µs 级引擎**而非 3s LLM——神经核输 4–5 个数量级且更不精确 |

**核心综合**：JEV 的真正启示不是"用神经网络算数"，而是 **"输出头可以不是
token 序列"**。Lug-MoO 的正确分工：**模型决定"算什么"（解析/路由/消歧），
引擎决定"等于多少"（精确执行）**。

### 10.2 非营利前提下的路线反转

项目定位 = 研究优先（不图钱），§1.1 的商业论证降权为参考。随之调整：

- **P1 引擎升为第一优先**：它是 MoO 的基础设施——无限精确训练数据工厂
  （steps 管线天然就是 token 级 CoT 监督信号）、验证器（RLCD 的可验证奖励）、
  课程生成器（按位数/深度生成训练课程）、校准标定器。**引擎与模型不是竞争
  关系，是师生关系**：引擎教模型，模型永远过不了引擎的验证就不出门。
- P2 服务器（鉴权/计费/控制台）降级为可选：研究只需要引擎 + 数据管线 CLI。
- golden 语料角色再 +1：差分测试之外，兼作 MoO 的 eval 基准生成器。

### 10.3 MoO 实验设计 v2（对齐 JEV 三件套）

- **架构**：encoder（表达式/混乱文本，双向编码）+ 两个并行输出头：
  ① 路由头（Jev 式决策）：对槽位输出算子码分布（registry 0–79 + invalid），
  基数 ≤255 与 JEV 同级；② 数字头（阶段 B）：定长数字位并行预测，每位带置信度；
- **训练**：引擎生成课程（1→12 位乘法等，无限数据）；RLVR = 与引擎精确比对；
  校准 = 留出集按引擎真值做温度缩放/分箱标定；
- **评测**：① 长度泛化曲线（对照 Abacus）；② 校准曲线（confidence vs accuracy，
  JEV 式卖点）；③ 延迟/能耗 vs 引擎基线（预期输 4–5 个数量级，如实报告）；
- **预期诚实结果**：分布内高准确、长度泛化悬崖、校准可修——三个都值得写出来；
- **产出**：研究报告 + neural estimate tier 可行性结论（若校准成立）。

### 10.4 轻量开源路线：把 JEV 三件套逐件替换

| JEV 部件 | JEV 方案（闭源/自建） | 轻量开源等价 |
|---|---|---|
| 并行结构化输出 | 自研架构 + 并行采样器 | encoder + **每槽分类头**（NER 式槽填充）：parse-tree 模板槽位固定 = 一次前向出全部决策，无采样器、无 KV cache、无解码 |
| 类型安全 schema | 预定义 schema | 构造性保证（槽头只会输出合法算子码）；若走自回归则用语法约束解码：**Outlines / XGrammar / llama.cpp GBNF**（均开源） |
| 校准置信度 | RLCD（自研闭源） | **Conformal prediction**（MAPIE / crepes 开源）：留出集上给分布无关覆盖率保证——"置信 ≥ p ⟹ 经验准确 ≥ p"，统计有原则、无需 RL |
| 70ms 推理 | 自建集群 | 5–30M 参数模型 + ONNX Runtime / candle，CPU 亚毫秒；纯路由甚至 fastText / GBDT 就够 |
| 训练数据 | 未公开 | 引擎 = 无限精确数据工厂（§10.2 已定） |

**可效仿的现有项目**：Outlines（dottxt，FSM 约束生成）、XGrammar（MLC）、
llama.cpp GBNF、MAPIE / crepes（conformal）、fastText（最轻文本分类基线）、
spaCy NER（单遍结构化预测的工程范本）、ModernBERT（高效 encoder）、
Medusa（并行输出头研究线）、modded-nanogpt（从零训练 harness）、
ONNX Runtime / candle / burn（CPU 部署）。

**比 JEV 更轻的三个思路**：

1. **槽填充替代解码**：表达式 parse-tree 模板槽位固定，模型只做 N 个分类，
   构造性类型安全——比 JEV 的并行采样更彻底（它还需采样器，我们只需 softmax）；
2. **Conformal 替代 RLCD**：覆盖率保证一句话说清，数据需求 = 一个引擎标注的
   留出集，零 RL 基础设施；
3. **对数表估算层（零 ML）**：纯算术 estimate tier 可用 log-LUT + 插值——
   原始的 System One 算术，亚微秒、**相对误差可解析证明**（不用训练就有误差界），
   与引擎精确层构成"估算 → 验证"两级。

**边界**：ML 只配守模糊前门——良构输入的归一化是正则表达式的活；小模型唯一
值得站的位置是"混乱人类输入 → 文法合法表达式"。

### 10.5 JEV 架构重建（公开事实 → 必然推断）+ MoO v0 规格

**公开事实**（博客/FAQ，静态页与 Framer JSON 提取）：① 新架构但零细节；
② 并行采样器——"单次查询并行输出全部概率，非逐 token 自回归"；③ 彻底放弃
字符串生成，输出为预定义 schema 的类型安全值；④ 每字段基数 ≤255，高基数走
两阶段（独立打分 → 显式选择）；⑤ RLCD 训练校准置信；⑥ "Jev is neither small
nor an LLM"；⑦ 刻意不发公开基准；⑧ 自称"数据研究实验室"，数据全自产；
⑨ 续篇《The Bitterest Lesson》：task > data > compute > algorithms，
引 InstructGPT——GPT-2 尺寸模型在正确任务上碾压 GPT-3。

**必然推断**（约束条件几乎唯一确定形态）：输出 = 固定 schema 的有限分类字段
（每字段 ≤255 类）+ 全字段单次并行 + 类型错误构造性不可能 ⇒ 架构必然是：

```text
双向文本 encoder（BERT/ModernBERT 形态）
  └─ 每输出字段一个分类头（linear → softmax ≤255）
       └─ 单次前向 = "并行采样器"
高基数字段：候选独立打分（bi-encoder 点积）→ softmax  = "两阶段选择"
训练：自产数据监督 + RLCD（校准损失/RL，奖励 = stated confidence 对齐经验准确）
```

即"新架构"大概率 = **砍掉自回归 decoder、换多分类头**的 encoder-only 决策模型
（2018 年 BERT 多任务头模式的工程化），真正的创新在任务选择、schema 契约、
校准训练与系统工程——不在某个新算子。此推断与公开证据无矛盾，置信度高；
内部细节（层数/维度/参数量）未知。

**MoO v0 架构规格（开源复刻该形态，供研究）**：

```text
输入：表达式 / 混乱人类输入
├─ Tokenizer：表达式词法级小词表（~64），或 char 级
├─ Backbone：双向 encoder 6–8 层，d=256–384，RoPE，10–30M 参数
├─ 输出头（并行，单次前向）：
│   ├─ 路由头 × 槽位数：每槽 softmax(注册表 0–79 + <invalid> + <eos>)
│   ├─ 数字头（阶段 B）：先出位数，再每位 softmax(0–9 + ⊘) 并行预测
│   └─ 置信头：p(argmax) + conformal 校准（MAPIE）
└─ 高基数两阶段：别名/消歧字段独立打分后 softmax（对齐 JEV）

训练：引擎生成 (输入, 槽值) 无限监督（交叉熵）
     + RLVR（数字头与引擎精确比对）
     + 校准：留出集 conformal / 温度缩放
部署：ONNX Runtime CPU，目标 <1ms
```

**方法论对齐**：JEV 创始人的"bitterest lesson"（选对任务 > 数据 > 算力 > 算法）
恰好为 MoO 背书——MoO 的"正确任务"已定义（模型决定算什么，引擎决定等于多少），
数据由引擎无限自产，于是架构反而是最不重要的部分：encoder + 分类头，全部
开源可复刻。研究贡献点应放在**校准与长度泛化的实证**上，而非架构发明。

### 10.6 MoO 完整形态："数学 System One"（结构化输入 → 结构化输出）

**概念转念**：JEV 卖的从来不是"对"，是"**校准地知道自己多对**"。所以数学
MoO 不需要永远正确——它需要**知道何时正确**：高置信直接答，低置信交引擎。
这就是 Kahneman 意义上的 System 1 / System 2 分工：MoO = 快直觉（毫秒级、
带认证置信度），引擎 = 慢深思（微秒级、绝对精确）。

**双模块设计**：

```text
Module A「理解头」（结构化输入）：任意文本 → 文法合法的 parse tree
  encoder + 槽位分类头：槽位词表 = 注册表 0–79 + <invalid>，文法安全 by construction
  训练数据：引擎生成无限 (混乱文本, 规范表达式) 对——噪声/别名/自然语言模板合成
  解决：结构化输入这一半（自然语言、语音转写、OCR、脏输入）

Module B「计算头」（结构化输出）：规范表达式 → 类型化结果对象
  encoder + 并行数字头：符号位 + 位数 + 每位数字（softmax 0–9），单次前向全出
  输出 schema：{ digits[×n], sign, per-digit confidence, calibrated interval }
  训练：引擎课程（操作数 1→n 位）+ RLVR（与引擎逐位比对）+ conformal 校准
```

**算术对非自回归（NAT）的特殊友好性（本研究最有价值的洞察）**：
NAT 在自然语言里的死穴是**多模态性**（一个输入对应多个合法输出，边缘化困难）；
而算术**答案唯一**——NAT 的头号病在算术域天然不存在。剩下的唯一硬骨头是
**进位传播**：每位结果依赖低位进位，串行依赖 O(n)。两条对策：
① Transformer 深度预算（每层传一位进位）⇒ 深度 ≥ 位数的悬崖，与 Abacus 观察
吻合；② **硬件早有答案：先行进位/并行前缀（carry-lookahead）只需 O(log n)
深度**——给架构加并行前缀归纳偏置（K 轮 refine 迭代，每轮数字头以邻位分布
为条件重预测，即可微进位网络），是本研究最值得赌的架构假设。

**JEV 式产品契约**：类型安全永远成立（文法/槽位构造性保证）；值正确性以
**校准概率**表达（"置信 0.98 ⟹ 经验准确 ≥ 0.98"，conformal 保证）；
低置信 → 引擎接手。诚实声明：类型安全是承诺，数值正确是概率。

**定位（研究 vs 生产）**：纯算术生产场景引擎永远更快更准，MoO 的价值在——
① **研究空白**：NAT-for-arithmetic + conformal 校准 + 符号教师/验证器的完整
闭环没人做过（Abacus 是自回归、无校准、无验证）；② 混乱输入前门（引擎的
解析盲区）；③ 无网络嵌入式场景（agent 本地算术头，不乐意图 wasm/FFI 时）；
④ 品牌内容：System 1（模型）/ System 2（引擎）的双系统叙事。

**与引擎的比较协议（比较即论文图表）**：

先说破不对称：**计算器没有"准确率"属性，它有的是正确性定理**——所以比较
从来不是"谁更准"（0 失误 vs p 失误，无聊），校准置信是模型没有定理时给自己
发的"误差说明书"。有意义的比较轴只有三个：

| 轴 | 引擎 | MoO | 度量 |
|---|---|---|---|
| 输入覆盖 | 仅文法合法表达式（盲区：脏文本） | 任意文本 | 覆盖率 %（模型唯一的存在意义） |
| 选择性正确 | 100% | 置信 τ 以上的经验准确率 | risk–coverage 曲线 |
| 延迟/能耗 | ~1µs / µJ | ~0.1–1ms / mJ | 如实报告（预期全输） |

**级联评测协议**（MoO + 引擎组成 System 1→System 2 级联，这是研究的主图）：

```text
for τ in 置信阈值扫描:
    覆盖率 = P(模型置信 ≥ τ)
    系统错误率 = P(模型错 ∧ 置信 ≥ τ)        # 引擎兜底部分恒为 0
    平均延迟 = 覆盖率×模型延迟 + (1−覆盖率)×引擎延迟
画 risk–coverage 曲线 + reliability diagram（ECE）+ 长度泛化悬崖 + 进位位错误分布
```

读法：conformal 校准成立时，"错误率 ≤ ε"是可证的系统级保证，τ 是
"错误预算 ↔ 覆盖率"的旋钮；四张图无论结果好坏都是可发表的结论。

### 10.7 与传统本地计算的比较（地板、扩展包、产品形态三层）

**裸比较（干净输入、纯算术）——诚实承认本地计算全赢**：

| | 本地计算（bc/Python/CPU 指令） | 引擎 API | MoO 本地小模型 |
|---|---|---|---|
| 延迟 | ~1µs，进程内 | 网络往返 10–50ms | 0.1–1ms，进程内 |
| 正确性 | 100% | 100% | p<100% + 校准置信 |
| 费用/隐私/离线 | 免费 / ✅ / ✅ | tokens / ❌ | 免费 / ✅ / ✅ |
| 失败方式 | 异常崩溃（SyntaxError） | 错误响应 | **低置信告警**（可预告） |

**结论：干净输入的纯算术，服务端永远打不过本地——这不是弱点，是分层依据**。

**三层定位**（每层只在自己赢的轴上竞争）：

1. **地板 = 本地计算**：快、免费、私、离线。任何服务在"干净输入的纯计算"
   上都无法战胜它，不要试图战胜；
2. **MoO 本地小模型 = 本地计算的输入扩展包**：同隐私、同离线、同量级延迟，
   多吃一块本地计算完全无法处理的输入——自然语言/语音/OCR 脏文本
   （传统本地计算的失败方式是 SyntaxError 崩溃，MoO 是低置信告警 + 可回退）。
   这是 AI 对本地计算**唯一公平且能赢**的竞争场；
3. **引擎服务 = 多用户产品形态**：卖的从来不是算术（算术是地板），是步骤流
   审计、token 计费、密钥/控制台、agent 可依赖的确定性接口。

**研究比较协议**（MoO vs 传统本地算法的四代基线）：
① CPU 原生整数指令（~1ns）→ ② 软件大数算法（Karatsuba，µs）→
③ log-LUT 查表（ns，解析误差界）→ ④ MoO（0.1–1ms，校准误差）。
度量：延迟、能耗、准确率、输入覆盖、位数扩展性。预期 MoO 在前三轴全输、
在"错误行为"轴独赢——**传统计算失败 = 异常，MoO 失败 = 可预告的校准告警**，
对交互系统与 agent 而言这是不同的失败哲学。

### 10.8 场景地图：API 调用 AI 在什么情况下优于本地计算

**锚点（不可竞争区，先钉死）**：干净表达式 + 纯算术 + 单机 = 本地计算永远赢
（1µs、免费、隐私、离线）。API 的机会全部来自本地计算的**四个盲区**：

| 盲区 | 本地计算的失败方式 | API/AI 的解法 | 强度 | 场景 |
|---|---|---|---|---|
| ① 输入盲区 | SyntaxError 崩溃 | 理解层：自然语言/语音/OCR/脏文本 → 表达式 | ★★★ | 语音助手、作业批改、聊天内随手算 |
| ② 信任盲区 | 结果无质检：人写错公式/单位/溢出，错了无从知晓 | 双路校验 + 步骤流 + 错误码契约 = **可审计的计算收据** | ★★★ | agent function calling、金融、教育判分 |
| ③ 结合盲区 | 常数静态、无法与判断混合 | 计算结合新鲜数据（汇率/利率）与模糊决策 | ★★☆ | 实时换算、"邮件里三个数加起来超预算没" |
| ④ 分发盲区 | 新运算要客户端发版；版本碎片化 | **活的运算表**：`/operator` 下发编码表，客户端零升级获得新运算；全端语义一致 | ★★☆ | 运算体系持续扩张的项目（正是本计划） |

**综合场景：agent function calling 四盲区全占**——输入是 LLM 生成的混乱表达（①），
执行必须可信（②，LLM 自写代码不可信）、常结合上下文数据（③）、需要统一语义
分发（④）。"OpenAI 风格 calculate API"的形态就是给 agent 生态做确定性的手。

**伪机会排查（看似机会实为本地区）**：超大规模算术（本地大数库足够）；
低延迟要求（网络往返 10–50ms 永远输进程内 1µs）；隐私敏感（必须本地）；
离线环境（必须本地）。均应主动划出，不做无效竞争。

**弱设备的补充说明**：wasm/本地小模型正在侵蚀"无本地环境"盲区，④ 是
长期更稳的轴——语义分发一致性比"你没有 CPU"更持久。

### 10.9 科研领域：API 替代本地重计算的法则与落点

**铁律：API 能替代本地重计算的领域，都因为服务方拥有"本地不可复制物"**——
硬件、数据、摊销的巨额预计算、策展、或权威性。逐案验证：

| 领域 | API | 不可本地复制物 |
|---|---|---|
| 量子计算 | IBM Quantum / Braket | 硬件本身 |
| 蛋白结构 | AlphaFold DB | 摊销的巨额预计算 |
| 材料性质 | Materials Project（DFT） | HPC 摊销 + 社区策展 |
| 序列搜索 | NCBI BLAST | 巨型索引库 |
| 天文查询 | Gaia / SDSS | PB 级档案 |
| 符号数学 | Wolfram\|Alpha | 30 年符号栈 + 策展 |
| LLM 推理 | OpenAI API | 模型规模（当代最大案例） |
| 路径规划 | Google Maps | 全球数据 + 车队反馈 |

**Lug 式表达式求值在科研的诚实定位**：算术本身不是"大量复杂计算"（科研的
重计算是 HPC 的活；数据分析受数据本地性约束留在 Jupyter）。但存在一个
**工具层真空带：计算的可信性与可复现性**（reproducibility crisis 是科研核心
痛点，服务化产品近乎为零）。Lug 三件套恰好映射为"**计算收据**"：

| 落点 | 适配度 | 需要引擎补的能力 |
|---|---|---|
| 不确定度传播即服务（GUM） | ★★★ 每步可展示 = SSE 步骤流天作之合；校准实验室/分析化学/学生实验的真实痛点 | `Uncertain(x, σ)` 类型 + 传播规则（逐步展示灵敏度系数） |
| 可引用计算收据（computation receipt） | ★★☆ 论文补充材料可回放：固定 id + 步骤流 + 双路校验 = DOI-able 计算 | receipt 持久化端点 + 常数库版本化（CODATA 2018/2022） |
| 单位感知计算（量纲检查） | ★★☆ Mars Climate Orbiter 级错误的解法；量纲推导天然逐步 | Value 加量纲维度，量纲不匹配 = 步骤流报错 |
| 教学/实验课 | ★★☆ 步骤流即教具 | 无 |
| **不做**：数组/统计/符号计算 | 数据本地性不可逆；Jupyter 是地板（同 §10.7 锚点逻辑） | — |

**与 §10.8 盲区框架的衔接**：科研场景全部落在"②信任盲区"（计算收据/校准）
与"④分发盲区"（版本化常数/统一语义）——科研不缺算力，缺的是**可信与可复现**
的标准化层。

### 10.10 直接痛点的本质：同位替代存量昂贵工作流

**修正**：§10.9 的计算收据/不确定度传播是"创造新工作流"（要先说服用户想要
可审计性），所以不直接。JEV 之所以直接，是因为它**同位替代一个已经存在的
昂贵工作流**：LLM 做结构化决策（3–329s、贵、幻觉）→ 换成 70ms、便宜、
类型安全。痛点不用教育，存量成本就在那里。

**Lug 的同款直接痛点其实存在：agent 生态的算术现状**。替代阶梯：

```text
第一代（现状主流）：LLM 心算          —— 秒级、烧 tokens、多位数会错
第二代（改良）：LLM 写代码 + 沙箱执行  —— 仍秒级、更贵、代码有 bug、有安全面
第三代：确定性计算 API（tool call）    —— 毫秒级、近零成本、零幻觉、类型化
```

| | JEV | Lug calculate API |
|---|---|---|
| 被替代的存量 | LLM 做结构化决策 | LLM 心算 / 代码解释器做算术 |
| 替代收益 | 快 ×100、省 ×400、类型安全 | 快 ×1000、成本趋零、正确性 100% |
| 痛点直接性 | 存量工作流已存在且量大 | **同样已存在且量大**（每个 agent 都在算数） |

**推论**：Lug 的"JEV 时刻"= **把 agent 的算术从 token 里搬出来**。
function calling 已是 agent 标准接口，calculator 是最标准化的 tool 之一；
直接痛点的排序应为：agent 算术替代（主）＞ 步骤流审计（附属价值）＞
计算收据/不确定度（第二梯队，等存量存在后再谈）。

**方法论沉淀**：找直接痛点的检验式——"今天谁在为这件事付钱/付时间？
我是否同位替代（drop-in）而非要求新行为？"凡是要求用户新增工作流的，
都排在不直接的那一档。

**§10.10 补充：LLM 算术的失效三维度（"多复杂才会错"的实证形状）**

先纠偏：LLM 的算术错误不是"明显幻觉"——经典幻觉是编造事实，算术错误是
**执行滑移**：输出格式完美、只错一两位数字、甚至与上一轮答案不一致。
"不明显"恰恰是它比幻觉更危险的原因。公开评测揭示的失效形状是三个维度：

| 维度 | 失效起点（量级感知，各家模型不同且快速演进） |
|---|---|
| 位数复杂度 | 无工具时加减 ~6–10 位开始散；乘法 3×3 出错、4×4 以上明显；推理模型（长 CoT）可推远但仍 <100% |
| 表达式复杂度 | 每层运算累积误差：单步 99% 对，20 层链路 ≈ 82% 全对 |
| 语义干扰 | GSM-Symbolic 类研究：改人名/加无关细节即掉分——模式匹配被带偏，与算术难度无关 |

**三个关键性质**：① 错误静默且貌似合理（最坏的失败模式）；② 非确定性
（同一题多次答案不同）；③ **没有可靠边界**——无法预判哪道题会错。

**对替代阶梯（§10.10）的推论**：推理模型用长 CoT 买准确率，代价是每次
算术 = 秒级 + 数千 token——恰好坐实第一/二代的成本结构。确定性 API 的
卖点因此不是"平均错误率更低"，而是"**错误率 = 0 且可预判**"：agent 不必
为每道题赌一次模型状态。前沿实验室给 ChatGPT 内置计算器/代码解释器，
本身就是对这个空隙的官方承认。

### 10.11 确认：解析前端 = JEV 框架的同构实现

用户提案：多格式输入（含半口语化/语义化公式）→ parse 模型 → 超级 engine。
与 JEV 逐件对齐：

| JEV 部件 | JEV 的做法 | 解析前端的对应 |
|---|---|---|
| 非结构化状态进 | 短而密的状态段落 | 半口语化/多格式输入（自然语言、语音转写、脏表达式） |
| 类型安全出 | 预定义 schema，类型错误构造性不可能 | 输出 = 文法合法的 parse tree，**schema 就是 Lug 文法/注册表本身**（比 JEV 的 schema 更小、完全可枚举） |
| 校准概率 | RLCD 置信度 | **解析意图置信度**："log" 底数歧义、"over" 是除还是在上标——歧义时输出 top-k 解释 + 概率，低置信触发澄清而不是硬答 |
| 并行决策 | 单次查询出全部字段 | 槽位头一次前向出整棵树（或语法约束解码） |
| 两阶段高基数 | 打分 → 选择 | 罕见别名/消歧字段独立打分 |

**一处比 JEV 更彻底**：JEV 的输出是终局决策，运行时无从验证；解析前端的
输出是**可执行的符号 IR**——树可以渲染回文本做 round-trip 一致性检查、
结果可被引擎双路验证。类型安全（文法）与意图正确（校准置信）分离成两个
独立可测的契约。

**真正的难点不在架构**（encoder + 槽头 / 语法约束解码都是成熟件），在**数据**：
引擎能无限自产干净表达式，但半口语化输入的多样性需要合成（腐蚀函数：别名
替换、语序打乱、量词插入、错别字）+ 少量真实采集。研究工作量的大头在此。

**产品契约（v1 定稿，用户决策：两档制，砍掉澄清循环）**：

- 确定性格式（LaTeX/AsciiMath/干净表达式）：解析成功 → 计算；失败 →
  `expression_error`（现状错误码，与 v4 相同）；
- 神经路径（口语/歧义输入）：conf ≥ τ_auto → 正常计算（响应回显规范形 +
  `interpretation.confidence` 附加字段）；conf < τ_auto → 4xx `ambiguous_input`，
  **error 内携带 top-k 候选与各自置信度**——对人是有用提示，对 agent 是
  可编程重试依据（读 candidates 换写法重发即可），且仍是单次无状态响应，
  不是多轮澄清；
- τ_auto 不拍脑袋：conformal 在引擎标注的留出集上按目标错误预算 ε 反解
  （`LUG_PARSE_CONF_MIN` 可配）；未校准的 softmax 置信没有语义，τ 的意义
  依赖校准；
- 错误响应示例：

```json
{"error": {"message": "Input is ambiguous. Did you mean: 35 * (4+2) [0.87] or 35/4 + 2 [0.11]?",
           "type": "invalid_request_error", "param": "expression",
           "code": "ambiguous_input",
           "candidates": [{"expression": "35 * (4+2)", "confidence": 0.87},
                           {"expression": "35/4 + 2", "confidence": 0.11}]}}
```

引擎零改动——它本来就是这条管线的 System 2。

### 10.12 广泛使用的数学标记体系（输入格式清单与优先级）

| 体系 | 形态示例 | 谁在用 | 适配度 |
|---|---|---|---|
| **LaTeX** | `\frac{a}{b}` `\sum_{i=1}^{n}` `\sqrt{x}` | 学术界事实标准（论文/arXiv/StackExchange/Overleaf） | ★★★ 科研落点必配 |
| **Unicode 文本记号** | `2²` `√2` `∑ᵢ₌₁ⁿ` `π` `×` `÷` | 聊天/社交/日常输入法 | ★★★ 已部分支持（×÷），扩展上标下标 |
| **纯文本约定（AsciiMath 系）** | `sqrt(x)` `x^2` `2**3` | 教育平台/程序员/GeoGebra | ★★☆ 与 Lug 语法最近 |
| **Excel/Sheets 公式** | `=SQRT(x)` `=SUM(A1:A10)` `POWER(2,3)` | 办公人群（最大用户基数） | ★★☆ 商业/办公落点 |
| **CAS 语法** | SymPy `sqrt(x)`、Mathematica `Sqrt[x]`、Maple `i=1..n` | 科研/工程（粘贴来源） | ★★☆ |
| **MathML**（呈现/内容两版） | `<mfrac><mi>a</mi><mi>b</mi></mfrac>` | 浏览器原生/无障碍/出版 | ★★☆ 冗长，低优先 |
| **OMML** | Word 公式 XML | Word 用户（复制公式即得） | ★☆☆ 粘贴集成场景 |
| **JS Math** | `Math.sqrt(x)` `Math.pow(2,3)` | 开发者 | ★☆☆ |
| **自然语言** | "三十五乘以四" | 所有人 | Module A 的既定目标 |

**架构原则（重申 §10.4 边界）**：LaTeX/AsciiMath/CAS/Excel 都有**确定性文法**
（开源 parser 多，如 KaTeX AST、latex2sympy），优先走确定性解析；神经模型
只接格式检测失败与口语输入。格式检测 = 流水线第一个分类头（或廉价启发式：
`\frac` → LaTeX；`=` 开头大写函数 → Excel；含中文字符 → 口语）。

**多格式放大的歧义清单**（统一到 Lug 规范语义，响应回显规范形）：
`^`（幂 vs 部分语言 XOR）、隐式乘法（`2x`）、`ln/log` 底数约定、
`//`（整除 vs 注释）、逗号小数（欧洲记号 `3,14`）。

**输出侧**：科研落点（§10.9）需要步骤的 LaTeX/MathML 渲染——前端接 KaTeX
即可，引擎只需多一个"步骤 → LaTeX"的渲染函数（注册表存 LaTeX 形态即可）。

---

## 10.13 实施状态（2026-09-29，v1.0.0-alpha.1）

**已完成并验证**（2026-09-29 追加：审计日志 `audit.log` JSONL 逐请求留痕，
成功/错误都记录、先落账后播流；计量当前**全免费**——用量口径即审计口径，
计费仅为未来可选层；存量 `api_keys.json` 的 Key 与 Rust 服务端直接互通）：

| 项 | 状态 | 验证方式 |
|---|---|---|
| `lug-engine`（value/lexer/ast/registry/parser/prefix/steps/billing） | ✅ | `cargo test`：差分 105/105 逐字节 + 预期差异 2/2 + 单元 9/9 |
| 差分语料（`legacy/export_golden.py` → `tests/golden/`） | ✅ | Python 原型为可执行规格；D1/D3 预期差异显式断言 |
| 计费口径 D2（1 步 = 1 token） | ✅ | 单元 + 集成测试断言 |
| 运算预算护栏 D4（位数预估 + 表达式长度） | ✅ | 单元测试（嵌套幂 12,004,001 位拦截） |
| `lug-server`：calculate（JSON+SSE）/ operator / 静态 / Key 鉴权 / 404 文案 | ✅ | 集成测试 10 例（tower oneshot） |
| KeyStore（Python JSON 格式互通 + 原子写 0600 + 损坏拒启） | ✅ | 兼容性单元测试 |
| `lugctl` CLI（create/list/revoke） | ✅ | 端到端冒烟 |
| 部署（systemd Rust 单元 / 交叉编译 / env 样例） | ✅ | `deploy/README.md` |
| 代码质量 | ✅ | rustfmt + clippy 0 警告 |

**实现期发现并如实记录的原型行为**（差分过程中暴露）：
1. 原型 `compute` 返回值先过 `format_num`——中间结果即被 12 位舍入/整数化并
   **反馈进后续计算**（`2/3` → `0.666666666667` → `×3 = 2.000000000001`）；
   v1 逐字节复刻该行为（双重格式化语义）。
2. `10.0**308` 在原型即报 Internal consistency check failed（format_num 整数化
   使 prefix 路径变整数幂、tree 路径为浮点幂）——v1 复刻同型失败。
3. Python 3.13 的零负次幂消息为 "zero to a negative power"（非旧版
   "0 cannot be raised to a negative power"），v1 按语料对齐。

**三角函数上线 + 两个真 bug 修复（2026-09-30 追加）**：
1. **标识符词法修复**：字母序列允许后跟数字（`log2`/`log10` 此前被断成
   `log`+`2`）——标准标识符规则 `[A-Za-z_][A-Za-z0-9_]*`；
2. **IR 数值保真（D8）**：先序 IR 的数字 token 不再在构建期 format_num——
   原型在该处舍入导致 `tan(pi/4)` 双路分叉（AST=0.999999999999 vs
   prefix=1.000000000001，tan 在 π/4 附近导数为 2，12 位舍入被放大）；
   修复后舍入点统一到 compute 层，双路恢复一致，且原型两个 Internal
   consistency 崩溃案例（10.0**308 族）转为给出精确 309/618 位结果；
   差分语料相应迁移（expected_diff ok 型，D8）。

**L2 高阶运算 + 平台控制台上线（2026-09-30，v1.0.0-alpha.2）**：
- 注册表扩张落地（§3.4 分区表全部启用）：常量 56+（词法期折叠）、函数 32–47
  （f64 核 + format_num 统一 + 域错误契约）、阶乘 16（上限 10000 + Big）、
  聚合 72/73（Opaque 单步 + suppress 机制 + Env 绑定栈 + 嵌套）；
- `sum` 从 `+` 的单词别名退役（D7）；聚合含上界（Σ i=1..10）；迭代上限 10^6；
- 平台控制台（协作者并行开发的 platform.rs 域层 + 本轮补齐的 HTTP 层）：
  OAuth PKCE S256 / state 一次性 / 会话 TTL / CSRF + same-origin /
  Key owner 作用域（IDOR 防护）/ Key 生命周期审计；缺省凭据时登录禁用；
- 公网验证：log(8,2)=3、sqrt(2)=1.414213562373、5!=120、sum=385、
  max(三加五, pi*2)=8（中英混合）、sqrt(-1) 域错误契约；
- 部署注意：reqwest/rustls 引入 ring（C 代码）→ musl 交叉需要 musl-gcc
  （brew install filosottile/musl-cross/musl-cross）。

**L1 语义规则层上线（2026-09-30，v1.0.0-alpha.2）**：`semantic.rs` ——
词别名最长匹配接词法（registry::WORD_ALIASES 数据复用）、中文数字解析
（十百千万亿分节/两/零〇/点小数/ASCII 混合续接 `3万5千`）、口语负号 `负`。
全部确定性规则（§10.4 边界），13 项语义测试 + 服务器集成测试 + 公网验证
（`三加五乘二`→13、`3万5千`→35000 单 token 计量）。差分语料保持不变
（旧语法零影响）。已知边界：`sum` 词别名与未来聚合函数同名（L2 时移除）、
"3 万"带空格不续接（紧贴才合并，词法确定性优先）。

**生产部署（2026-09-30，RackNerd 204.152.193.238）**：musl 静态二进制
（rust-lld 链接，无 C 依赖）→ `/opt/lug/bin/`，systemd 切换，经
Cloudflare → Caddy → 127.0.0.1:8000 全链路验证（401 契约 / 计算 / 官网 200）。
部署期发现并修复：① tar 包 `./` 条目以 root 解压会覆盖 `/opt/lug` 属主
（www-data 失写权限）→ 解压后统一恢复属主；② 存量 `api_keys.json` 存在
**缺 owner_id 的历史记录** → `KeyRec.owner_id` 加 `serde(default)`（"local"，
与原型 `.get` 语义一致）；③ lugctl 相对路径默认在 CWD 解析 → 显式
`API_KEYS_PATH`。回滚预案：`lug.service.py.bak` + Python 文件原样保留。

**待办（按优先级）**：
1. GitHub OAuth + 会话 + CSRF + 控制台 API（参考 `legacy/platform_auth.py`）；
2. 高阶运算第一批（§3.4 注册表已就位，加 `OpDef` 即生效）；
3. 前端 `/docs` 计费文案（v4 口径 → D2）；
4. SQLite KeyStore fast-follow；
5. `precision` 参数（§3.1 超精数阶段一）。

**运维注意**：`lugctl` 与服务端共用 `api_keys.json`；运行中的服务端不感知
外部文件修改，CLI 操作需在服务端停止时进行（SQLite/控制台阶段消除）。

**§10.6 补充：1c1g 部署容量评估（RackNerd 实测口径）**：

| 方案 | 参数量 | 内存 RSS | 单次延迟 |
|---|---|---|---|
| 规则/词典（中文数字+算子别名） | 0 | ~0 | <0.1ms |
| BERT-tiny 级 + 槽头（int8） | 4.4M | ~30MB | 5–20ms |
| §10.5 规格 encoder | 10–30M | 100–200MB | 20–80ms |
| BERT-base 级 | 110M | 500MB+ | 不建议（顶内存上限） |

结论：Module A 在 1c1g 可行（10–50 QPS 串行推理）；引擎（µs 级）不受影响。
架构原则强化：**规则/词典先行**（中文数字解析、算子别名、可枚举句式覆盖大部分
口语，零模型），神经模型只兜规则失败后的模糊尾巴。推理用 candle（纯 Rust）
保住单二进制 + musl 交叉编译部署路线；int8 权重 ~10MB 随包分发；训练在本地，
服务器只推理。
