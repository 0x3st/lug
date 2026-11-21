# Lug · 结构化计算 AI（Rust v1）

> Lug，正式登场。

Lug 是面向结构化计算的 AI，API 是将其计算能力接入应用的方式。

当前引擎**输入只支持表达式字符串**；解析成 AST 后编译为
**先序（preorder/波兰式）中间表示**（运算符编码化，可栈式计算）；逐步求值并支持
**流式输出计算过程**；**按步骤计量（1 步 = 1 token）**——当前全免费，
计量即审计（每笔计算留痕，见「用量与审计」）。

> **v1-alpha（Rust）**：本版本是设计文档 [`docs/rust-redesign.md`](docs/rust-redesign.md)
> 的落地实现。引擎与核心路由已完成并通过与 Python 原型（`legacy/`）的
> **105 例差分测试**（步骤序列逐字节一致）；GitHub OAuth / 控制台 API
> 已迁移上线（见「实施状态」），Key 也可用 `lugctl` CLI 管理。

## 启动

```bash
cargo run --release -p lug-server     # 默认 127.0.0.1:8000
PORT=9000 cargo run --release -p lug-server

# 签发 API Key（控制台可用 GitHub 登录后创建；CLI 需在服务端停止时操作）
cargo run -p lug-server --bin lugctl -- create-key my-key
```

测试：

```bash
cargo test --workspace                       # 全量（差分 + 单元 + 集成）
python3 legacy/export_golden.py              # 重新生成差分语料（改语义后）
cargo fmt --all && cargo clippy --workspace  # 0 警告基线
```

## 接口（未列出的路径一律 404）

| 路由 | 方法 | 鉴权 | 说明 |
|---|---|---|---|
| `/` · `/docs` · `/platform` | GET | 无 | 官网 / API 文档 / 开发者控制台（GitHub 登录） |
| `/assets/*` | GET | 无 | 前端构建产物（immutable 缓存） |
| `/f/a/o/calculate` | POST | API Key | 表达式求值：默认直接输出答案；`"process": true` 开启运算过程（SSE 流式） |
| `/f/a/o/operator` | GET | API Key | 运算符编码表（`0=+ 1=- 2=* 3=/ 4=// 5=% 6=**`） |

**API 鉴权**：`Authorization: Bearer sk-xxx`。服务端只存 SHA-256 哈希，明文
仅创建时返回一次；撤销即时生效。**现有 `api_keys.json`（原型生成的 Key）
无需迁移、直接可用**（格式互通）。Key 管理（v1-alpha）：

```bash
cargo run -p lug-server --bin lugctl -- create-key <名称>
cargo run -p lug-server --bin lugctl -- list-keys
cargo run -p lug-server --bin lugctl -- revoke-key <id>
```

## 输入（唯一形态）

```json
{"expression": "3+5*2**(1+1)"}
```

可选参数：`"process": true` 开启运算过程（SSE 流式输出）；默认直接输出答案。

语法与 Python 一致：`+ - * / // % **`、括号、一元正负号、`**` 右结合
（`2**3**2 = 512`，`-2**2 = -4`），也接受 `× ÷ ^` 符号。

**语义输入（L1 规则层）与高阶运算（L2）**：

```json
{"expression": "三加五乘二"}        → 13
{"expression": "3万5千"}            → 35000（续接合并为单 token）
{"expression": "两点五 * 2"}        → 5
{"expression": "3 plus 4 * 2"}     → 11
{"expression": "log(8, 2)"}        → 3
{"expression": "sqrt(2)"}          → 1.414213562373
{"expression": "5!"}               → 120
{"expression": "sum(i, 1, 10, i**2)"}  → 385
{"expression": "max(三加五, pi*2)"}    → 8
```

**L2 高阶运算**：常量（`pi` `e` `tau`）、函数（`sqrt` `cbrt` `abs` `exp` `ln`
`log2` `log10` `log(x[,b])` `sin` `cos` `tan` `floor` `ceil` `round` `min` `max`）、
后缀阶乘 `!`（非负整数，上限 10000）、聚合 `sum(var, from, to, body)` 与
`prod(...)`（边界须整数，迭代上限 100 万，Opaque 单步计费，支持嵌套）。

支持的形态：算子词别名（`加/加上/减/乘/乘以/除/除以/整除/取余/幂/乘方/
plus/minus/times/divide/modulo/power`…）、中文整数（含 十百千万亿 分节、
`两`、`零/〇`）、中文小数（`三点一四`）、ASCII 整数紧贴续接（`3万5千`）、
口语负号（`负一百`）。规则全部确定性执行（零模型零延迟）；**v1 语义变更**
见下方「与 Python 原型的差异」。

## 运算处理器流水线

```
表达式 ─tokenize→ tokens ─parse(递归下降)→ AST
    ─ast_to_prefix→ 先序 IR（运算符编码化）─栈式求值→ 结果（验算通道）
    ─evaluate_tree(后序遍历)→ 逐步化简求值 → steps（过程通道）
双路一致性校验：两路结果必须相等，否则报内部一致性错误
```

## 直接输出答案（默认，不开启运算过程）

```bash
curl -s http://127.0.0.1:8000/f/a/o/calculate \
  -H "Authorization: Bearer sk-xxx" -H "Content-Type: application/json" \
  -d '{"expression": "3+5*2**(1+1)"}'
```

```json
{
  "id": "eval-xxxx", "object": "evaluation", "created": 1700000000,
  "model": "calculator-1", "expression": "3+5*2**(1+1)",
  "result": 23,
  "usage": {"prompt_tokens": 11, "completion_tokens": 5, "total_tokens": 16,
            "completion_tokens_details": {"reasoning_tokens": 4}}
}
```

## 开启运算过程（process: true → 流式）

```bash
curl -N http://127.0.0.1:8000/f/a/o/calculate \
  -H "Authorization: Bearer sk-xxx" -H "Content-Type: application/json" \
  -d '{"expression": "(3+5)*2**2/4", "process": true}'
```

SSE 事件序列（`prefix → step×N → final → [DONE]`），**零延迟**（事件即算即发），
每步 `cost_tokens` 恒为 `1`：

```
data: {"type":"prefix","prefix":"/ * + 3 5 ** 2 2 4"}

data: {"type":"step","cost_tokens":1,"step":1,"expr":"3+5","operator":"+","operator_code":0,"result":8,"state":"8*2**2/4"}

data: {"type":"step","cost_tokens":1,"step":2,"expr":"2**2","operator":"**","operator_code":6,"result":4,"state":"8*4/4"}

data: {"type":"step","cost_tokens":1,"step":3,"expr":"8*4","operator":"*","operator_code":2,"result":32,"state":"32/4"}

data: {"type":"step","cost_tokens":1,"step":4,"expr":"32/4","operator":"/","operator_code":3,"result":8,"state":"8"}

data: {"type":"final","expression":"(3+5)*2**2/4","result":8,"steps_count":4,"usage":{...}}

data: [DONE]
```

## 用量与审计（1 步 = 1 token，当前全免费）

| 项 | 口径 | `3+5*2**(1+1)` |
|---|---|---|
| `prompt_tokens` | 表达式词法 token 数 | 11 |
| `reasoning_tokens` | **步骤数**（1 步 = 1 token） | 4 |
| `completion_tokens` | reasoning + 1（答案） | 5 |
| `total_tokens` | prompt + completion | 16 |

> v4（Python 原型）的 reasoning 是"各步子表达式 token 数之和"；
> v1 改为步骤数（设计文档差异表 D2）：用量在求值前即可精确确定，
> 且与显示层彻底解耦。**计量当前免费**，它同时是审计口径。

**审计（不能省）**：每笔请求（成功与错误都算）以 JSONL 追加到 `audit.log`
（`AUDIT_LOG_PATH` 可配）：时间戳、Key、request_id、表达式、结果/错误、
token 用量、耗时。聚合计数在 KeyStore，逐笔明细在审计日志——前者回答
"用了多少"，后者回答"每一笔是什么"。审计写入先于流式响应（先落账后播流）。

## 与 Python 原型的差异（v1 定稿语义）

| # | 类别 | 差异 |
|---|---|---|
| D1 | 语义决策 | `x`/`X` 不再是乘号别名（`×` `*` 保留） |
| D2 | 语义决策 | 计费 = 步骤数（1 步 = 1 token）；SSE `cost_tokens` 恒为 1 |
| D3 | 修复 | 浮点非有限结果（±inf/NaN）报 `Numeric overflow`（原型会输出非法 JSON 的 `Infinity`） |
| D4 | 修复 | 运算预算护栏：中间值 > 10^7 位（`LUG_MAX_VALUE_DIGITS` 可配）或表达式 > 1024 字符报错（原型嵌套幂会打爆内存） |
| D5/D6 | 新增 | 标识符/函数/`sum`/`prod`/`!` 语法与 operator 表扩容（**本 alpha 未实现**，为下一迭代） |

整数运算为**任意精度**（i64 溢出自动晋升 BigInt，与 Python 一致；
`2**10000` 照算），浮点为 f64（`0.1+0.2` 显示为 `0.3`）。

## 工程结构

```
crates/lug-engine    纯计算库：value/lexer/ast/registry/parser/prefix/steps/billing（零 I/O）
crates/lug-server    HTTP 服务：axum 路由 / Key 鉴权 / SSE / 静态资源 / lugctl CLI
legacy/              Python 原型（可执行规格）+ export_golden.py 差分语料导出
tests/golden/        差分语料（105 用例 + 2 预期差异，导出器生成）
deploy/              systemd / Caddy / Nginx / 环境变量样例 + 部署文档
docs/rust-redesign.md 完整设计文档（架构 / 数值语义 / JEV 对齐研究 / 场景地图）
```

## 部署到 VPS

单静态二进制（~1.3MB），服务器无需任何运行时。完整步骤见
[`deploy/README.md`](deploy/README.md)：

```bash
cargo zigbuild --release --target x86_64-unknown-linux-gnu   # 交叉编译
sudo cp deploy/lug.service /etc/systemd/system/lug.service   # systemd 守护
# Caddy 反代（deploy/Caddyfile）：自动 HTTPS，SSE 开箱即用
```

## 实施状态（v1-alpha，已部署生产）

- [x] `lug-engine`：三态数值（i64/BigInt/f64 自动晋升）、注册表驱动、
      双路求值 + 一致性校验、步骤流、预算护栏、步骤计费（D2）
- [x] 差分测试：105 例与 Python 原型逐字节一致（result/prefix/steps/错误文案）
- [x] `lug-server`：calculate（JSON + SSE）/ operator / 静态资源 / API Key 鉴权
      / KeyStore（与原型 JSON 格式互通，**存量 Key 直接可用**）/ `lugctl` CLI / 优雅停机
- [x] 审计日志：`audit.log` JSONL 逐请求留痕（成功/错误、先落账后播流）；
      计量当前全免费，计费仅作为未来的可选层
- [x] **L1 语义规则层（已上线）**：算子词别名 + 中文数字/小数 +
      ASCII 续接（`3万5千`）+ 口语负号（`负一百`）；规则确定性执行（§10.4
      边界：模型只兜规则失败后的尾巴）
- [x] **L2 高阶运算第一批（已上线）**：常量（pi/e/tau）+ 17 个函数 + `!` +
      `sum/prod` 聚合（Opaque 单步 + Env 绑定 + 嵌套）；`sum` 词别名退役（D7）
- [x] **平台控制台 API（已上线）**：GitHub OAuth（PKCE S256 + state 一次性 +
      浏览器绑定）+ 会话（TTL 7 天）+ CSRF/same-origin + Key 生命周期审计
      + owner 作用域隔离（IDOR 防护）；配置缺省时登录按钮禁用
- [x] 部署：**已上线** `https://lug.by530.com`（RackNerd / musl 静态二进制 /
      systemd / Caddy；交叉编译用 rust-lld，无需 zig）
- [x] 存量数据兼容：`api_keys.json`（含缺 owner_id 的历史记录）与生产 Key 直接可用
- [ ] 前端 `/docs` 页文案更新（L2 函数表 / 计量口径 / 语义输入示例）
- [ ] `lugctl` 运行中不感知外部修改：CLI 操作需在服务端停止时进行
      （`API_KEYS_PATH=/opt/lug/api_keys.json`，SQLite/控制台阶段消除）
- [ ] `precision` 参数（超精数阶段一，§3.1 路线）
- [ ] 前端 `/docs` 页计费文案更新（当前仍为 v4 口径）
- [ ] SQLite KeyStore（fast-follow，替换 JSON 单写者限制）

## 演示客户端

```bash
cargo run --release -p lug-server &
cargo run -p lug-server --bin lugctl -- create-key demo
python3 demo_client.py sk-xxx
```

## License

First-party project code is licensed under the [MIT License](LICENSE).
Third-party dependencies and assets retain their respective licenses and
notices. In particular, STIX Two Text is distributed under the
[SIL Open Font License 1.1](frontend/public/assets/stixtwotext-OFL.txt).
