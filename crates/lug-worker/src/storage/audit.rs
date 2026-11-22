//! 请求审计日志，D1 `audit_log` 表后端。对齐 `lug-server/src/audit.rs`
//! 的 `AuditLog::record` 类型边界与「尽力而为」语义（persistence.md
//! 附录 B #6）：审计失败**绝不阻断主流程**——与原 JSONL 追加的
//! `let _ = writeln!(...)` 同构，失败仅 `console_error`。
//!
//! 字段一一对应（usage 嵌套拍平为 prompt/completion 两列）：ts / key_id /
//! key_prefix / request_id / expression / kind(ok|error) / result(格式化值
//! 或错误消息) / usage{prompt,completion} / duration_ms。
//!
//! 执行方式（persistence.md §2.2）：调用方建议经 `ctx.wait_until(audit.record(e))`
//! 挂后台执行（注意 `'static`：需把 entry 的借用字段 clone 成 owned 再挂），
//! 或直接 await（await 增加每请求 1 行写的延迟，设计文档 §5-行3/§6-R10）。
//! `kind` 的 ok|error 约束由表 CHECK 兜底；写入侧不做二次校验（原实现同样
//! 不校验 kind 取值）。

use worker::{console_error, D1Database, Date};

use super::{prepare, sat_i64, SqlParam};

pub(crate) const SQL_INSERT: &str = "INSERT INTO audit_log \
        (ts, key_id, key_prefix, request_id, expression, kind, result, \
         prompt_tokens, completion_tokens, duration_ms) \
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";

/// 一条审计记录（字段与 lug-server `AuditEntry` 一致；`duration_ms` 保持 u128
/// 以对齐 `Instant::elapsed().as_millis()` 调用面）。
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

/// 当前 Unix 秒（Workers `Date::now()`；与原 `int(time.time())` 同粒度）。
pub(crate) fn unix_now() -> i64 {
    (Date::now().as_millis() / 1000) as i64
}

/// `u128` 毫秒 → i64（先钳 u64 再钳 i64，两级饱和）。
pub(crate) fn sat_ms(v: u128) -> i64 {
    sat_i64(u64::try_from(v).unwrap_or(u64::MAX))
}

/// `record` 的绑定参数（顺序 = `SQL_INSERT` 占位符序）。纯函数，供单测断言。
pub(crate) fn insert_params(e: &AuditEntry<'_>, ts: i64) -> Vec<SqlParam> {
    vec![
        SqlParam::int(ts),
        SqlParam::text(e.key_id),
        SqlParam::text(e.key_prefix),
        SqlParam::text(e.request_id),
        SqlParam::text(e.expression),
        SqlParam::text(e.kind),
        SqlParam::text(e.outcome),
        SqlParam::int(sat_i64(e.prompt_tokens)),
        SqlParam::int(sat_i64(e.completion_tokens)),
        SqlParam::int(sat_ms(e.duration_ms)),
    ]
}

pub struct AuditLog {
    db: D1Database,
}

impl AuditLog {
    pub fn open(db: D1Database) -> Self {
        Self { db }
    }

    /// 追加一条审计记录。尽力而为：失败仅 `console_error`，不阻断主流程、
    /// 不向上传播（同原 JSONL 实现的静默容错）。
    pub async fn record(&self, e: &AuditEntry<'_>) {
        if let Err(err) = self.try_record(e, unix_now()).await {
            console_error!("audit insert failed (best-effort, not blocking): {err}");
        }
    }

    /// 可观测失败路径：需要自行处理错误（如强一致升级）的调用方使用。
    async fn try_record(&self, e: &AuditEntry<'_>, ts: i64) -> worker::Result<()> {
        prepare(&self.db, SQL_INSERT, &insert_params(e, ts))?
            .run()
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>() -> AuditEntry<'a> {
        AuditEntry {
            key_id: "a1b2c3",
            key_prefix: "sk-x...y",
            request_id: "req-42",
            expression: "1+2*3",
            kind: "ok",
            outcome: "7",
            prompt_tokens: 11,
            completion_tokens: 22,
            duration_ms: 5,
        }
    }

    #[test]
    fn insert_sql_covers_all_flat_columns() {
        assert!(SQL_INSERT.starts_with("INSERT INTO audit_log"));
        assert!(SQL_INSERT.contains(
            "(ts, key_id, key_prefix, request_id, expression, kind, result, \
             prompt_tokens, completion_tokens, duration_ms)"
        ));
        assert!(SQL_INSERT.contains("VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"));
        // kind 的取值约束在 schema CHECK，不在写入 SQL
        assert!(!SQL_INSERT.contains("CHECK"));
    }

    #[test]
    fn params_order_matches_column_list() {
        assert_eq!(
            insert_params(&entry(), 1_700_000_000),
            vec![
                SqlParam::int(1_700_000_000),
                SqlParam::text("a1b2c3"),
                SqlParam::text("sk-x...y"),
                SqlParam::text("req-42"),
                SqlParam::text("1+2*3"),
                SqlParam::text("ok"),
                SqlParam::text("7"),
                SqlParam::int(11),
                SqlParam::int(22),
                SqlParam::int(5),
            ]
        );
    }

    #[test]
    fn error_kind_entry_maps_to_result_column() {
        let e = AuditEntry {
            kind: "error",
            outcome: "division by zero",
            ..entry()
        };
        let params = insert_params(&e, 1);
        assert_eq!(params[5], SqlParam::text("error"));
        assert_eq!(params[6], SqlParam::text("division by zero"));
    }

    #[test]
    fn counters_and_duration_saturate_without_wrapping() {
        let e = AuditEntry {
            prompt_tokens: u64::MAX,
            completion_tokens: u64::MAX,
            duration_ms: u128::MAX,
            ..entry()
        };
        let params = insert_params(&e, 1);
        assert_eq!(params[7], SqlParam::int(i64::MAX));
        assert_eq!(params[8], SqlParam::int(i64::MAX));
        assert_eq!(params[9], SqlParam::int(i64::MAX));
    }
}
