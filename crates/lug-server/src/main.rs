//! Lug 服务端装配（v1-alpha）。
//!
//! 路由面：`/` `/docs` `/platform` `/assets/*`（前端）、
//! `POST /f/a/o/calculate`、`GET /f/a/o/operator`（计算 API，Bearer Key 鉴权）、
//! `/f/a/o/platform/*`（控制台：GitHub OAuth 登录 / 会话 / Key 管理 / 用量）。
//!
//! GitHub 登录需配置 `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` / `LUG_PUBLIC_URL`；
//! 未配置时控制台仍可打开，仅登录入口禁用，Key 由 `lugctl` CLI 管理。

use lug_server::{
    audit::AuditLog,
    config::Config,
    keystore::KeyStore,
    platform::{AuthConfig, GitHubOAuth},
    routes,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let cfg = Config::from_env();
    let auth = AuthConfig::from_env().unwrap_or_else(|e| {
        eprintln!("[lug] {e}");
        std::process::exit(1);
    });
    let flow = Arc::new(GitHubOAuth::new(auth.clone()).unwrap_or_else(|e| {
        eprintln!("[lug] {e}");
        std::process::exit(1);
    }));
    let keys =
        KeyStore::open_with_legacy_migration(cfg.api_keys_path.clone(), &auth.legacy_owner_id)
            .unwrap_or_else(|e| {
                eprintln!("[lug] {e}");
                std::process::exit(1);
            });

    let addr = format!("{}:{}", cfg.host, cfg.port);
    let state = Arc::new(routes::AppState {
        config: cfg.clone(),
        keys,
        audit: AuditLog::open(cfg.audit_log_path.clone()),
        auth,
        flow,
        sessions: lug_server::platform::SessionStore::new(lug_server::platform::SESSION_TTL),
    });

    let app =
        routes::router(state.clone()).layer(axum::middleware::from_fn(routes::cors_middleware));

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("[lug] 无法绑定 {addr}: {e}");
            std::process::exit(1);
        });

    println!("结构化 AI 计算引擎 (Rust v1) 已启动: http://{addr}");
    println!("  官网: http://{addr}/  |  文档: /docs  |  控制台: /platform");
    println!(
        "  GitHub 登录: {}",
        if state.flow.enabled() {
            "已启用（多用户）"
        } else {
            "未配置（控制台登录不可用，Key 用 lugctl）"
        }
    );
    println!("  计费: prompt=表达式 token 数；reasoning=步骤数（1 步 1 token）；答案=1");
    println!(
        "  预算: 单值 ≤ {} 位；表达式 ≤ {} 字符",
        cfg.max_value_digits, cfg.max_expr_len
    );

    // 优雅停机：Ctrl+C 或 systemd SIGTERM
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        eprintln!("[lug] 服务错误: {e}");
        std::process::exit(1);
    }
    println!("服务已停止");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("安装 SIGTERM 处理器")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
