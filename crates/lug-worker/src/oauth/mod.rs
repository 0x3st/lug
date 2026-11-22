//! GitHub OAuth（PKCE S256）——lug-worker 实现。
//!
//! 平移自 lug-server/src/platform.rs 的 [`GithubFlow`]（begin / consume /
//! exchange），PKCE 语义逐行保留：
//! - verifier：48 字节随机 → base64url（64 字符；RFC 7636 允许 43~128）；
//! - challenge = base64url(sha256(verifier))，`code_challenge_method=S256`；
//! - state 一次性、绑定浏览器 Cookie（`STATE_COOKIE`）、TTL 600s
//!   （[`STATE_TTL`]），query state 与 Cookie state 用常数时间比较；
//! - GitHub 凭据与 access token 只在服务端流转，绝不落库/下发；
//!   exchange 只负责 code → 已验证 [`User`]，会话换发由集成层完成。
//!
//! 与 platform.rs 的差异（均为 Workers 平台约束，非语义变更）：
//! 1. 随机源：`rand` → 平台 Web Crypto（[`random`]，wasm32 无默认 rand 后端）；
//! 2. 出站 HTTP：reqwest → worker `Fetch`，两步均 10s 超时
//!    （AbortController + Delay 竞速，见 [`fetch_json`]）；
//! 3. state 存取**不 import storage 模块**：抽象为 [`OAuthStateStore`]，
//!    由集成层把 T5 的 D1 实现注入 [`WorkerGithubFlow`]（每请求经 `Env` 构造）；
//! 4. 配置来自 worker Env：client_secret 走 `env.secret`、client_id /
//!    LUG_PUBLIC_URL 走 `env.var`（tech-notes §8）；
//! 5. trait 的 begin/consume 由同步改 async（state 落库在 D1 上是 IO）。

pub mod random;
pub mod util;

use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::task::Poll;
use std::time::Duration;

use sha2::{Digest, Sha256};
use worker::send::SendFuture;
use worker::wasm_bindgen::JsValue;
use worker::*;

use util::{base64url, const_eq, token_urlsafe, urlencode};

// ------------------------------------------------------------------ 常量

/// 平移自 platform.rs：回调路径 / state Cookie 名 / state TTL。
pub const CALLBACK_PATH: &str = "/f/a/o/platform/auth/github/callback";
pub const STATE_COOKIE: &str = "lug_oauth_state";
pub const STATE_TTL: u64 = 600;

/// Env 绑定名（wrangler `[vars]` / secret，tech-notes §6/§8）。
pub const VAR_CLIENT_ID: &str = "GITHUB_CLIENT_ID";
pub const VAR_PUBLIC_URL: &str = "LUG_PUBLIC_URL";
pub const SECRET_CLIENT_SECRET: &str = "GITHUB_CLIENT_SECRET";

/// 出站 GitHub 请求超时：reqwest `Client::timeout(10s)` 的等价物。
const GITHUB_TIMEOUT: Duration = Duration::from_secs(10);
/// 响应体上限（platform.rs::github_json_from 的 65536）。
const MAX_JSON_BODY: usize = 65536;

const PUBLIC_URL_ERR: &str =
    "LUG_PUBLIC_URL must be an HTTPS origin (HTTP is allowed only on loopback).";

// ------------------------------------------------------------------ OAuth 错误

/// OAuth 失败 → 安全的错误码（重定向到 /platform?login_error=<code>）。
/// 平移自 platform.rs::OAuthError：错误码是对外契约，不得增删改。
/// GithubCancelled 由回调路由在 GitHub 侧 `error=access_denied` 时产生（T7）。
#[derive(Clone, Debug, PartialEq)]
pub enum OAuthError {
    GithubNotConfigured,
    InvalidOauthState,
    GithubCancelled,
    GithubUnavailable,
}

impl OAuthError {
    pub fn as_str(&self) -> &'static str {
        match self {
            OAuthError::GithubNotConfigured => "github_not_configured",
            OAuthError::InvalidOauthState => "invalid_oauth_state",
            OAuthError::GithubCancelled => "github_cancelled",
            OAuthError::GithubUnavailable => "github_unavailable",
        }
    }
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ------------------------------------------------------------------ 用户

/// 已验证的 GitHub 身份（access token 不持久化、不下发）。
/// 平移自 platform.rs::User。
#[derive(Clone, Debug, PartialEq)]
pub struct User {
    pub provider: String,
    pub id: String,
    pub login: String,
}

impl User {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "provider": self.provider,
            "id": self.id,
            "login": self.login,
        })
    }
}

// ------------------------------------------------------------------ state 存储抽象

/// future 别名：必须 `Send`（axum handler 约束；Worker 绑定对象本身不是
/// Send，实现方用 `worker::send::SendFuture` 包装，Workers 单线程下安全）。
pub type StoreFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;
pub type FlowFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// OAuth state 存储抽象。
///
/// state 的存取不经本模块 import storage——集成层把 T5 的 D1 实现
/// 注入 [`WorkerGithubFlow`]。实现方责任（platform.rs StateStore 语义）：
/// - [`put`](Self::put)：`ttl_secs` 秒后 state 自动失效；
/// - [`take`](Self::take)：一次性取出——成功即删除，重放/过期/不存在
///   一律 `Err(OAuthError::InvalidOauthState)`（不向调用方区分，防枚举）。
pub trait OAuthStateStore: Send + Sync + 'static {
    /// 保存 state → verifier（TTL `ttl_secs` 秒）。
    /// 存储不可用 → `Err(OAuthError::GithubUnavailable)`（begin 由此失败）。
    fn put(
        &self,
        state: String,
        verifier: String,
        ttl_secs: u64,
    ) -> StoreFut<Result<(), OAuthError>>;
    /// 一次性取出（成功即删除）：存在且未过期 → `Ok(verifier)`。
    fn take(&self, state: String) -> StoreFut<Result<String, OAuthError>>;
}

// ------------------------------------------------------------------ 配置

/// GitHub 登录配置（校验规则平移自 platform.rs::AuthConfig，省略与流程
/// 无关的 legacy_owner_id / cookie_secure——属 T7 会话层）。
#[derive(Clone, Debug, Default)]
pub struct WorkerAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// 站点公网 origin（如 https://lug.by530.com），决定回调 redirect_uri。
    pub public_url: String,
}

fn origin_host(url: &str) -> Option<String> {
    // 极简 origin 解析（urlsplit 语义）：scheme://[userinfo@]host[:port][/path][?query][#fragment]
    let (scheme, rest) = url.split_once("://")?;
    let _ = scheme;
    let netloc = rest.split(['/', '?', '#']).next()?;
    if netloc.is_empty() || netloc.contains('@') {
        return None; // 空主机或带 userinfo（username/password）
    }
    let host = if let Some(stripped) = netloc.strip_prefix('[') {
        stripped.split(']').next()? // IPv6 字面量
    } else {
        netloc.split(':').next()?
    };
    Some(host.to_ascii_lowercase())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

impl WorkerAuthConfig {
    /// 显式构造（供测试/集成层）；校验规则与 platform.rs::from_parts 一致
    /// （省略 legacy_owner_id）。空 public_url 合法 → 未配置。
    pub fn from_parts(
        client_id: &str,
        client_secret: &str,
        public_url: &str,
    ) -> Result<Self, String> {
        let mut public_url = public_url.trim().to_string();
        if !public_url.is_empty() {
            while public_url.ends_with('/') {
                public_url.pop();
            }
            let (scheme, rest) = public_url
                .split_once("://")
                .ok_or_else(|| PUBLIC_URL_ERR.to_string())?;
            // path / query / fragment 一律不允许（rstrip('/') 之后仍不得出现）
            if rest.contains(['/', '?', '#']) {
                return Err(PUBLIC_URL_ERR.to_string());
            }
            let host = origin_host(&public_url).ok_or_else(|| PUBLIC_URL_ERR.to_string())?;
            let local = is_loopback_host(&host);
            if !(scheme == "https" || (local && scheme == "http")) {
                return Err(PUBLIC_URL_ERR.to_string());
            }
        }
        Ok(Self {
            client_id: client_id.trim().to_string(),
            client_secret: client_secret.trim().to_string(),
            public_url,
        })
    }

    /// 从 worker Env 读取（每请求一次，tech-notes §8）：
    /// client_secret 走 `env.secret`（加密绑定）；client_id / public_url 走
    /// `env.var`。绑定缺失按空串处理（→ 未配置），public_url 非法才报错——
    /// 与 platform.rs 的 env_trim+from_env 行为一致。
    pub fn from_worker_env(env: &Env) -> Result<Self, String> {
        // env.var/secret 返回 worker::Result；缺失绑定 → 空串（未配置）
        let read = |name: &str| -> String {
            env.var(name)
                .map(|v| v.to_string())
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let client_secret = env
            .secret(SECRET_CLIENT_SECRET)
            .map(|s| s.to_string())
            .unwrap_or_default();
        Self::from_parts(&read(VAR_CLIENT_ID), &client_secret, &read(VAR_PUBLIC_URL))
    }

    /// 平移自 platform.rs::github_enabled。
    pub fn github_enabled(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty() && !self.public_url.is_empty()
    }

    /// 平移自 platform.rs::redirect_uri。
    pub fn redirect_uri(&self) -> String {
        format!("{public_url}{CALLBACK_PATH}", public_url = self.public_url)
    }
}

// ------------------------------------------------------------------ 纯函数（begin/exchange 的可测核心）

/// PKCE S256：challenge = base64url(sha256(verifier))。platform.rs::begin 同式。
pub(crate) fn pkce_challenge(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// GitHub authorize URL：platform.rs::begin 的拼接逐字平移。
/// state / challenge 本身就是 base64url 字符，无需再编码。
pub(crate) fn authorize_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> String {
    format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
        urlencode(client_id),
        urlencode(redirect_uri),
        state,
        challenge,
    )
}

/// access_token 端点的 application/x-www-form-urlencoded 体。
/// platform.rs::exchange 的 form 拼接逐字平移。
pub(crate) fn token_request_form(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> String {
    [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", code_verifier),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", urlencode(v)))
    .collect::<Vec<_>>()
    .join("&")
}

// ------------------------------------------------------------------ 出站 fetch

/// 构造出站 GitHub 请求（redirect=manual，理由见下）。
fn github_request(
    method: Method,
    url: &str,
    headers: Headers,
    body: Option<String>,
) -> Result<Request, OAuthError> {
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(body) = body {
        init.with_body(Some(JsValue::from_str(&body)));
    }
    // reqwest `redirect::Policy::none()` 的等价物（凭据绝不能跟随重定向流向
    // 其它目的地）。⚠️ 语义差异（tech-notes §9）：JS fetch 的 manual 不像
    // reqwest 那样给出可读的 3xx 响应，而是 opaqueredirect——status/headers/
    // body 均不可读（status_code()=0）。GitHub 两个端点正常都不 3xx；一旦
    // 发生重定向，这里与 reqwest 的 `!status.is_success()` 同为 fail-closed
    // （→ GithubUnavailable），凭据不外流。
    init.with_redirect(RequestRedirect::Manual);
    Request::new_with_init(url, &init).map_err(|_| OAuthError::GithubUnavailable)
}

/// 出站请求 → JSON object（platform.rs::github_json_from 语义平移）：
/// 网络错误 / 非成功状态 / 响应体 > 64KiB / 非对象 JSON → GithubUnavailable。
///
/// 超时：Workers fetch 无内建 timeout 参数（reqwest `Client::timeout` 的
/// 等价物需自实现，tech-notes §9）——`Fetch::send_with_signal` 与
/// `Delay`（setTimeout 定时器）竞速，超时分支 abort 后与 reqwest 超时
/// 一样映射为 GithubUnavailable。竞速用 std 的 poll_fn 手写（本 crate 的
/// futures-util 以 default-features=false 引入，不依赖其 select）。
async fn fetch_json(req: Request) -> Result<serde_json::Value, OAuthError> {
    let controller = AbortController::default();
    let signal = controller.signal();
    // 先绑定 Fetch（future 借用 &self，临时值活不过语句）
    let fetch = Fetch::Request(req);
    let mut fetch_fut = pin!(fetch.send_with_signal(&signal));
    let mut timeout = pin!(Delay::from(GITHUB_TIMEOUT));
    // 竞速：每次唤醒先 poll fetch、再 poll Delay，先 Ready 者胜。
    let outcome = poll_fn(|cx| {
        if let Poll::Ready(resp) = fetch_fut.as_mut().poll(cx) {
            return Poll::Ready(Some(resp));
        }
        if let Poll::Ready(()) = timeout.as_mut().poll(cx) {
            return Poll::Ready(None); // 超时
        }
        Poll::Pending
    })
    .await;
    let mut resp = match outcome {
        Some(resp) => resp.map_err(|_| OAuthError::GithubUnavailable)?,
        None => {
            // 超时：丢弃 in-flight future 并 abort signal（取消子请求）。
            controller.abort();
            return Err(OAuthError::GithubUnavailable);
        }
    };
    if !(200..300).contains(&resp.status_code()) {
        return Err(OAuthError::GithubUnavailable);
    }
    let raw = resp.bytes().await.map_err(|_| OAuthError::GithubUnavailable)?;
    if raw.len() > MAX_JSON_BODY {
        return Err(OAuthError::GithubUnavailable);
    }
    match serde_json::from_slice::<serde_json::Value>(&raw) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Err(OAuthError::GithubUnavailable),
    }
}

// ------------------------------------------------------------------ GithubFlow 抽象

/// 登录流程抽象（platform.rs::GithubFlow 的 worker 版；测试替身/集成层实现）。
/// 与源差异：begin/consume 为 async（state 落库在 D1 上是 IO）。
pub trait GithubFlow: Send + Sync {
    fn enabled(&self) -> bool;
    /// 开始登录 → (state, authorize URL)；调用方对 URL 回 302。
    fn begin(&self) -> FlowFut<Result<(String, String), OAuthError>>;
    /// 一次性消费 state → PKCE verifier。
    fn consume(&self, state: String, browser_state: String) -> FlowFut<Result<String, OAuthError>>;
    /// code + verifier → 已验证身份（出站网络调用）。
    fn exchange(&self, code: String, verifier: String) -> FlowFut<Result<User, OAuthError>>;
}

// ------------------------------------------------------------------ 真实 GitHub OAuth

/// GitHub App web application flow（用户 access token，非 installation）。
/// 只调用 GET /user；access / refresh token 一律不持久化。
/// `S`（state store）须 `Clone`：每次调用克隆进独立 future（flow future
/// 需 'static + Send，不能借 `&self` 跨 await）。
#[derive(Clone)]
pub struct WorkerGithubFlow<S: OAuthStateStore> {
    pub cfg: WorkerAuthConfig,
    store: S,
}

impl<S: OAuthStateStore + Clone> WorkerGithubFlow<S> {
    pub fn new(cfg: WorkerAuthConfig, store: S) -> Self {
        Self { cfg, store }
    }

    /// platform.rs::GitHubOAuth::begin 的平移。
    /// 以值接收 self：future 须 'static + Send（不能借 &self 跨 await）。
    async fn begin_flow(self) -> Result<(String, String), OAuthError> {
        if !self.cfg.github_enabled() {
            return Err(OAuthError::GithubNotConfigured);
        }
        // PKCE：state 32 字节、verifier 48 字节随机（与 platform.rs 一致）
        let state = token_urlsafe(32);
        let verifier = token_urlsafe(48);
        self.store
            .put(state.clone(), verifier.clone(), STATE_TTL)
            .await?;
        let challenge = pkce_challenge(&verifier);
        let url = authorize_url(
            &self.cfg.client_id,
            &self.cfg.redirect_uri(),
            &state,
            &challenge,
        );
        Ok((state, url))
    }

    /// platform.rs::StateStore::consume 的语义平移：先常数时间比较 query
    /// state 与浏览器 Cookie state（在触达 store 之前），再一次性取出
    /// （TTL / 重放防护由 store 实现保证）。
    async fn consume_flow(self, state: String, browser_state: String) -> Result<String, OAuthError> {
        if state.is_empty() || browser_state.is_empty() || !const_eq(&state, &browser_state) {
            return Err(OAuthError::InvalidOauthState);
        }
        self.store.take(state).await
    }

    /// platform.rs::GitHubOAuth::exchange 的平移：
    /// ① POST github.com/login/oauth/access_token（form 编码）
    /// ② GET  api.github.com/user（Bearer token）
    /// 返回已验证身份；会话换发由集成层完成。
    async fn exchange_flow(self, code: String, verifier: String) -> Result<User, OAuthError> {
        if !self.cfg.github_enabled() {
            return Err(OAuthError::GithubNotConfigured);
        }
        let redirect_uri = self.cfg.redirect_uri();
        let body = token_request_form(
            &self.cfg.client_id,
            &self.cfg.client_secret,
            &code,
            &redirect_uri,
            &verifier,
        );

        // ① code → access token（错误/超时/过大 → github_unavailable）
        let req = github_request(
            Method::Post,
            "https://github.com/login/oauth/access_token",
            Headers::from_iter([
                ("Accept", "application/json"),
                ("Content-Type", "application/x-www-form-urlencoded"),
                // reqwest Client 的 default user_agent("Lug") 无对应物，逐请求显式携带
                ("User-Agent", "Lug"),
            ]),
            Some(body),
        )?;
        let token = fetch_json(req).await?;
        let access_token = token
            .get("access_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let has_error = token.get("error").map(|e| !e.is_null()).unwrap_or(false);
        if has_error || access_token.is_empty() {
            return Err(OAuthError::GithubUnavailable);
        }

        // ② access token → 用户档案（id / login；token 用后即弃）
        let authorization = format!("Bearer {access_token}");
        let req = github_request(
            Method::Get,
            "https://api.github.com/user",
            Headers::from_iter([
                ("Accept", "application/vnd.github+json"),
                ("Authorization", authorization.as_str()),
                ("X-GitHub-Api-Version", "2022-11-28"),
                ("User-Agent", "Lug"),
            ]),
            None,
        )?;
        let profile = fetch_json(req).await?;
        let uid = profile
            .get("id")
            .and_then(|v| v.as_u64())
            .filter(|&n| n > 0)
            .ok_or(OAuthError::GithubUnavailable)?;
        let login = profile
            .get("login")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or(OAuthError::GithubUnavailable)?;
        Ok(User {
            provider: "github".into(),
            id: uid.to_string(),
            login: login.chars().take(100).collect(),
        })
    }
}

impl<S: OAuthStateStore + Clone> GithubFlow for WorkerGithubFlow<S> {
    fn enabled(&self) -> bool {
        self.cfg.github_enabled()
    }

    fn begin(&self) -> FlowFut<Result<(String, String), OAuthError>> {
        // Worker 绑定类型不是 Send：Workers 单线程保证下用 SendFuture 包装
        // （worker::send，tech-notes §1）。
        Box::pin(SendFuture::new(self.clone().begin_flow()))
    }

    fn consume(&self, state: String, browser_state: String) -> FlowFut<Result<String, OAuthError>> {
        Box::pin(SendFuture::new(self.clone().consume_flow(state, browser_state)))
    }

    fn exchange(&self, code: String, verifier: String) -> FlowFut<Result<User, OAuthError>> {
        Box::pin(SendFuture::new(self.clone().exchange_flow(code, verifier)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    /// 测试用单次 poll 执行器：只覆盖立即完成的 future（store 为纯内存
    /// mock）；Pending 即缺陷。网络/FFI 路径（begin 的随机源、exchange 的
    /// Fetch）不在 host 上执行，其核心逻辑已拆为可测纯函数。
    fn block_on<F: Future>(fut: F) -> F::Output {
        // Waker::noop() 返回 &'static Waker
        let mut cx = Context::from_waker(Waker::noop());
        let mut fut = std::pin::pin!(fut);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("test future did not complete immediately"),
        }
    }

    /// 内存 mock store（T5 D1 实现的替身，验证 trait 契约）。
    #[derive(Clone)]
    struct FixedStore {
        verifier: Option<String>,
        put_ok: bool,
    }

    impl OAuthStateStore for FixedStore {
        fn put(
            &self,
            _state: String,
            _verifier: String,
            _ttl_secs: u64,
        ) -> StoreFut<Result<(), OAuthError>> {
            let ok = self.put_ok;
            Box::pin(std::future::ready(if ok {
                Ok(())
            } else {
                Err(OAuthError::GithubUnavailable)
            }))
        }

        fn take(&self, _state: String) -> StoreFut<Result<String, OAuthError>> {
            let verifier = self.verifier.clone();
            Box::pin(std::future::ready(
                verifier.ok_or(OAuthError::InvalidOauthState),
            ))
        }
    }

    fn test_flow(store: FixedStore) -> WorkerGithubFlow<FixedStore> {
        WorkerGithubFlow::new(
            WorkerAuthConfig::from_parts("cid", "sec", "https://lug.example").unwrap(),
            store,
        )
    }

    // ---------------- 配置（platform.rs 校验规则的平移用例） ----------------

    #[test]
    fn config_from_parts_accepts_https_origin() {
        let cfg = WorkerAuthConfig::from_parts("cid", "sec", "https://lug.by530.com").unwrap();
        assert_eq!(
            cfg.redirect_uri(),
            format!("https://lug.by530.com{CALLBACK_PATH}")
        );
        assert!(cfg.github_enabled());
    }

    #[test]
    fn config_from_parts_strips_trailing_slashes() {
        let cfg = WorkerAuthConfig::from_parts("cid", "sec", "https://lug.by530.com///").unwrap();
        assert_eq!(cfg.public_url, "https://lug.by530.com");
    }

    #[test]
    fn config_from_parts_allows_http_on_loopback_only() {
        assert!(WorkerAuthConfig::from_parts("cid", "sec", "http://localhost:8787").is_ok());
        assert!(WorkerAuthConfig::from_parts("cid", "sec", "http://127.0.0.1").is_ok());
        assert_eq!(
            WorkerAuthConfig::from_parts("cid", "sec", "http://lug.by530.com").unwrap_err(),
            PUBLIC_URL_ERR
        );
    }

    #[test]
    fn config_from_parts_rejects_path_query_fragment() {
        assert_eq!(
            WorkerAuthConfig::from_parts("cid", "sec", "https://lug.by530.com/x").unwrap_err(),
            PUBLIC_URL_ERR
        );
        assert_eq!(
            WorkerAuthConfig::from_parts("cid", "sec", "https://lug.by530.com?a=1").unwrap_err(),
            PUBLIC_URL_ERR
        );
    }

    #[test]
    fn config_from_parts_rejects_non_url_and_userinfo() {
        assert_eq!(
            WorkerAuthConfig::from_parts("cid", "sec", "lug.by530.com").unwrap_err(),
            PUBLIC_URL_ERR
        );
        assert_eq!(
            WorkerAuthConfig::from_parts("cid", "sec", "https://u:p@lug.by530.com").unwrap_err(),
            PUBLIC_URL_ERR
        );
    }

    #[test]
    fn config_empty_public_url_is_unconfigured() {
        let cfg = WorkerAuthConfig::from_parts("cid", "sec", "").unwrap();
        assert!(!cfg.github_enabled());
    }

    #[test]
    fn github_enabled_requires_all_parts() {
        assert!(
            !WorkerAuthConfig::from_parts("", "sec", "https://x.example")
                .unwrap()
                .github_enabled()
        );
        assert!(
            !WorkerAuthConfig::from_parts("cid", "", "https://x.example")
                .unwrap()
                .github_enabled()
        );
        assert!(!WorkerAuthConfig::from_parts("cid", "sec", "").unwrap().github_enabled());
    }

    // ---------------- PKCE / URL / form 纯函数 ----------------

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 Appendix B
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorize_url_encodes_and_pins_s256() {
        let url = authorize_url(
            "Iv1.client id",
            "https://lug.example/f/a/o/platform/auth/github/callback",
            "st.ate-x",
            "ch_allenge",
        );
        assert_eq!(
            url,
            "https://github.com/login/oauth/authorize?client_id=Iv1.client%20id&redirect_uri=https%3A%2F%2Flug.example%2Ff%2Fa%2Fo%2Fplatform%2Fauth%2Fgithub%2Fcallback&state=st.ate-x&code_challenge=ch_allenge&code_challenge_method=S256"
        );
    }

    #[test]
    fn token_request_form_encodes_secret_values() {
        let form = token_request_form("cid", "s e+cret", "c/ode", "https://lug.example/cb", "v&x");
        assert_eq!(
            form,
            "client_id=cid&client_secret=s%20e%2Bcret&code=c%2Fode&redirect_uri=https%3A%2F%2Flug.example%2Fcb&code_verifier=v%26x"
        );
    }

    #[test]
    fn oauth_error_codes_are_stable_contract() {
        assert_eq!(OAuthError::GithubNotConfigured.as_str(), "github_not_configured");
        assert_eq!(OAuthError::InvalidOauthState.as_str(), "invalid_oauth_state");
        assert_eq!(OAuthError::GithubCancelled.as_str(), "github_cancelled");
        assert_eq!(OAuthError::GithubUnavailable.as_str(), "github_unavailable");
        assert_eq!(OAuthError::InvalidOauthState.to_string(), "invalid_oauth_state");
    }

    #[test]
    fn user_to_json_shape() {
        let user = User {
            provider: "github".into(),
            id: "123".into(),
            login: "octocat".into(),
        };
        assert_eq!(
            user.to_json(),
            serde_json::json!({"provider": "github", "id": "123", "login": "octocat"})
        );
    }

    // ---------------- 流程门控 / store 契约（无 FFI 路径） ----------------

    #[test]
    fn begin_rejects_unconfigured_before_any_randomness() {
        // 未配置在任何随机/网络调用之前失败（与 platform.rs 门控顺序一致）
        let flow = WorkerGithubFlow::new(
            WorkerAuthConfig::default(),
            FixedStore {
                verifier: None,
                put_ok: true,
            },
        );
        assert_eq!(block_on(flow.begin()).unwrap_err(), OAuthError::GithubNotConfigured);
    }

    #[test]
    fn exchange_rejects_unconfigured_before_network() {
        let flow = WorkerGithubFlow::new(
            WorkerAuthConfig::default(),
            FixedStore {
                verifier: None,
                put_ok: true,
            },
        );
        assert_eq!(
            block_on(flow.exchange("c".into(), "v".into())).unwrap_err(),
            OAuthError::GithubNotConfigured
        );
    }

    #[test]
    fn consume_rejects_state_cookie_mismatch_before_store() {
        let flow = test_flow(FixedStore {
            verifier: Some("v".into()),
            put_ok: true,
        });
        // query state 与浏览器 Cookie state 不一致 → invalid_oauth_state
        assert_eq!(
            block_on(flow.consume("s1".into(), "s2".into())).unwrap_err(),
            OAuthError::InvalidOauthState
        );
    }

    #[test]
    fn consume_rejects_empty_parts() {
        let flow = test_flow(FixedStore {
            verifier: Some("v".into()),
            put_ok: true,
        });
        assert_eq!(
            block_on(flow.consume(String::new(), "x".into())).unwrap_err(),
            OAuthError::InvalidOauthState
        );
        assert_eq!(
            block_on(flow.consume("x".into(), String::new())).unwrap_err(),
            OAuthError::InvalidOauthState
        );
    }

    #[test]
    fn consume_returns_verifier_and_maps_store_miss_to_invalid_state() {
        let flow = test_flow(FixedStore {
            verifier: Some("ver-48".into()),
            put_ok: true,
        });
        assert_eq!(block_on(flow.consume("s".into(), "s".into())).unwrap(), "ver-48");
        // store 未命中（已消费/过期/不存在）→ 同一错误码，不区分
        let flow = test_flow(FixedStore {
            verifier: None,
            put_ok: true,
        });
        assert_eq!(
            block_on(flow.consume("s".into(), "s".into())).unwrap_err(),
            OAuthError::InvalidOauthState
        );
    }

    #[test]
    fn store_put_contract_failure_maps_to_unavailable() {
        // begin() 的 store 失败分支依赖 wasm 随机源（host 不执行），
        // 这里直接验证 put 的错误约定：不可用 → GithubUnavailable。
        let store = FixedStore {
            verifier: None,
            put_ok: false,
        };
        assert_eq!(
            block_on(store.put("s".into(), "v".into(), STATE_TTL)).unwrap_err(),
            OAuthError::GithubUnavailable
        );
        let store = FixedStore {
            verifier: None,
            put_ok: true,
        };
        assert!(block_on(store.put("s".into(), "v".into(), STATE_TTL)).is_ok());
    }

    #[test]
    fn flow_and_store_futures_are_send() {
        // Send 边界的编译期验证（axum handler 约束）；只构造、不 poll。
        fn assert_send<T: Send>(_: &T) {}
        let flow = test_flow(FixedStore {
            verifier: Some("v".into()),
            put_ok: true,
        });
        assert_send(&flow.begin());
        assert_send(&flow.consume("s".into(), "s".into()));
        assert_send(&flow.exchange("c".into(), "v".into()));
        let store = FixedStore {
            verifier: None,
            put_ok: true,
        };
        assert_send(&store.put("s".into(), "v".into(), STATE_TTL));
        assert_send(&store.take("s".into()));
    }
}
