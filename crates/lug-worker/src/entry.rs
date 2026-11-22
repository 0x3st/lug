//! wasm32-only 胶水层：`#[event(fetch)]` 入口、Env 绑定装配、panic 防护接线。
//!
//! 本模块只在 `target_arch = "wasm32"` 下编译（lib.rs cfg 分层）；
//! 纯逻辑一律放公共模块（routes/sse/panic_guard/platform/storage/oauth）。
//! T7.5 起本模块只做三件事：
//! 1. [`AppState::from_env`]：`env` → D1/KV/vars/secrets 绑定装配。binding 名
//!    与 wrangler.toml 逐一对齐（C9 commit 说明）：D1 = `LUGDB`、
//!    KV = `SESSIONS`、vars = `LUG_MAX_VALUE_DIGITS` / `LUG_MAX_EXPR_LEN` /
//!    `GITHUB_CLIENT_ID` / `LUG_PUBLIC_URL`，secret =
//!    `GITHUB_CLIENT_SECRET`（wrangler secret put，不进 toml）。
//!    ASSETS binding 不在本模块消费：静态请求经 Workers Static Assets
//!    资产优先路由直接命中，不进 worker（见 wrangler.toml 注释）。
//! 2. 每请求构建 `routes::router(state)`（tech-notes §1：保守做法，
//!    Env 绑定跨请求缓存的复用语义无官方承诺）；
//! 3. `panic_guard::catch_unwind_async` 包住整个 Router 调用（stable wasm32
//!    panic=abort 下无害无效果，nightly `--panic-unwind` 后自动生效）。

use crate::{oauth, panic_guard, routes, storage};
use axum::response::IntoResponse;
use std::sync::Arc;
use tower_service::Service;
use worker::*;

/// 绑定名（与 wrangler.toml 对齐；wrangler 校验缺名会在部署时报错）。
const BINDING_D1: &str = "LUGDB";
const BINDING_KV_SESSIONS: &str = "SESSIONS";

/// 护栏变量（lug-server config.rs 的变量名原样保留）：
/// 缺失/非法 → 回落默认值（同 config.rs `env_or + parse().unwrap_or`）。
fn env_usize(env: &Env, name: &str, default: usize) -> usize {
    env.var(name)
        .ok()
        .map(|v| v.to_string())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(default)
}

impl routes::AppState {
    /// 每请求装配（ lug-server main.rs 的 Workers 等价物）：
    /// D1Database 非 Clone → Key/审计/OAuth-state 三处各取一次句柄；
    /// LUG_PUBLIC_URL 非法 → Err（fetch 回 500，同 lug-server 拒绝启动）。
    pub(crate) fn from_env(env: &Env) -> worker::Result<Self> {
        let config = routes::WorkerConfig {
            max_value_digits: env_usize(env, "LUG_MAX_VALUE_DIGITS", 10_000_000),
            max_expr_len: env_usize(env, "LUG_MAX_EXPR_LEN", 1024),
        };
        let db_keys = env.d1(BINDING_D1)?;
        let db_audit = env.d1(BINDING_D1)?;
        let db_states = env.d1(BINDING_D1)?;
        let kv = env.kv(BINDING_KV_SESSIONS)?;
        // GITHUB_CLIENT_ID / LUG_PUBLIC_URL 走 vars，GITHUB_CLIENT_SECRET 走
        // secret（oauth::WorkerAuthConfig::from_worker_env，tech-notes §8）。
        let auth = oauth::WorkerAuthConfig::from_worker_env(env).map_err(worker::Error::from)?;
        let states = routes::D1StateStore::open(db_states);
        let flow = oauth::WorkerGithubFlow::new(auth.clone(), states);
        Ok(Self {
            config,
            keys: Arc::new(storage::keystore::KeyStore::open(db_keys)),
            audit: Arc::new(storage::audit::AuditLog::open(db_audit)),
            sessions: Arc::new(storage::sessions::SessionStore::open(kv)),
            auth,
            flow,
        })
    }
}

/// fetch 入口（官方 axum 模板写法，tech-notes §1）：
/// `http::Request<worker::Body>` 进 axum `Router::call`，
/// `http::Response` 直接回给 runtime。
#[event(fetch)]
async fn fetch(
    req: HttpRequest,
    env: Env,
    _ctx: Context,
) -> Result<axum::http::Response<axum::body::Body>> {
    // Router::call 的 Output 是 Result<Response, Infallible>（tower Service），
    // catch_unwind 后需剥两层：外层 panic、内层 Infallible。
    let mut svc = match routes::AppState::from_env(&env) {
        Ok(state) => routes::router(state),
        Err(err) => {
            // 绑定缺失/配置非法：不放大为 panic（isolate 会死），回统一 500。
            console_error!("binding/config assembly failed: {err}");
            return Ok(routes::api_error(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Service bindings or configuration are unavailable.",
                "internal_error",
                None,
            )
            .into_response());
        }
    };
    let resp = match panic_guard::catch_unwind_async(svc.call(req)).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(never)) => match never {},
        Err(_) => routes::api_error(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "internal panic during request handling",
            "internal_error",
            None,
        ),
    };
    Ok(resp)
}
