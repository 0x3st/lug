import * as React from "react";
import { AppHeader, CopyButton } from "@/components/site";
import { ArrowUpRight } from "lucide-react";
import { useI18n, useTitle } from "@/i18n";
import { cn } from "@/lib/utils";

const NAV = [
  {
    group: "GET STARTED",
    items: [
      { id: "introduction", zh: "概述", en: "Introduction" },
      { id: "authentication", zh: "身份验证", en: "Authentication" },
    ],
  },
  {
    group: "API REFERENCE",
    items: [
      { id: "calculate", zh: "表达式求值", en: "Evaluate", tag: "POST" },
      { id: "streaming", zh: "流式计算过程", en: "Streaming process" },
      { id: "operators", zh: "运算符编码", en: "Operator codes", tag: "GET" },
      { id: "platform", zh: "平台管理接口", en: "Platform management" },
    ],
  },
  {
    group: "RESOURCES",
    items: [
      { id: "usage", zh: "Token 用量", en: "Token usage" },
      { id: "errors", zh: "错误处理", en: "Error handling" },
    ],
  },
];

function CodeCard({
  head,
  children,
}: {
  head?: string;
  children: React.ReactNode;
}) {
  const ref = React.useRef<HTMLPreElement>(null);
  return (
    <div className="doc-code">
      {head && (
        <div className="doc-code-header">
          <span>{head}</span>
          <CopyButton getText={() => ref.current?.textContent ?? ""} />
        </div>
      )}
      <pre ref={ref} tabIndex={0}>
        {children}
      </pre>
    </div>
  );
}

function Callout({
  title,
  children,
}: {
  title: { zh: string; en: string };
  children: React.ReactNode;
}) {
  const { lang } = useI18n();
  return (
    <div className="doc-callout">
      <strong>{title[lang]}</strong>
      {children}
    </div>
  );
}

function Endpoint({ method, path }: { method: "POST" | "GET"; path: string }) {
  const { t } = useI18n();
  return (
    <div className="my-5 flex min-w-0 flex-wrap items-center gap-3.5 rounded-md bg-secondary px-4.5 py-3.5">
      <span
        className={cn(
          "shrink-0 rounded-[4px] px-2 py-1 font-mono text-[10px] font-semibold",
          method === "POST"
            ? "bg-accent-soft text-accent"
            : "bg-success-soft text-success",
        )}
      >
        {method}
      </span>
      <code className="break-all font-mono text-[13px]">{path}</code>
      <span className="ml-auto hidden font-mono text-[10px] whitespace-nowrap text-muted-foreground lg:inline">
        {t("Bearer token", "Bearer token")}
      </span>
    </div>
  );
}

function DataTable({
  head,
  rows,
}: {
  head: { zh: string; en: string }[];
  rows: React.ReactNode[][];
}) {
  const { lang } = useI18n();
  return (
    <div
      className="doc-table-wrap"
      role="region"
      aria-label={lang === "zh" ? "接口参考表" : "API reference table"}
      tabIndex={0}
    >
      <table className="w-full min-w-[440px] border-collapse text-left text-xs">
        <thead>
          <tr>
            {head.map((h, i) => (
              <th
                key={i}
                className="border-b bg-secondary/60 px-4 py-3 text-[11px] font-medium whitespace-nowrap text-muted-foreground"
              >
                {h[lang]}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={i}>
              {row.map((cell, j) => (
                <td
                  key={j}
                  className={cn(
                    "border-t px-4 py-3 align-top text-muted-foreground",
                    j === 0 && "text-foreground",
                  )}
                >
                  {cell}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const CURL_DEFAULT = `curl https://lug.by530.com/f/a/o/calculate \\
  -H "Authorization: Bearer $LUG_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{"expression": "3+5*2**(1+1)"}'`;

const CURL_STREAM = `curl -N https://lug.by530.com/f/a/o/calculate \\
  -H "Authorization: Bearer $LUG_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{"expression": "(3+5)*2", "process": true}'`;

export default function Docs() {
  const { t, ts, lang } = useI18n();
  useTitle({ zh: "Lug — API 文档", en: "Lug — API Reference" });
  const [current, setCurrent] = React.useState("introduction");

  React.useEffect(() => {
    const onScroll = () => {
      const threshold = window.innerWidth <= 750 ? 155 : 130;
      let currentId = NAV[0].items[0].id;
      for (const group of NAV) {
        for (const item of group.items) {
          const el = document.getElementById(item.id);
          if (el && el.getBoundingClientRect().top <= threshold)
            currentId = item.id;
        }
      }
      setCurrent(currentId);
    };
    onScroll();
    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("resize", onScroll);
    return () => {
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("resize", onScroll);
    };
  }, []);

  return (
    <>
      <AppHeader page="docs" />
      <div className="docs-layout">
        <nav
          className="docs-sidebar"
          aria-label={ts("文档目录", "Table of contents")}
        >
          {NAV.map((group) => (
            <React.Fragment key={group.group}>
              <div className="docs-group">
                {t(
                  group.group === "GET STARTED"
                    ? "开始使用"
                    : group.group === "API REFERENCE"
                      ? "接口参考"
                      : "开发资源",
                  group.group,
                )}
              </div>
              {group.items.map((item) => (
                <a
                  key={item.id}
                  href={`#${item.id}`}
                  aria-current={current === item.id ? "location" : undefined}
                >
                  {item.tag && (
                    <span className="mr-1.5 font-mono text-[9px] text-accent">
                      {item.tag}
                    </span>
                  )}
                  {lang === "zh" ? item.zh : item.en}
                </a>
              ))}
            </React.Fragment>
          ))}
          <div className="docs-sidebar-note">
            Lug API
            <br />
            {t("API 参考", "API reference")}
          </div>
        </nav>
        <main className="docs-content">
          <div className="mb-7 text-[11px] text-muted-foreground">
            {t("开发文档", "Developer docs")}{" "}
            <span className="mx-2.5 text-border">/</span>{" "}
            {t("API 参考", "API reference")}
          </div>

          <section id="introduction">
            <div className="mb-3 font-mono text-[10px] tracking-[1.5px] text-accent">
              DEVELOPER DOCUMENTATION
            </div>
            <h1>{t("API 参考", "API reference")}</h1>
            <p className="mt-4 mb-7 max-w-[650px] text-[15px] text-muted-foreground">
              {t(
                "通过 API 调用 Lug，提交表达式并获取结构化的计算结果。从第一个请求开始，了解身份验证、运算过程与用量统计。",
                "Access Lug through its API: submit an expression and get a structured result. Start with your first request, then explore authentication, streaming and usage.",
              )}
            </p>
            <div className="flex flex-wrap items-center gap-5 rounded-md bg-secondary px-4.5 py-3.5 max-md:block">
              <span className="mr-0 font-mono text-[10px] text-muted-foreground max-md:mb-1.5 max-md:block">
                BASE URL
              </span>
              <code className="font-mono text-xs">https://lug.by530.com</code>
            </div>
            <Callout title={{ zh: "开始之前", en: "Before you start" }}>
              {t("在", "Create an API key in the ")}
              <a
                href="/platform"
                className="text-accent underline underline-offset-2"
              >
                {t("开发者控制台", "developer console")}
              </a>
              {t(
                "创建 API Key。请求体使用 JSON，默认响应为 JSON；开启过程模式后返回 SSE 事件流。",
                ". Request bodies are JSON; responses are JSON by default and SSE event streams in process mode.",
              )}
            </Callout>
          </section>

          <section id="authentication" className="mt-12 pt-9">
            <h2 className="mb-4 text-2xl font-medium tracking-tight">
              {t("身份验证", "Authentication")}
            </h2>
            <p className="text-[13px] text-muted-foreground">
              {t(
                "计算与运算符接口使用",
                "Compute and operator endpoints use a ",
              )}
              <strong className="font-medium text-foreground">
                Bearer API Key
              </strong>
              {t("。在每次请求中添加", ". Add the ")}
              <code className="font-mono text-xs">Authorization</code>
              {t(
                "请求头；平台管理接口则使用单独的会话令牌。",
                " header on every request; platform management endpoints use a separate session token.",
              )}
            </p>
            <CodeCard head="HTTP / Request header">
              {"Authorization: Bearer sk-your-api-key"}
            </CodeCard>
            <p className="text-[13px] text-muted-foreground">
              {t(
                "密钥在创建时仅显示一次，服务端仅保存 SHA-256 哈希。请将密钥保存在服务端环境变量中，",
                "Keys are shown once at creation; the server only stores a SHA-256 hash. Keep keys in server-side environment variables — ",
              )}
              <strong className="font-medium text-foreground">
                {t(
                  "不要嵌入公开的前端代码",
                  "never embed them in public frontend code",
                )}
              </strong>
              {t(
                "。密钥丢失时，在控制台撤销并创建新密钥。",
                ". If a key leaks, revoke it in the console and create a new one.",
              )}
            </p>
          </section>

          <section id="calculate" className="mt-12 pt-9">
            <h2 className="mb-4 text-2xl font-medium tracking-tight">
              {t("表达式求值", "Evaluate an expression")}
            </h2>
            <Endpoint method="POST" path="/f/a/o/calculate" />
            <p className="text-[13px] text-muted-foreground">
              {t(
                "提交算术表达式字符串，获取计算结果。默认仅返回答案，设置",
                "Submit an arithmetic expression string and get the result. By default only the answer is returned; set ",
              )}
              <code className="font-mono text-xs">process: true</code>
              {t(" 可开启", " to enable the ")}
              <a
                href="#streaming"
                className="text-accent underline underline-offset-2"
              >
                {t("流式计算过程", "streaming process")}
              </a>
              {t("。", ".")}
            </p>
            <h3 className="mt-7 mb-3 text-[15px] font-semibold">
              {t("请求参数", "Request parameters")}
            </h3>
            <DataTable
              head={[
                { zh: "参数", en: "Parameter" },
                { zh: "类型", en: "Type" },
                { zh: "说明", en: "Description" },
              ]}
              rows={[
                [
                  <code className="font-mono text-[11px]">expression</code>,
                  <span>
                    string
                    <span className="mt-0.5 block text-[10px] text-accent">
                      {t("必填", "Required")}
                    </span>
                  </span>,
                  t(
                    "表达式字符串。支持 + - * / // % **、括号、一元正负号及幂运算右结合；也接受 × ÷ ^。",
                    "The expression string. Supports + - * / // % **, parentheses, unary signs and right-associative powers; × ÷ ^ are also accepted.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">process</code>,
                  <span>
                    boolean
                    <span className="mt-0.5 block text-[10px] text-muted-foreground">
                      {t("可选 · 默认 false", "Optional · default false")}
                    </span>
                  </span>,
                  t(
                    "设为 true 时，以 SSE 流式返回运算过程。",
                    "When true, the computation process is streamed as SSE events.",
                  ),
                ],
              ]}
            />
            <h3 className="mt-7 mb-3 text-[15px] font-semibold">
              {t("请求示例", "Request example")}
            </h3>
            <p className="text-[13px] text-muted-foreground">
              {t("将环境变量", "Set the ")}
              <code className="font-mono text-xs">LUG_API_KEY</code>
              {t(
                " 设为控制台签发的密钥。",
                " environment variable to a key from the console.",
              )}
            </p>
            <CodeCard head="Terminal / cURL">{CURL_DEFAULT}</CodeCard>
            <h3 className="mt-7 mb-3 text-[15px] font-semibold">
              {t("响应示例", "Response example")}
            </h3>
            <CodeCard
              head={`200 OK / application/json · ${t("关键字段", "key fields")}`}
            >
              {`{
  "id": "eval-4f2a...",
  "object": "evaluation",
  "model": "calculator-1",
  "expression": "3+5*2**(1+1)",
  "result": 23,
  "usage": {
    "prompt_tokens": 11,
    "completion_tokens": 1,
    "total_tokens": 12,
    "completion_tokens_details": { "reasoning_tokens": 0 }
  }
}`}
            </CodeCard>
          </section>

          <section id="streaming" className="mt-12 pt-9">
            <h2 className="mb-4 text-2xl font-medium tracking-tight">
              {t("流式计算过程", "Streaming computation process")}
            </h2>
            <p className="text-[13px] text-muted-foreground">
              {t("设置", "Set ")}
              <code className="font-mono text-xs">process: true</code>
              {t(
                "，逐步接收先序表示、子表达式化简、最终结果与用量。使用",
                " to receive the prefix form, step-by-step reductions, the final result and usage. Use ",
              )}
              <code className="font-mono text-xs">curl -N</code>
              {t(
                " 关闭客户端输出缓冲。",
                " to disable client output buffering.",
              )}
            </p>
            <CodeCard head="Terminal / cURL">{CURL_STREAM}</CodeCard>
            <DataTable
              head={[
                { zh: "事件", en: "Event" },
                { zh: "说明", en: "Description" },
              ]}
              rows={[
                [
                  <code className="font-mono text-[11px]">prefix</code>,
                  t(
                    "可读的先序（波兰式）表示。内部 IR 使用运算符编码，事件中的 prefix 使用符号展示。",
                    "A readable prefix (Polish notation) form. The internal IR uses operator codes; the prefix field uses symbols.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">step</code>,
                  t(
                    "逐步化简，包含 step、expr、operator、operator_code、result、state 和 cost_tokens。",
                    "Each reduction step with step, expr, operator, operator_code, result, state and cost_tokens.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">final</code>,
                  t(
                    "最终结果、步骤数与完整用量统计。",
                    "The final result, step count and full usage.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">[DONE]</code>,
                  t(
                    "流结束标记，不是 JSON 对象。",
                    "End-of-stream marker; not a JSON object.",
                  ),
                ],
              ]}
            />
            <CodeCard
              head={`200 OK / text/event-stream · ${t("字段节选", "excerpt")}`}
            >
              {`data: {"type": "prefix", "prefix": "* + 3 5 2"}

data: {"type": "step", "step": 1, "expr": "3+5", "result": 8}

data: {"type": "step", "step": 2, "expr": "8*2", "result": 16}

data: {"type": "final", "expression": "(3+5)*2", "result": 16}

data: [DONE]`}
            </CodeCard>
          </section>

          <section id="operators" className="mt-12 pt-9">
            <h2 className="mb-4 text-2xl font-medium tracking-tight">
              {t("运算符编码", "Operator codes")}
            </h2>
            <Endpoint method="GET" path="/f/a/o/operator" />
            <p className="text-[13px] text-muted-foreground">
              {t(
                "获取运算符编码、符号与别名对照表。计算步骤中的",
                "Returns the code, symbol and alias table. The ",
              )}
              <code className="font-mono text-xs">operator_code</code>
              {t(
                " 使用下列编码。表达式中请使用符号，而非文字别名。",
                " in each step uses these codes. Use symbols, not textual aliases, in expressions.",
              )}
            </p>
            <DataTable
              head={[
                { zh: "编码", en: "Code" },
                { zh: "符号", en: "Symbol" },
                { zh: "运算", en: "Operation" },
                { zh: "示例", en: "Example" },
              ]}
              rows={[
                [
                  "0",
                  <code className="font-mono text-[11px]">+</code>,
                  t("加法", "Addition"),
                  <code className="font-mono text-[11px]">3+5 → 8</code>,
                ],
                [
                  "1",
                  <code className="font-mono text-[11px]">-</code>,
                  t("减法", "Subtraction"),
                  <code className="font-mono text-[11px]">8-3 → 5</code>,
                ],
                [
                  "2",
                  <code className="font-mono text-[11px]">*</code>,
                  t("乘法", "Multiplication"),
                  <code className="font-mono text-[11px]">3*5 → 15</code>,
                ],
                [
                  "3",
                  <code className="font-mono text-[11px]">/</code>,
                  t("除法", "Division"),
                  <code className="font-mono text-[11px]">7/2 → 3.5</code>,
                ],
                [
                  "4",
                  <code className="font-mono text-[11px]">//</code>,
                  t("整除", "Floor division"),
                  <code className="font-mono text-[11px]">7//2 → 3</code>,
                ],
                [
                  "5",
                  <code className="font-mono text-[11px]">%</code>,
                  t("取模", "Modulo"),
                  <code className="font-mono text-[11px]">7%2 → 1</code>,
                ],
                [
                  "6",
                  <code className="font-mono text-[11px]">**</code>,
                  t("幂运算", "Power"),
                  <code className="font-mono text-[11px]">2**3 → 8</code>,
                ],
              ]}
            />
          </section>

          <section id="platform" className="mt-12 pt-9">
            <h2 className="mb-4 text-2xl font-medium tracking-tight">
              {t("平台管理接口", "Platform management")}
            </h2>
            <p className="text-[13px] text-muted-foreground">
              {t(
                "使用 GitHub 登录后，浏览器通过 HttpOnly Cookie 保持会话，有效期为 7 天。每个账号只能管理自己的密钥与用量；GitHub 用户名变更不会改变归属。管理接口与计算接口的 Bearer API Key 相互独立。",
                "Sign in with GitHub to establish a 7-day HttpOnly cookie session. Each account can access only its own keys and usage; renaming a GitHub account does not change ownership. Console sessions are separate from compute Bearer API keys.",
              )}
            </p>
            <CodeCard head="JavaScript / Same-origin console request">
              {
                'const session = await fetch("/f/a/o/platform/me").then(r => r.json());\n\nawait fetch("/f/a/o/platform/keys", {\n  method: "POST",\n  credentials: "same-origin",\n  headers: {\n    "Content-Type": "application/json",\n    "X-CSRF-Token": session.csrf_token\n  },\n  body: JSON.stringify({ name: "my-app" })\n});'
              }
            </CodeCard>
            <DataTable
              head={[
                { zh: "接口", en: "Endpoint" },
                { zh: "说明", en: "Description" },
              ]}
              rows={[
                [
                  <code className="font-mono text-[11px]">
                    GET /f/a/o/platform/auth
                  </code>,
                  t("获取当前可用的登录方式。", "Available sign-in methods."),
                ],
                [
                  <code className="font-mono text-[11px]">
                    GET /f/a/o/platform/auth/github
                  </code>,
                  t(
                    "跳转到 GitHub 授权；回调由服务端完成，令牌不会出现在前端 URL 中。",
                    "Redirect to GitHub. The server handles the callback; tokens never appear in frontend URLs.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">
                    GET /f/a/o/platform/me
                  </code>,
                  t(
                    "获取当前账号和 CSRF Token，未登录返回 401。",
                    "Current account and CSRF token; returns 401 when signed out.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">
                    POST /f/a/o/platform/logout
                  </code>,
                  t(
                    "撤销服务端会话并清除 Cookie，需要 CSRF Token。",
                    "Revoke the server session and clear the cookie. Requires a CSRF token.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">
                    GET /f/a/o/platform/keys
                  </code>,
                  t(
                    "仅列出当前账号的密钥，密钥仅展示脱敏前缀。",
                    "List only the current account’s keys, with masked prefixes.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">
                    POST /f/a/o/platform/keys
                  </code>,
                  t(
                    '提交 {"name":"my-app"} 签发密钥，明文仅在创建时返回。',
                    'Body {"name":"my-app"} issues a key; the plaintext is returned only at creation.',
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">
                    DELETE /f/a/o/platform/keys/{"{id}"}
                  </code>,
                  t(
                    "撤销自己的密钥；其他用户的密钥与不存在的 ID 均返回 404。",
                    "Revoke your own key. Other users’ keys and unknown IDs both return 404.",
                  ),
                ],
                [
                  <code className="font-mono text-[11px]">
                    GET /f/a/o/platform/usage
                  </code>,
                  t(
                    "仅统计当前账号的请求数与 Token 用量，含逐密钥统计。",
                    "Request and token usage for the current account only, including per-key totals.",
                  ),
                ],
              ]}
            />
            <p className="text-[13px] text-muted-foreground">
              {t(
                "仅支持 GitHub 登录，不提供管理员或密码登录。Cookie 会话的写操作必须携带 X-CSRF-Token。会话保存在内存中，服务重启后需重新登录；密钥和用量持久保存。",
                "GitHub is the only sign-in method; there is no admin or password login. Cookie-authenticated writes require X-CSRF-Token. Sessions live in memory and expire on restart; keys and usage persist.",
              )}
            </p>
          </section>

          <section id="usage" className="mt-12 pt-9">
            <h2 className="mb-4 text-2xl font-medium tracking-tight">
              {t("Token 用量", "Token usage")}
            </h2>
            <p className="text-[13px] text-muted-foreground">
              {t(
                "Lug 按表达式词法拆分统计 Token，而非字符数。以",
                "Lug counts tokens by lexical splitting of the expression, not by characters. For ",
              )}
              <code className="font-mono text-xs">3+5*2**(1+1)</code>
              {t(" 为例：", ":")}
            </p>
            <DataTable
              head={[
                { zh: "字段", en: "Field" },
                { zh: "统计口径", en: "Definition" },
                { zh: "默认模式", en: "Default" },
                { zh: "过程模式", en: "Process mode" },
              ]}
              rows={[
                [
                  <code className="font-mono text-[11px]">prompt_tokens</code>,
                  t("输入表达式 Token 数", "Tokens in the input expression"),
                  "11",
                  "11",
                ],
                [
                  <code className="font-mono text-[11px]">
                    reasoning_tokens
                  </code>,
                  t(
                    "各步子表达式 Token 数之和",
                    "Sum of tokens across step sub-expressions",
                  ),
                  "0",
                  "12",
                ],
                [
                  <code className="font-mono text-[11px]">
                    completion_tokens
                  </code>,
                  t(
                    "过程 Token + 1（答案）",
                    "Process tokens + 1 (the answer)",
                  ),
                  "1",
                  "13",
                ],
                [
                  <code className="font-mono text-[11px]">total_tokens</code>,
                  t("输入 + 输出 Token", "Input + output tokens"),
                  "12",
                  "24",
                ],
              ]}
            />
            <p className="text-[13px] text-muted-foreground">
              <code className="font-mono text-xs">reasoning_tokens</code>
              {t(" 位于 ", " lives in ")}
              <code className="font-mono text-xs">
                usage.completion_tokens_details
              </code>
              {t(
                " 中。累计用量可在",
                ". Cumulative usage is available in the ",
              )}
              <a
                href="/platform"
                className="text-accent underline underline-offset-2"
              >
                {t("控制台", "console")}
              </a>
              {t("查看。", ".")}
            </p>
          </section>

          <section id="errors" className="mt-12 pt-9">
            <h2 className="mb-4 text-2xl font-medium tracking-tight">
              {t("错误处理", "Error handling")}
            </h2>
            <p className="text-[13px] text-muted-foreground">
              {t(
                "接口使用 HTTP 状态码标识请求结果。错误消息为英文，响应中的",
                "HTTP status codes indicate the outcome. Error messages are in English; the ",
              )}
              <code className="font-mono text-xs">error</code>
              {t(
                " 对象包含 message、type 等字段。请优先根据 HTTP 状态码及可用的 code 处理错误。",
                " object carries message, type and more. Handle errors by status code first, then by code.",
              )}
            </p>
            <CodeCard head="400 Bad Request / application/json">
              {`{
  "error": {
    "message": "Step 1 failed: 1/0: Division by zero",
    "type": "invalid_request_error",
    "param": null,
    "code": "computation_error"
  }
}`}
            </CodeCard>
            <DataTable
              head={[
                { zh: "HTTP", en: "HTTP" },
                { zh: "Code", en: "Code" },
                { zh: "处理方式", en: "How to handle" },
              ]}
              rows={[
                [
                  "400",
                  <code className="font-mono text-[11px]">
                    expression_error
                  </code>,
                  t(
                    "检查表达式语法、括号配对与非法字符。",
                    "Check expression syntax, matching parentheses and invalid characters.",
                  ),
                ],
                [
                  "400",
                  <code className="font-mono text-[11px]">
                    computation_error
                  </code>,
                  t(
                    "检查除零、溢出与计算限制。",
                    "Check for division by zero, overflow and computation limits.",
                  ),
                ],
                [
                  "400",
                  t("其他请求错误", "Other request errors"),
                  t(
                    "检查 JSON 请求体与 expression 字段类型。",
                    "Check the JSON body and the expression field type.",
                  ),
                ],
                [
                  "401",
                  <code className="font-mono text-[11px]">
                    invalid_api_key
                  </code>,
                  t(
                    "检查 Bearer 密钥是否正确、是否已撤销。",
                    "Verify the Bearer key is correct and not revoked.",
                  ),
                ],
                [
                  "401",
                  <code className="font-mono text-[11px]">
                    invalid_session
                  </code>,
                  t(
                    "会话无效或已过期，请重新登录。",
                    "Session invalid or expired — sign in again.",
                  ),
                ],
                [
                  "404",
                  t("路径不存在", "Unknown path"),
                  t(
                    "核对请求路径与 HTTP 方法。",
                    "Verify the path and HTTP method.",
                  ),
                ],
              ]}
            />
          </section>

          <div className="mt-12 flex flex-wrap items-center justify-between gap-6 py-6 max-md:flex-col max-md:items-start">
            <div>
              <strong className="text-[13px] font-medium">
                {t("准备开始集成？", "Ready to integrate?")}
              </strong>
              <small className="mt-1.5 block text-xs text-muted-foreground">
                {t(
                  "创建第一枚密钥，发起你的第一次计算。",
                  "Create your first key and make your first computation.",
                )}
              </small>
            </div>
            <a className="action action-small" href="/platform">
              {t("进入控制台", "Open the console")} <ArrowUpRight size={14} />
            </a>
          </div>
          <footer className="mt-9 flex flex-wrap justify-between gap-4 font-mono text-[10px] text-muted-foreground">
            <span>Lug</span>
            <a href="#introduction" className="hover:text-accent">
              {t("返回顶部 ↑", "Back to top ↑")}
            </a>
          </footer>
        </main>
      </div>
    </>
  );
}
