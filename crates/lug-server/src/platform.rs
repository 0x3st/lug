//! 平台认证域逻辑：GitHub OAuth（PKCE S256）+ 内存会话存储。
//! 与 `legacy/platform_auth.py` 语义逐项对齐：
//!
//! - GitHub 凭据与 access token 只在服务端流转，绝不落库/下发；
//! - 会话为不透明 token（内存态，TTL 7 天），Cookie HttpOnly + SameSite=Lax；
//! - OAuth state 一次性、绑定浏览器 Cookie、TTL 600s，code_verifier 只存内存；
//! - 工作空间以不可变的 GitHub 用户 ID 标识（`github:<id>`），与用户名无关。

use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const CALLBACK_PATH: &str = "/f/a/o/platform/auth/github/callback";
pub const SESSION_COOKIE: &str = "lug_session";
pub const STATE_COOKIE: &str = "lug_oauth_state";
pub const STATE_TTL: u64 = 600;
pub const SESSION_TTL: u64 = 7 * 24 * 3600;

const MAX_SESSIONS: usize = 4096;
const MAX_OAUTH_STATES: usize = 2048;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ------------------------------------------------------------------ 随机 token

/// base64url（无 padding），等价 Python `token_urlsafe`。
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHA[(n >> 18) as usize & 63] as char);
        out.push(ALPHA[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHA[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHA[n as usize & 63] as char);
        }
    }
    out
}

fn token_urlsafe(nbytes: usize) -> String {
    let mut buf = vec![0u8; nbytes];
    rand::rng().fill_bytes(&mut buf);
    base64url(&buf)
}

/// application/x-www-form-urlencoded（unreserved: A-Z a-z 0-9 - _ . ~）。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 常数时间比较（等价 `hmac.compare_digest`）。
pub(crate) fn const_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ------------------------------------------------------------------ 配置

/// GitHub 登录配置（全部来自环境变量，校验与原型一致）。
#[derive(Clone, Debug, Default)]
pub struct AuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// 历史单用户密钥的迁移目标（`LUG_LEGACY_OWNER_GITHUB_ID`）。
    pub legacy_owner_id: String,
    /// 站点公网 origin（如 https://lug.by530.com），决定 Cookie Secure 与同源校验。
    pub public_url: String,
}

const PUBLIC_URL_ERR: &str =
    "LUG_PUBLIC_URL must be an HTTPS origin (HTTP is allowed only on loopback).";
const LEGACY_ID_ERR: &str = "LUG_LEGACY_OWNER_GITHUB_ID must be a positive numeric GitHub user ID.";

fn env_trim(key: &str) -> String {
    std::env::var(key).unwrap_or_default().trim().to_string()
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

impl AuthConfig {
    pub fn from_env() -> Result<Self, String> {
        let client_id = env_trim("GITHUB_CLIENT_ID");
        let client_secret = env_trim("GITHUB_CLIENT_SECRET");
        let public_url = env_trim("LUG_PUBLIC_URL");
        let legacy_owner_id = env_trim("LUG_LEGACY_OWNER_GITHUB_ID");
        Self::from_parts(&client_id, &client_secret, &public_url, &legacy_owner_id)
    }

    /// 显式构造（供测试）；校验规则与 from_env 一致。
    pub fn from_parts(
        client_id: &str,
        client_secret: &str,
        public_url: &str,
        legacy_owner_id: &str,
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
        let mut legacy = legacy_owner_id.trim().to_string();
        if !legacy.is_empty() {
            if !legacy.bytes().all(|b| b.is_ascii_digit()) {
                return Err(LEGACY_ID_ERR.to_string());
            }
            let n: u64 = legacy.parse().map_err(|_| LEGACY_ID_ERR.to_string())?;
            if n == 0 {
                return Err(LEGACY_ID_ERR.to_string());
            }
            legacy = n.to_string();
        }
        Ok(Self {
            client_id: client_id.trim().to_string(),
            client_secret: client_secret.trim().to_string(),
            legacy_owner_id: legacy,
            public_url,
        })
    }

    pub fn github_enabled(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty() && !self.public_url.is_empty()
    }

    pub fn redirect_uri(&self) -> String {
        format!("{public_url}{CALLBACK_PATH}", public_url = self.public_url)
    }

    /// Cookie Secure 判定：配置了 https 公网地址 → true；否则看请求 Host 是否本机。
    pub fn cookie_secure(&self, host: Option<&str>) -> bool {
        if !self.public_url.is_empty() {
            return self.public_url.starts_with("https://");
        }
        match host.map(str::to_ascii_lowercase) {
            Some(h) => {
                let bare = h
                    .strip_prefix('[')
                    .map(|s| s.split(']').next().unwrap_or(h.as_str()))
                    .unwrap_or(&h);
                let bare = bare.split(':').next().unwrap_or(bare);
                !is_loopback_host(bare)
            }
            None => true,
        }
    }
}

// ------------------------------------------------------------------ 用户与会话

/// 已验证的 GitHub 身份（access token 不持久化、不下发）。
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

/// 工作空间 ID：只接受已验证的 GitHub 身份（`github:<不可变数字 ID>`）。
#[allow(clippy::result_unit_err)]
pub fn owner_id(user: &User) -> Result<String, ()> {
    if user.provider != "github"
        || user.id.is_empty()
        || !user.id.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(());
    }
    let n: u64 = user.id.parse().map_err(|_| ())?;
    if n == 0 {
        return Err(());
    }
    Ok(format!("github:{n}"))
}

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub expires: u64,
    pub csrf_token: String,
    pub user: User,
    seq: u64,
}

/// 不透明、可过期、可撤销的会话。只存服务端内存。
pub struct SessionStore {
    ttl: u64,
    seq: AtomicU64,
    sessions: Mutex<HashMap<String, SessionRecord>>,
}

impl SessionStore {
    pub fn new(ttl: u64) -> Self {
        Self {
            ttl,
            seq: AtomicU64::new(0),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// 签发会话（拒绝匿名/本地身份——只给已验证的 GitHub 用户发会话）。
    pub fn create(&self, user: User) -> Result<String, String> {
        owner_id(&user).map_err(|_| "A verified GitHub identity is required.".to_string())?;
        let token = token_urlsafe(32);
        let now = unix_now();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, v| v.expires > now);
        if sessions.len() >= MAX_SESSIONS {
            // 淘汰最早创建的一条（Python dict 插入序淘汰的等价实现）
            if let Some(oldest) = sessions
                .iter()
                .min_by_key(|(_, v)| v.seq)
                .map(|(k, _)| k.clone())
            {
                sessions.remove(&oldest);
            }
        }
        sessions.insert(
            token.clone(),
            SessionRecord {
                expires: now + self.ttl,
                csrf_token: token_urlsafe(32),
                user,
                seq,
            },
        );
        Ok(token)
    }

    pub fn get(&self, token: &str) -> Option<SessionRecord> {
        if token.is_empty() {
            return None;
        }
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(token) {
            Some(r) if r.expires > unix_now() => Some(r.clone()),
            _ => {
                sessions.remove(token);
                None
            }
        }
    }

    pub fn revoke(&self, token: &str) {
        if token.is_empty() {
            return;
        }
        self.sessions.lock().unwrap().remove(token);
    }
}

// ------------------------------------------------------------------ OAuth state（PKCE）

struct PendingState {
    verifier: String,
    expires: u64,
    seq: u64,
}

/// OAuth state 存储：一次性、绑定浏览器 Cookie、TTL [`STATE_TTL`]。
pub struct StateStore {
    seq: AtomicU64,
    states: Mutex<HashMap<String, PendingState>>,
}

impl Default for StateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StateStore {
    pub fn new() -> Self {
        Self {
            seq: AtomicU64::new(0),
            states: Mutex::new(HashMap::new()),
        }
    }

    /// 新建 (state, verifier)；PKCE challenge 由调用方推导。
    pub fn create(&self) -> (String, String) {
        let state = token_urlsafe(32);
        let verifier = token_urlsafe(48);
        let now = unix_now();
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let mut states = self.states.lock().unwrap();
        states.retain(|_, v| v.expires > now);
        if states.len() >= MAX_OAUTH_STATES {
            if let Some(oldest) = states
                .iter()
                .min_by_key(|(_, v)| v.seq)
                .map(|(k, _)| k.clone())
            {
                states.remove(&oldest);
            }
        }
        states.insert(
            state.clone(),
            PendingState {
                verifier,
                expires: now + STATE_TTL,
                seq,
            },
        );
        let verifier = states.get(&state).unwrap().verifier.clone();
        (state, verifier)
    }

    /// 一次性消费：state 必须与浏览器 Cookie 常数时间相等且未过期。
    pub fn consume(&self, state: &str, browser_state: &str) -> Result<String, OAuthError> {
        if state.is_empty() || browser_state.is_empty() || !const_eq(state, browser_state) {
            return Err(OAuthError::InvalidOauthState);
        }
        let pending = self.states.lock().unwrap().remove(state);
        match pending {
            Some(p) if p.expires > unix_now() => Ok(p.verifier),
            _ => Err(OAuthError::InvalidOauthState),
        }
    }
}

// ------------------------------------------------------------------ OAuth 错误

/// OAuth 失败 → 安全的错误码（重定向到 /platform?login_error=<code>）。
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

// ------------------------------------------------------------------ GitHub 流程抽象

pub type ExchangeFut = Pin<Box<dyn Future<Output = Result<User, OAuthError>> + Send>>;

/// 登录流程抽象（真实 GitHub 流程 / 测试替身）。
pub trait GithubFlow: Send + Sync {
    fn enabled(&self) -> bool;
    /// 开始登录 → (state, authorize URL)
    fn begin(&self) -> Result<(String, String), OAuthError>;
    /// 一次性消费 state → PKCE verifier
    fn consume(&self, state: &str, browser_state: &str) -> Result<String, OAuthError>;
    /// code + verifier → 已验证身份（网络调用）
    fn exchange(&self, code: String, verifier: String) -> ExchangeFut;
}

// ------------------------------------------------------------------ 真实 GitHub OAuth

/// GitHub App web application flow（用户 access token，非 installation）。
/// 只调用 GET /user；access / refresh token 一律不持久化。
pub struct GitHubOAuth {
    pub cfg: AuthConfig,
    states: StateStore,
    http: reqwest::Client,
}

impl GitHubOAuth {
    pub fn new(cfg: AuthConfig) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            // 凭据绝不能跟随重定向流向其它目的地
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Lug")
            .build()
            .map_err(|e| format!("Cannot build HTTP client: {e}"))?;
        Ok(Self {
            cfg,
            states: StateStore::new(),
            http,
        })
    }
}

async fn github_json_from(
    builder: reqwest::RequestBuilder,
) -> Result<serde_json::Value, OAuthError> {
    let resp = builder
        .send()
        .await
        .map_err(|_| OAuthError::GithubUnavailable)?;
    if !resp.status().is_success() {
        return Err(OAuthError::GithubUnavailable);
    }
    let raw = resp
        .bytes()
        .await
        .map_err(|_| OAuthError::GithubUnavailable)?;
    if raw.len() > 65536 {
        return Err(OAuthError::GithubUnavailable);
    }
    match serde_json::from_slice::<serde_json::Value>(&raw) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Err(OAuthError::GithubUnavailable),
    }
}

impl GithubFlow for GitHubOAuth {
    fn enabled(&self) -> bool {
        self.cfg.github_enabled()
    }

    fn begin(&self) -> Result<(String, String), OAuthError> {
        if !self.cfg.github_enabled() {
            return Err(OAuthError::GithubNotConfigured);
        }
        let (state, verifier) = self.states.create();
        let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
        let url = format!(
            "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256",
            urlencode(&self.cfg.client_id),
            urlencode(&self.cfg.redirect_uri()),
            state,
            challenge,
        );
        Ok((state, url))
    }

    fn consume(&self, state: &str, browser_state: &str) -> Result<String, OAuthError> {
        self.states.consume(state, browser_state)
    }

    fn exchange(&self, code: String, verifier: String) -> ExchangeFut {
        let cfg = self.cfg.clone();
        let http = self.http.clone();
        Box::pin(async move {
            if !cfg.github_enabled() {
                return Err(OAuthError::GithubNotConfigured);
            }
            let redirect_uri = cfg.redirect_uri();
            let form = [
                ("client_id", cfg.client_id.as_str()),
                ("client_secret", cfg.client_secret.as_str()),
                ("code", code.as_str()),
                ("redirect_uri", redirect_uri.as_str()),
                ("code_verifier", verifier.as_str()),
            ];
            let body = form
                .iter()
                .map(|(k, v)| format!("{k}={}", urlencode(v)))
                .collect::<Vec<_>>()
                .join("&");

            // ① code → access token（响应错误/超时/过大 → github_unavailable）
            let token = github_json_from(
                http.post("https://github.com/login/oauth/access_token")
                    .header("Accept", "application/json")
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(body),
            )
            .await?;
            let access_token = token
                .get("access_token")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let has_error = token.get("error").map(|e| !e.is_null()).unwrap_or(false);
            if has_error || access_token.is_empty() {
                return Err(OAuthError::GithubUnavailable);
            }

            // ② access token → 用户档案（id / login）
            let profile = github_json_from(
                http.get("https://api.github.com/user")
                    .header("Accept", "application/vnd.github+json")
                    .header("Authorization", format!("Bearer {access_token}"))
                    .header("X-GitHub-Api-Version", "2022-11-28"),
            )
            .await?;
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
        })
    }
}

#[cfg(test)]
mod tests;
