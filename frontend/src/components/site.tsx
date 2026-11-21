import * as React from "react";
import {
  ArrowRight,
  ArrowUpRight,
  Check,
  Copy,
  Languages,
  LogOut,
} from "lucide-react";
import { useI18n } from "@/i18n";
import { cn } from "@/lib/utils";

const LAB_CREST = new URL("../assets/lab530-crest.svg", import.meta.url).href;

export function Logo({ href = "/" }: { href?: string }) {
  return (
    <a href={href} className="logo" aria-label="Lug">
      <span className="logo-name">Lug</span>
    </a>
  );
}

export function Eyebrow({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return <div className={cn("eyebrow", className)}>{children}</div>;
}

export function LangToggle() {
  const { lang, setLang } = useI18n();
  return (
    <button
      type="button"
      className="language-toggle"
      onClick={() => setLang(lang === "zh" ? "en" : "zh")}
      aria-label={lang === "zh" ? "Switch to English" : "切换到中文"}
    >
      <Languages size={16} aria-hidden="true" />
      <span>{lang === "zh" ? "EN" : "中文"}</span>
    </button>
  );
}

export function CopyButton({
  getText,
  className,
}: {
  getText: () => string;
  className?: string;
}) {
  const { t } = useI18n();
  const [state, setState] = React.useState<"idle" | "done" | "fail">("idle");
  const timer = React.useRef<ReturnType<typeof setTimeout>>(undefined);
  React.useEffect(() => () => clearTimeout(timer.current), []);
  return (
    <button
      type="button"
      className={cn("copy-button", className)}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(getText());
          setState("done");
        } catch {
          setState("fail");
        }
        clearTimeout(timer.current);
        timer.current = setTimeout(() => setState("idle"), 2500);
      }}
      aria-live="polite"
    >
      {state === "done" ? (
        <Check size={14} aria-hidden="true" />
      ) : (
        <Copy size={14} aria-hidden="true" />
      )}
      {state === "done"
        ? t("已复制", "Copied")
        : state === "fail"
          ? t("请手动复制", "Copy manually")
          : t("复制", "Copy")}
    </button>
  );
}

export function SiteNav({ active }: { active?: "docs" }) {
  const { t, ts } = useI18n();
  return (
    <header className="site-header">
      <div className="site-width header-inner">
        <Logo />
        <nav
          className="header-links"
          aria-label={ts("主导航", "Main navigation")}
        >
          <a className="capability-link" href="/#capabilities">
            {t("产品", "Product")}
          </a>
          <a href="/docs" aria-current={active === "docs" ? "page" : undefined}>
            {t("文档", "Docs")}
          </a>
        </nav>
        <div className="header-actions">
          <LangToggle />
          <a className="action action-small" href="/platform">
            {t("控制台", "Console")}
            <ArrowUpRight size={15} aria-hidden="true" />
          </a>
        </div>
      </div>
    </header>
  );
}

export function AppHeader({
  page,
  onLogout,
}: {
  page: "docs" | "platform";
  onLogout?: () => void;
}) {
  const { t, ts } = useI18n();
  return (
    <header className="app-header">
      <Logo />
      <span className="header-slash" aria-hidden="true">
        /
      </span>
      <span className="app-header-label">
        {page === "docs"
          ? t("开发文档", "Documentation")
          : t("开发者控制台", "Developer console")}
      </span>
      <nav
        className="app-header-actions"
        aria-label={ts("主导航", "Main navigation")}
      >
        <a className="header-home" href="/">
          {t("官网", "Home")}
        </a>
        <a href={page === "docs" ? "/platform" : "/docs"}>
          {page === "docs" ? t("控制台", "Console") : t("文档", "Docs")}
          <ArrowUpRight size={13} aria-hidden="true" />
        </a>
        <LangToggle />
        {onLogout && (
          <button
            type="button"
            className="logout-button"
            onClick={onLogout}
            aria-label={ts("退出登录", "Sign out")}
          >
            <LogOut size={16} aria-hidden="true" />
            <span>{t("退出", "Sign out")}</span>
          </button>
        )}
      </nav>
    </header>
  );
}

export function SiteFooter() {
  const { t } = useI18n();
  return (
    <footer className="site-footer" id="site-footer">
      <div className="site-width footer-top">
        <div className="footer-brand">
          <Logo />
          <span className="footer-brand-divider" aria-hidden="true" />
          <a
            className="lab-logo"
            href="https://by530.com/"
            aria-label="Lab 530"
          >
            <img
              className="footer-crest"
              src={LAB_CREST}
              alt=""
              width={1254}
              height={1254}
              loading="lazy"
            />
            <span>Lab 530</span>
          </a>
        </div>
        <div className="footer-links">
          <a href="/docs">
            {t("API 文档", "Documentation")}
            <ArrowUpRight size={14} />
          </a>
          <a href="/platform">
            {t("开发者控制台", "Developer console")}
            <ArrowUpRight size={14} />
          </a>
        </div>
      </div>
      <div className="site-width footer-bottom">
        <span>Lug · {t("结构化计算 AI", "AI for structured calculation")}</span>
        <a href="#main">
          {t("回到顶部", "Back to top")}
          <ArrowRight size={13} className="rotate-[-90deg]" />
        </a>
      </div>
    </footer>
  );
}
