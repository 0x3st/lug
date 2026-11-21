//! 服务端配置：全部来自环境变量（`deploy/lug.env.example` 有完整说明）。
//!
//! | 变量 | 默认 | 说明 |
//! |---|---|---|
//! | `HOST` / `PORT` | 127.0.0.1 / 8000 | 监听地址（公网部署只绑 127.0.0.1，反代对外） |
//! | `WEB_DIR` | `web` | 前端构建产物目录 |
//! | `API_KEYS_PATH` | `api_keys.json` | Key 存储（与 Python 原型同格式，可互换） |
//! | `LUG_MAX_VALUE_DIGITS` | 10000000 | 单值位数预算（§3.1 运算预算护栏） |
//! | `LUG_MAX_EXPR_LEN` | 1024 | 表达式长度上限（§3.7 工作有界） |

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub web_dir: PathBuf,
    pub api_keys_path: PathBuf,
    pub audit_log_path: PathBuf,
    pub max_value_digits: usize,
    pub max_expr_len: usize,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> Self {
        let max_value_digits = env_or("LUG_MAX_VALUE_DIGITS", "10000000")
            .parse()
            .unwrap_or(10_000_000);
        Self {
            host: env_or("HOST", "127.0.0.1"),
            port: env_or("PORT", "8000").parse().unwrap_or(8000),
            web_dir: PathBuf::from(env_or("WEB_DIR", "web")),
            api_keys_path: PathBuf::from(env_or("API_KEYS_PATH", "api_keys.json")),
            audit_log_path: PathBuf::from(env_or("AUDIT_LOG_PATH", "audit.log")),
            max_value_digits,
            max_expr_len: env_or("LUG_MAX_EXPR_LEN", "1024").parse().unwrap_or(1024),
        }
    }

    /// 引擎求值上下文（预算透传）。
    pub fn ctx(&self) -> lug_engine::Ctx {
        lug_engine::Ctx {
            max_digits: self.max_value_digits,
        }
    }
}
