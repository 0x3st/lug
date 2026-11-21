import * as React from "react";
import {
  Activity,
  ArrowRight,
  ArrowUpRight,
  BookOpen,
  Braces,
  ChevronRight,
  Github,
  KeyRound,
  LayoutDashboard,
  LockKeyhole,
  Plus,
  RefreshCw,
  ShieldCheck,
  Terminal,
  Zap,
} from "lucide-react";
import { AppHeader, CopyButton, Eyebrow } from "@/components/site";
import { useI18n, useTitle, locale } from "@/i18n";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Dialog, DialogFooter, DialogTitle } from "@/components/ui/dialog";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { cn } from "@/lib/utils";

type UsageTotal = {
  requests: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
};
type KeyRecord = {
  id: string;
  name: string;
  prefix: string;
  status: "active" | "revoked";
  created_at: number;
  last_used: number | null;
  usage: UsageTotal;
};

type AuthSession = {
  user: { id: string; login: string; provider: "github" };
  csrf_token: string;
};
type LoginOptions = { github_enabled: boolean };

function LoginView({
  options,
  notice = "",
}: {
  options: LoginOptions;
  notice?: string;
}) {
  const { t } = useI18n();
  const [busy, setBusy] = React.useState(false);
  React.useEffect(() => {
    const restore = () => setBusy(false);
    window.addEventListener("pageshow", restore);
    return () => window.removeEventListener("pageshow", restore);
  }, []);
  return (
    <section className="login-layout" aria-labelledby="login-title">
      <div className="login-story">
        <Eyebrow>Lug</Eyebrow>
        <h1>{t("开发者控制台", "Developer console")}</h1>
        <p>
          {t(
            "管理 API Key，查看请求数与 Token 用量。",
            "Manage API keys and view request counts and token usage.",
          )}
        </p>
        <div className="login-art" aria-hidden="true">
          <code>3 + 5 × 2²</code>
          <ArrowRight size={20} />
          <strong>23</strong>
        </div>
      </div>
      <div className="login-card">
        <div className="login-icon">
          <LockKeyhole size={20} />
        </div>
        <h2 id="login-title">{t("登录控制台", "Sign in to the console")}</h2>
        <p>
          {t("使用 GitHub 账号登录。", "Sign in with your GitHub account.")}
        </p>
        <Button
          type="button"
          className="github-sign-in"
          disabled={!options.github_enabled || busy}
          onClick={() => {
            setBusy(true);
            window.location.assign("/f/a/o/platform/auth/github");
          }}
        >
          <Github size={18} aria-hidden="true" />
          {t("使用 GitHub 继续", "Continue with GitHub")}
          <ArrowUpRight size={15} aria-hidden="true" />
        </Button>
        {!options.github_enabled && (
          <p className="auth-hint">
            {t(
              "GitHub 登录尚未配置，请联系平台维护者。",
              "GitHub sign-in is not configured yet. Contact the maintainer.",
            )}
          </p>
        )}
        <div
          id="login-err"
          role="alert"
          className="mt-3 text-xs text-destructive"
        >
          {notice}
        </div>
        <p className="login-note">
          {t(
            "密钥与用量仅对你可见。",
            "Your keys and usage are private to you.",
          )}
          <br />
          {t(
            "仅读取 GitHub 账号信息用于登录，不读取代码仓库。",
            "GitHub account info for sign-in only. We do not read repositories.",
          )}
        </p>
      </div>
    </section>
  );
}

const METRICS = [
  {
    id: "requests",
    zh: "总请求数",
    en: "Total requests",
    footZh: "累计计算请求",
    footEn: "Compute requests to date",
    icon: Activity,
  },
  {
    id: "prompt_tokens",
    zh: "输入 Token",
    en: "Prompt tokens",
    footZh: "表达式输入用量",
    footEn: "Expression input usage",
    icon: Braces,
  },
  {
    id: "completion_tokens",
    zh: "输出 Token",
    en: "Completion tokens",
    footZh: "过程与结果用量",
    footEn: "Process and result usage",
    icon: Zap,
  },
  {
    id: "total_tokens",
    zh: "总 Token",
    en: "Total tokens",
    footZh: "你的密钥累计用量",
    footEn: "Usage across your keys",
    icon: LayoutDashboard,
  },
] as const;

function Dashboard({
  session,
  onLogout,
}: {
  session: AuthSession;
  onLogout: (notice?: string) => void;
}) {
  const { t, ts, lang } = useI18n();
  const loc = locale(lang);
  const [keys, setKeys] = React.useState<KeyRecord[]>([]);
  const [usage, setUsage] = React.useState<UsageTotal | null>(null);
  const [loaded, setLoaded] = React.useState(false);
  const [loadError, setLoadError] = React.useState("");
  const [syncing, setSyncing] = React.useState(false);
  const loading = React.useRef(false);
  const mounted = React.useRef(true);
  const [syncedAt, setSyncedAt] = React.useState<Date | null>(null);
  const [keyName, setKeyName] = React.useState("");
  const [creating, setCreating] = React.useState(false);
  const [created, setCreated] = React.useState<{
    name: string;
    key: string;
  } | null>(null);
  const [revokeTarget, setRevokeTarget] = React.useState<KeyRecord | null>(
    null,
  );
  const [revokeError, setRevokeError] = React.useState("");
  const [revoking, setRevoking] = React.useState(false);
  const language = React.useRef(lang);
  language.current = lang;

  const api = React.useCallback(
    async (path: string, options: RequestInit = {}) => {
      const response = await fetch(path, {
        ...options,
        credentials: "same-origin",
        headers: {
          "Content-Type": "application/json",
          "X-CSRF-Token": session.csrf_token,
          ...options.headers,
        },
      });
      const data = await response.json().catch(() => ({}));
      if (!mounted.current) throw new Error("Session changed.");
      if (response.status === 401 && data.error?.code === "invalid_session") {
        onLogout(
          language.current === "zh"
            ? "会话已过期，请重新登录。"
            : "Session expired. Please sign in again.",
        );
        throw new Error("Session expired.");
      }
      if (!response.ok)
        throw new Error(data.error?.message || `HTTP ${response.status}`);
      return data;
    },
    [onLogout, session.csrf_token],
  );

  const loadAll = React.useCallback(async () => {
    if (loading.current) return;
    loading.current = true;
    setSyncing(true);
    setLoadError("");
    try {
      const [k, u] = await Promise.all([
        api("/f/a/o/platform/keys"),
        api("/f/a/o/platform/usage"),
      ]);
      if (mounted.current) {
        setKeys(k.data);
        setUsage(u.total);
        setLoaded(true);
        setSyncedAt(new Date());
      }
    } catch (err) {
      if (mounted.current)
        setLoadError(err instanceof Error ? err.message : String(err));
    } finally {
      loading.current = false;
      if (mounted.current) setSyncing(false);
    }
  }, [api]);

  React.useEffect(() => {
    mounted.current = true;
    void loadAll();
    return () => {
      mounted.current = false;
    };
  }, [loadAll]);

  async function createKey(e: React.FormEvent) {
    e.preventDefault();
    if (creating) return;
    setCreating(true);
    setLoadError("");
    try {
      const data = await api("/f/a/o/platform/keys", {
        method: "POST",
        body: JSON.stringify({ name: keyName.trim() }),
      });
      setCreated({ name: data.name, key: data.key });
      setKeyName("");
      await loadAll();
    } catch (err) {
      if (mounted.current)
        setLoadError(
          `${ts("创建失败：", "Create failed: ")}${err instanceof Error ? err.message : String(err)}`,
        );
    } finally {
      if (mounted.current) setCreating(false);
    }
  }
  async function confirmRevoke() {
    if (!revokeTarget || revoking) return;
    setRevoking(true);
    setRevokeError("");
    try {
      await api(`/f/a/o/platform/keys/${encodeURIComponent(revokeTarget.id)}`, {
        method: "DELETE",
      });
      setRevokeTarget(null);
      await loadAll();
    } catch (err) {
      if (mounted.current)
        setRevokeError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mounted.current) setRevoking(false);
    }
  }
  return (
    <div className="workspace">
      <aside className="workspace-sidebar">
        <div className="workspace-name">
          <span className="workspace-avatar">
            <Github size={17} aria-hidden="true" />
          </span>
          <strong className="workspace-user" title={session.user.login}>
            {session.user.login}
          </strong>
        </div>
        <div className="side-label">{t("工作空间", "WORKSPACE")}</div>
        <a href="#overview" aria-current="page">
          <LayoutDashboard size={16} />
          {t("概览与密钥", "Overview & keys")}
        </a>
        <a href="/docs">
          <BookOpen size={16} />
          {t("开发文档", "Documentation")}
          <ArrowUpRight size={13} />
        </a>
        <div className="side-note">
          Lug / Developer platform
          <br />
          {t("API 密钥与用量管理", "API keys and usage")}
        </div>
      </aside>
      <div className="dashboard" id="overview">
        <div className="dashboard-breadcrumb">
          {t("工作空间", "Workspace")}
          <ChevronRight size={12} />
          {t("概览", "Overview")}
          <span className="workspace-account" title={session.user.login}>
            · @{session.user.login}
          </span>
        </div>
        <div className="dashboard-head">
          <div>
            <h1>{t("概览与密钥", "Overview & keys")}</h1>
            <p>
              {t(
                "只显示你的密钥与用量，与其他用户独立。",
                "Your keys and usage, separate from every other workspace.",
              )}
            </p>
          </div>
          <Button variant="secondary" onClick={loadAll} disabled={syncing}>
            <RefreshCw size={14} className={cn(syncing && "animate-spin")} />
            {t("刷新数据", "Refresh")}
          </Button>
        </div>
        {loadError && (
          <div
            role="alert"
            className="mb-5 rounded-md border border-destructive-border bg-destructive-soft px-4 py-3 text-xs text-destructive"
          >
            {loadError}
          </div>
        )}
        <div
          className="metrics"
          aria-label={ts("累计使用量", "Cumulative usage")}
        >
          {METRICS.map((metric) => (
            <div className="metric" key={metric.id}>
              <div className="metric-label">
                {lang === "zh" ? metric.zh : metric.en}
                <metric.icon size={13} />
              </div>
              <div className="metric-value tabular-nums">
                {usage
                  ? new Intl.NumberFormat(loc).format(usage[metric.id])
                  : "—"}
              </div>
              <div className="metric-foot">
                {lang === "zh" ? metric.footZh : metric.footEn}
              </div>
            </div>
          ))}
        </div>
        <section className="key-section" aria-labelledby="keys-title">
          <div className="key-section-head">
            <div>
              <h2 id="keys-title">
                API Keys{" "}
                <span className="key-count">{loaded ? keys.length : "—"}</span>
              </h2>
              <p>
                {t(
                  "为每个应用创建专属密钥，独立管理访问与用量。",
                  "A dedicated key for each application. Access and usage, separated.",
                )}
              </p>
            </div>
            <KeyRound
              size={22}
              strokeWidth={1.5}
              className="text-muted-foreground"
            />
          </div>
          <form className="key-create-form" onSubmit={createKey}>
            <div>
              <Label htmlFor="keyName">{t("密钥名称", "Key name")}</Label>
              <Input
                id="keyName"
                placeholder={ts(
                  "例如：my-app-production",
                  "e.g. my-app-production",
                )}
                maxLength={100}
                autoComplete="off"
                value={keyName}
                onChange={(e) => setKeyName(e.target.value)}
              />
            </div>
            <Button type="submit" disabled={creating}>
              <Plus size={15} />
              {creating
                ? t("创建中…", "Creating…")
                : t("创建密钥", "Create key")}
            </Button>
          </form>
          {loaded && keys.length === 0 ? (
            <div className="empty-keys">
              <KeyRound size={27} strokeWidth={1.3} />
              <strong>{t("还没有 API Key", "No API keys yet")}</strong>
              <p>
                {t(
                  "创建你的第一个密钥，发起第一次计算。",
                  "Create your first key and make your first computation.",
                )}
              </p>
            </div>
          ) : (
            <div className="key-table">
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t("名称", "Name")}</TableHead>
                    <TableHead>{t("密钥", "Key")}</TableHead>
                    <TableHead>{t("状态", "Status")}</TableHead>
                    <TableHead>{t("创建时间", "Created")}</TableHead>
                    <TableHead>{t("请求数", "Requests")}</TableHead>
                    <TableHead>{t("总 Token", "Total tokens")}</TableHead>
                    <TableHead>{t("操作", "Actions")}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {keys.map((key) => (
                    <TableRow key={key.id}>
                      <TableCell title={key.name}>
                        <div className="key-name-cell">
                          <KeyRound size={14} />
                          <span>{key.name}</span>
                        </div>
                      </TableCell>
                      <TableCell className="font-mono text-[11px] text-muted-foreground">
                        {key.prefix}
                      </TableCell>
                      <TableCell>
                        <Badge
                          variant={
                            key.status === "active" ? "default" : "muted"
                          }
                        >
                          {key.status === "active"
                            ? t("使用中", "Active")
                            : t("已撤销", "Revoked")}
                        </Badge>
                      </TableCell>
                      <TableCell
                        className="font-mono text-[11px] text-muted-foreground"
                        title={
                          key.last_used
                            ? `${ts("最近使用：", "Last used: ")}${new Date(key.last_used * 1000).toLocaleString(loc)}`
                            : ts("尚未使用", "Never used")
                        }
                      >
                        {new Date(key.created_at * 1000).toLocaleDateString(
                          loc,
                        )}
                      </TableCell>
                      <TableCell className="font-mono text-[11px] tabular-nums">
                        {new Intl.NumberFormat(loc).format(key.usage.requests)}
                      </TableCell>
                      <TableCell className="font-mono text-[11px] tabular-nums">
                        {new Intl.NumberFormat(loc).format(
                          key.usage.total_tokens,
                        )}
                      </TableCell>
                      <TableCell>
                        {key.status === "active" ? (
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-destructive hover:text-destructive"
                            aria-label={`${ts("撤销密钥", "Revoke key")} ${key.name}`}
                            onClick={() => {
                              setRevokeTarget(key);
                              setRevokeError("");
                            }}
                          >
                            {t("撤销", "Revoke")}
                          </Button>
                        ) : (
                          "—"
                        )}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}
          <div className="key-section-foot">
            <span>
              {t(
                "完整密钥仅在创建时显示一次。撤销后立即失效。",
                "Keys are shown once at creation. Revocations take effect immediately.",
              )}
            </span>
            <span role="status">
              {syncing
                ? t("同步中…", "Syncing…")
                : loadError
                  ? t("同步失败", "Sync failed")
                  : syncedAt
                    ? `${ts("更新于 ", "Updated at ")}${syncedAt.toLocaleTimeString(loc, { hour12: false })}`
                    : t("等待同步", "Waiting to sync")}
            </span>
          </div>
        </section>
        <div className="quick-reference">
          <div>
            <h3>
              <Terminal size={15} />
              {t("API 端点", "API endpoint")}
            </h3>
            <code>https://lug.by530.com/f/a/o/calculate</code>
          </div>
          <a href="/docs#calculate">
            <h3>
              <BookOpen size={15} />
              {t("开始接入", "Start integrating")}
              <ArrowUpRight size={14} />
            </h3>
            <p>
              {t(
                "鉴权、请求与流式输出，一次了解。",
                "Authentication, requests and streaming. All explained.",
              )}
            </p>
          </a>
        </div>
      </div>
      <Dialog
        open={created !== null}
        onOpenChange={(open) => !open && setCreated(null)}
        aria-labelledby="created-title"
        aria-describedby="created-warning"
      >
        {created && (
          <div>
            <Eyebrow>{t("密钥已创建", "KEY CREATED")}</Eyebrow>
            <DialogTitle id="created-title" className="mt-3 mb-3">
              {t("你的新密钥已就绪", "Your new key is ready")}
            </DialogTitle>
            <p
              id="created-warning"
              className="mb-5 rounded-md border border-warning-border bg-warning-soft p-3 text-xs text-warning"
            >
              {t(
                "请立即复制并安全保存。关闭窗口后，将无法再次查看完整密钥。",
                "Copy and store it now. Once this window closes, the full key cannot be viewed again.",
              )}
            </p>
            <p className="text-sm text-muted-foreground break-words">
              {created.name}
            </p>
            <div className="my-3 rounded-md border bg-secondary px-4 py-4 font-mono text-xs leading-7 select-all break-all">
              {created.key}
            </div>
            <CopyButton getText={() => created.key} />
            <DialogFooter>
              <Button onClick={() => setCreated(null)}>
                {t("我已保存", "I saved it")}
              </Button>
            </DialogFooter>
          </div>
        )}
      </Dialog>
      <Dialog
        open={revokeTarget !== null}
        onOpenChange={(open) => !open && !revoking && setRevokeTarget(null)}
        preventClose={revoking}
        aria-labelledby="revoke-title"
      >
        {revokeTarget && (
          <div>
            <Eyebrow>{t("撤销访问权限", "REVOKE ACCESS")}</Eyebrow>
            <DialogTitle id="revoke-title" className="mt-3 mb-3">
              {t("撤销这枚密钥？", "Revoke this key?")}
            </DialogTitle>
            <p className="mb-5 text-sm text-muted-foreground break-words">
              {t(
                `“${revokeTarget.name}”将立即失效。使用该密钥的应用将无法继续调用 API，此操作不可撤销。`,
                `"${revokeTarget.name}" will stop working immediately. Applications using it will lose API access. This cannot be undone.`,
              )}
            </p>
            {revokeError && (
              <div role="alert" className="mb-3 text-xs text-destructive">
                {revokeError}
              </div>
            )}
            <DialogFooter>
              <Button
                variant="secondary"
                onClick={() => setRevokeTarget(null)}
                disabled={revoking}
                autoFocus
              >
                {t("取消", "Cancel")}
              </Button>
              <Button
                variant="destructive"
                onClick={confirmRevoke}
                disabled={revoking}
              >
                {revoking
                  ? t("撤销中…", "Revoking…")
                  : t("确认撤销", "Revoke key")}
              </Button>
            </DialogFooter>
          </div>
        )}
      </Dialog>
    </div>
  );
}

export default function Platform() {
  useTitle({ zh: "Lug — 开发者控制台", en: "Lug — Developer console" });
  const { t, ts } = useI18n();
  const [session, setSession] = React.useState<AuthSession | null>(null);
  const [options, setOptions] = React.useState<LoginOptions | null>(null);
  const [startupError, setStartupError] = React.useState(false);
  const [notice, setNotice] = React.useState("");
  const [signingOut, setSigningOut] = React.useState(false);
  const [oauthError, setOAuthError] = React.useState(() =>
    new URLSearchParams(window.location.search).get("login_error"),
  );
  const oauthNotices: Record<string, string> = {
    github_cancelled: ts(
      "GitHub 授权已取消，你可以重新尝试。",
      "GitHub authorization was cancelled. You can try again.",
    ),
    invalid_oauth_state: ts(
      "登录链接已过期或无效，请重新开始。",
      "This sign-in link has expired or is invalid. Please start again.",
    ),
    github_not_configured: ts(
      "GitHub 登录尚未配置，请联系平台维护者。",
      "GitHub sign-in is not configured yet. Contact the maintainer.",
    ),
    github_unavailable: ts(
      "暂时无法完成 GitHub 登录，请稍后重试。",
      "GitHub sign-in is temporarily unavailable. Please try again.",
    ),
  };
  const refreshSession = React.useCallback(async () => {
    const response = await fetch("/f/a/o/platform/me", {
      credentials: "same-origin",
      cache: "no-store",
    });
    if (response.status === 401) {
      setSession(null);
      return;
    }
    if (!response.ok) throw new Error("Could not load your session.");
    setSession(await response.json());
    setOAuthError(null);
    setNotice("");
  }, []);
  const initialize = React.useCallback(async () => {
    setStartupError(false);
    try {
      const [response] = await Promise.all([
        fetch("/f/a/o/platform/auth", { cache: "no-store" }),
        refreshSession(),
      ]);
      if (!response.ok) throw new Error("Could not load sign-in options.");
      setOptions(await response.json());
    } catch {
      setStartupError(true);
    }
  }, [refreshSession]);
  React.useEffect(() => {
    // Migrate away from the old, script-readable session token.
    localStorage.removeItem("lug_session");
    if (new URLSearchParams(window.location.search).has("login_error")) {
      window.history.replaceState(null, "", "/platform");
    }
    void initialize();
  }, [initialize]);
  const expired = React.useCallback((message = "") => {
    setSession(null);
    setNotice(message);
  }, []);
  async function logout() {
    if (!session || signingOut) return;
    setSigningOut(true);
    try {
      const response = await fetch("/f/a/o/platform/logout", {
        method: "POST",
        credentials: "same-origin",
        headers: { "X-CSRF-Token": session.csrf_token },
      });
      if (!response.ok && response.status !== 401) throw new Error();
      expired();
    } catch {
      setNotice(ts("退出失败，请重试。", "Sign-out failed. Please try again."));
    } finally {
      setSigningOut(false);
    }
  }
  return (
    <>
      <AppHeader
        page="platform"
        onLogout={session ? () => void logout() : undefined}
      />
      <main>
        {startupError ? (
          <section className="auth-loading" role="alert">
            <h1>{t("暂时无法连接控制台", "Can't reach the console")}</h1>
            <Button onClick={() => void initialize()}>
              {t("重试", "Try again")}
            </Button>
          </section>
        ) : !options ? (
          <section className="auth-loading" role="status">
            {t("正在读取登录状态…", "Checking your session…")}
          </section>
        ) : session ? (
          <>
            {notice && (
              <p role="alert" className="auth-status">
                {notice}
              </p>
            )}
            <Dashboard
              key={session.csrf_token}
              session={session}
              onLogout={expired}
            />
          </>
        ) : (
          <LoginView
            options={options}
            notice={notice || (oauthError && oauthNotices[oauthError]) || ""}
          />
        )}
      </main>
    </>
  );
}
