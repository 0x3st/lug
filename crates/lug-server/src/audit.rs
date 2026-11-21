//! 请求审计日志（§3.7：审计不能省——计费当前全免费，但每笔计算必须留痕）。
//!
//! 形态：JSONL 追加文件（一行一请求），成功与错误都记录。这是 request 级
//! 明细（SQLite fast-follow 的前置形态），与 KeyStore 的聚合计数器互补：
//! 聚合计数回答"用了多少"，审计日志回答"每一笔是什么、何时、结果如何"。
//!
//! 字段：ts / key_id / key_prefix / request_id / expression / kind(ok|error)
//!      / result(格式化值或错误消息) / usage(tokens) / duration_ms。
//! 追加写 + Mutex 串行；文件路径 `AUDIT_LOG_PATH`（默认 `audit.log`）。

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AuditLog {
    path: PathBuf,
    lock: Mutex<()>,
}

pub struct AuditEntry<'a> {
    pub key_id: &'a str,
    pub key_prefix: &'a str,
    pub request_id: &'a str,
    pub expression: &'a str,
    /// "ok" | "error"
    pub kind: &'a str,
    /// 成功：格式化结果串；错误：错误消息。
    pub outcome: &'a str,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub duration_ms: u128,
}

impl AuditLog {
    pub fn open(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// 追加一条审计记录。写失败不阻断主流程（审计尽力而为；
    /// SQLite 阶段升级为强一致）。
    pub fn record(&self, e: &AuditEntry<'_>) {
        let line = serde_json::json!({
            "ts": unix_now(),
            "key_id": e.key_id,
            "key_prefix": e.key_prefix,
            "request_id": e.request_id,
            "expression": e.expression,
            "kind": e.kind,
            "result": e.outcome,
            "usage": {
                "prompt_tokens": e.prompt_tokens,
                "completion_tokens": e.completion_tokens,
            },
            "duration_ms": e.duration_ms,
        });
        let _guard = self.lock.lock().unwrap();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
