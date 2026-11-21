import { useState } from "react";
import {
  ArrowDown,
  ArrowRight,
  ArrowUpRight,
  Braces,
  Check,
  CheckCheck,
  Code2,
  GitBranch,
  ListTree,
  ShieldCheck,
  Terminal,
} from "lucide-react";
import { CopyButton, Eyebrow, SiteNav, SiteFooter } from "@/components/site";
import { useI18n, useTitle } from "@/i18n";

export const EXAMPLES = [
  {
    expression: "3+5*2**(1+1)",
    display: (
      <>
        3 + 5 × 2<sup>(1 + 1)</sup>
      </>
    ),
    result: 23,
    prompt: 11,
    prefix: "+ 3 * 5 ** 2 + 1 1",
    steps: ["1 + 1 = 2", "2 ** 2 = 4", "5 * 4 = 20", "3 + 20 = 23"],
  },
  {
    expression: "128/(2+6)+3**2",
    display: (
      <>
        128 ÷ (2 + 6) + 3<sup>2</sup>
      </>
    ),
    result: 25,
    prompt: 11,
    prefix: "+ / 128 + 2 6 ** 3 2",
    steps: ["2 + 6 = 8", "128 / 8 = 16", "3 ** 2 = 9", "16 + 9 = 25"],
  },
  {
    expression: "(18+6)/3",
    display: <>(18 + 6) ÷ 3</>,
    result: 8,
    prompt: 7,
    prefix: "/ + 18 6 3",
    steps: ["18 + 6 = 24", "24 / 3 = 8"],
  },
];

function ComputationDemo() {
  const { t, ts } = useI18n();
  const [selected, setSelected] = useState(0);
  const [process, setProcess] = useState(false);
  const example = EXAMPLES[selected];
  const total = example.prompt + 1 + (process ? example.steps.length * 3 : 0);
  return (
    <section
      className="demo-window"
      aria-label={ts(
        "计算演示（预设示例，不调用 API）",
        "Computation demo (preset examples, no API request)",
      )}
    >
      <div className="demo-toolbar">
        <div className="demo-title">
          <Braces size={17} />
          <span>{t("计算视图", "Evaluation")}</span>
          <span className="example-label">{t("示例", "Demo")}</span>
        </div>
        <div
          className="mode-switch"
          role="group"
          aria-label={ts("演示模式", "Demo mode")}
        >
          <button
            type="button"
            aria-pressed={!process}
            onClick={() => setProcess(false)}
          >
            <Code2 size={14} />
            {t("结果", "Result")}
          </button>
          <button
            type="button"
            aria-pressed={process}
            onClick={() => setProcess(true)}
          >
            <ListTree size={14} />
            {t("过程", "Process")}
          </button>
        </div>
      </div>
      <div className="demo-canvas">
        <div className="expression-node">
          <div className="node-label">
            <span className="node-square" />
            {t("输入表达式", "EXPRESSION")}
          </div>
          <div className="math-expression">{example.display}</div>
          <code>{example.expression}</code>
        </div>
        <div className="flow-bridge" aria-hidden="true">
          <span />
          <div className="engine-node">
            <span className="engine-wordmark">Lug</span>
          </div>
          <span />
          <ArrowRight size={13} />
        </div>
        <div className="answer-node">
          <div className="node-label">
            {t("计算结果", "RESULT")}
            <CheckCheck size={16} />
          </div>
          <output
            className="demo-answer"
            aria-live="polite"
            aria-label={ts("示例结果", "Example result")}
          >
            {example.result}
          </output>
          <span className="verified">
            <Check size={12} />
            {t("双路验算一致", "Dual-engine verified")}
          </span>
        </div>
      </div>
      {process && (
        <div className="trace-panel">
          <div className="trace-prefix">
            <GitBranch size={14} />
            <span>IR</span>
            <code>{example.prefix}</code>
          </div>
          <ol className="trace-steps">
            {example.steps.map((step, index) => (
              <li key={step}>
                <span>{String(index + 1).padStart(2, "0")}</span>
                <code>{step}</code>
                <Check size={13} />
              </li>
            ))}
          </ol>
        </div>
      )}
      <div className="demo-footer">
        <div className="example-picker">
          <span>{t("换个示例", "Try an example")}</span>
          {EXAMPLES.map((_, index) => (
            <button
              type="button"
              key={index}
              onClick={() => setSelected(index)}
              aria-pressed={selected === index}
              aria-label={ts(`示例 ${index + 1}`, `Example ${index + 1}`)}
            >
              {String(index + 1).padStart(2, "0")}
            </button>
          ))}
        </div>
        <span className="token-count">
          <span className="status-dot" />
          {total} tokens{" "}
          <span className="token-mode">/ {process ? "SSE" : "JSON"}</span>
        </span>
      </div>
    </section>
  );
}

const CURL = `curl https://lug.by530.com/f/a/o/calculate \\
  -H "Authorization: Bearer $LUG_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{"expression": "3+5*2**(1+1)"}'`;

export default function Home() {
  const { t, lang } = useI18n();
  useTitle({ zh: "Lug，正式登场。", en: "Introducing Lug." });
  return (
    <div className="marketing-page">
      <SiteNav />
      <main id="main">
        <section className="hero site-width">
          <a className="release-label" href="#capabilities">
            {t("结构化计算 AI", "STRUCTURED CALCULATION AI")}
          </a>
          <h1 className={`hero-title ${lang === "zh" ? "hero-title-zh" : ""}`}>
            {lang === "zh" ? (
              <>
                Lug<span className="hero-comma">，</span>
                <span className="hero-title-rest">正式登场。</span>
              </>
            ) : (
              <>
                Introducing <span className="hero-name">Lug.</span>
              </>
            )}
          </h1>
          <p className="hero-description">
            {t(
              "面向结构化计算的 AI，支持表达式求值、流式计算过程与用量统计。By Lab 530",
              "AI for structured calculation, with expression evaluation, streamed steps and usage tracking. By Lab 530",
            )}
          </p>
          <div className="hero-actions">
            <a className="action" href="/platform">
              {t("开始构建", "Start building")}
              <ArrowUpRight size={16} />
            </a>
            <a className="action action-outline" href="/docs">
              {t("阅读文档", "Read the docs")}
              <ArrowRight size={16} />
            </a>
          </div>
          <div className="hero-footnote">
            <span>{t("表达式求值", "Expression evaluation")}</span>
            <i />
            <span>{t("流式计算过程", "Streamed steps")}</span>
            <i />
            <span>{t("按 Token 计量", "Token-level usage")}</span>
          </div>
        </section>
        <div className="site-width">
          <ComputationDemo />
          <div className="demo-caption">
            <span>
              {t(
                "计算示例，可切换结果与过程。",
                "Calculation example. Switch between result and process views.",
              )}
            </span>
            <ArrowDown size={15} />
          </div>
        </div>
        <section id="capabilities" className="capabilities-section site-width">
          <div className="section-heading">
            <div>
              <h2>{t("功能概览", "Features")}</h2>
            </div>
            <p>
              {t(
                "表达式求值、流式计算过程与用量统计。",
                "Expression evaluation, streamed calculation steps and usage tracking.",
              )}
            </p>
          </div>
          <div className="feature-grid">
            {[
              {
                index: "01",
                title: t("表达式求值", "Expression evaluation"),
                description: t(
                  "支持七种算术运算符、括号、运算优先级与一元正负号。",
                  "Supports seven arithmetic operators, parentheses, precedence and unary signs.",
                ),
                detail: "+  −  ×  ÷  //  %  **",
              },
              {
                index: "02",
                title: t("流式计算过程", "Streamed calculation steps"),
                description: t(
                  "默认返回计算结果；开启过程模式后，通过 SSE 返回先序表示与逐步化简。",
                  "Returns the result by default. Process mode streams the prefix representation and evaluation steps over SSE.",
                ),
                detail: "prefix → step → final",
              },
              {
                index: "03",
                title: t("密钥与用量", "Keys and usage"),
                description: t(
                  "在控制台创建或撤销 API Key，查看自己的请求数与 Token 用量。",
                  "Create or revoke API keys and view your request counts and token usage in the console.",
                ),
                detail: "Authorization: Bearer sk-…",
              },
            ].map((feature) => (
              <article className="feature" key={feature.index}>
                <div className="feature-heading">
                  <span>{feature.index}</span>
                </div>
                <h3>{feature.title}</h3>
                <p>{feature.description}</p>
                <code>{feature.detail}</code>
              </article>
            ))}
          </div>
        </section>
        <section className="integration-section site-width" id="quickstart">
          <div className="integration-copy">
            <Eyebrow>{t("API 接入", "API ACCESS")}</Eyebrow>
            <h2>{t("快速接入", "Quickstart")}</h2>
            <p>
              {t(
                "通过 API 将 Lug 接入你的应用。创建 API Key 后，即可使用 HTTP 客户端提交表达式，无需安装 SDK。",
                "Use the API to integrate Lug into your application. Create an API key and submit expressions with an HTTP client. No SDK is required.",
              )}
            </p>
            <a href="/docs#calculate">
              {t("查看接入指南", "Explore the quickstart")}
              <ArrowRight size={16} />
            </a>
            <div className="integration-note">
              <ShieldCheck size={15} />
              {t(
                "密钥仅在创建时显示一次，请安全保存。",
                "Keys are shown once. Keep yours somewhere safe.",
              )}
            </div>
          </div>
          <div className="integration-code">
            <div className="integration-code-header">
              <span>
                <Terminal size={15} />
                Terminal
              </span>
              <CopyButton getText={() => CURL} />
            </div>
            <pre tabIndex={0} aria-label="cURL">
              <code>
                <span className="code-comment">
                  #{" "}
                  {t(
                    "先设置 LUG_API_KEY 环境变量",
                    "Set your LUG_API_KEY environment variable first",
                  )}
                </span>
                {"\n\n"}
                <span className="code-verb">curl</span>
                {CURL.slice(4)}
              </code>
            </pre>
            <div className="integration-response">
              <span>
                <Check size={14} />
                {t("示例响应 · 字段节选", "Example response · excerpt")}
              </span>
              <code>
                {'{ "result": '}
                <strong>23</strong>
                {', "usage": { "total_tokens": 12 } }'}
              </code>
            </div>
          </div>
        </section>
      </main>
      <SiteFooter />
    </div>
  );
}
