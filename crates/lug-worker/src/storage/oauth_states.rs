//! OAuth state 存储（PKCE），D1 `oauth_states` 表后端。对齐
//! `lug-server/src/platform.rs` 的 `StateStore`（create/consume），语义对照见
//! persistence.md 附录 B #8。
//!
//! # 为什么不用 KV（关键决定，persistence.md §3.3，评审 gate：state 不入 KV）
//!
//! KV 没有原子删除。用 `GET + DELETE` 实现一次性消费存在两层不可修复的缺陷：
//!
//! 1. **竞态**：两个并发 callback（或攻击者与受害者浏览器）同持一个 state——
//!    双方 GET 均命中 → 双方 DELETE 均成功 → 双方都拿到 `code_verifier` 进入
//!    token exchange。PKCE 的 code 单次有效只保证只有一家换到 token，但阻止
//!    不了攻击者成为那一家：持有完整重定向 URL（code+state）的赢家可以完成
//!    「以受害者身份登录」（登录 CSRF / 会话固定）。
//! 2. **更糟**：即使串行重放也防不住——KV 删除的跨 PoP 传播 ≤60s，消费成功后
//!    其他 PoP 的 GET 在窗口内仍返回旧值；写 tombstone 同样受传播延迟限制，
//!    无法把窗口压到零。
//!
//! 而本表的单条 `DELETE … WHERE state=? AND expires>? RETURNING verifier` 在
//! D1（SQLite WAL 行锁）内语句级原子：两个并发 isolate 同时 consume，**恰有
//! 一个**拿到 verifier，且该行已消失——多实例下一次性语义严格成立。state 存活
//! 仅 600s、每登录一行，D1 成本可忽略；原 `MAX_OAUTH_STATES=2048` 进程内淘汰
//! 随之废弃（600s TTL + `expires` 索引 + cron 兜底清理）。
//!
//! 边界划分：state 与浏览器 Cookie 的常数时间比较（`const_eq`）留在应用层
//! （routes，T7），不进 SQL——本层只保证存储侧的一次性消费。

use worker::D1Database;

use super::{prepare, row, SqlParam};

/// state 有效期（秒），对齐 lug-server `platform.rs::STATE_TTL`。
pub const STATE_TTL: i64 = 600;

pub(crate) const SQL_INSERT: &str = "INSERT INTO oauth_states (state, verifier, expires) \
        VALUES (?1, ?2, ?3)";

pub(crate) const SQL_CONSUME: &str = "DELETE FROM oauth_states \
        WHERE state = ?1 AND expires > ?2 \
        RETURNING verifier";

pub(crate) const SQL_PURGE: &str = "DELETE FROM oauth_states WHERE expires <= ?1";

/// `create` 的绑定参数（expires 由本层按 STATE_TTL 推导）。
pub(crate) fn create_params(state: &str, verifier: &str, now: i64) -> Vec<SqlParam> {
    vec![
        SqlParam::text(state),
        SqlParam::text(verifier),
        SqlParam::int(now.saturating_add(STATE_TTL)),
    ]
}

/// `consume` 的绑定参数。
pub(crate) fn consume_params(state: &str, now: i64) -> Vec<SqlParam> {
    vec![SqlParam::text(state), SqlParam::int(now)]
}

pub struct OAuthStateStore {
    db: D1Database,
}

impl OAuthStateStore {
    pub fn open(db: D1Database) -> Self {
        Self { db }
    }

    /// 登记一个 pending 登录（`state`/`verifier` 由调用方经 Workers crypto 生成，
    /// PKCE challenge 由调用方按 S256 从 verifier 现算，不落库）。
    /// `expires = now + STATE_TTL`。
    pub async fn create(&self, state: &str, verifier: &str, now: i64) -> worker::Result<()> {
        debug_assert!(!state.is_empty() && !verifier.is_empty(), "state/verifier 由调用方生成，不得为空");
        prepare(&self.db, SQL_INSERT, &create_params(state, verifier, now))?
            .run()
            .await?;
        Ok(())
    }

    /// 原子一次性消费：返回 `Some(verifier)` 当且仅当 state 存在、未过期、
    /// 且这是第一次消费（行已随之删除，不可能二次消费）。
    /// `None` ⇒ 不存在 / 已过期 / 已被消费——调用方统一映射
    /// `invalid_oauth_state`（与原 `OAuthError::InvalidOauthState` 单一错误码一致）。
    pub async fn consume(&self, state: &str, now: i64) -> worker::Result<Option<String>> {
        let res = prepare(&self.db, SQL_CONSUME, &consume_params(state, now))?
            .all()
            .await?;
        let rows: Vec<serde_json::Value> = res.results()?;
        match rows.first() {
            Some(v) => Ok(Some(row::text(row::as_object(v)?, "verifier")?)),
            None => Ok(None),
        }
    }

    /// 过期行兜底清理（Cron Trigger 每 5 分钟；已被消费的行早已不在）。
    /// 返回清除行数。
    pub async fn purge_expired(&self, now: i64) -> worker::Result<u64> {
        let res = prepare(&self.db, SQL_PURGE, &[SqlParam::int(now)])?
            .run()
            .await?;
        Ok(res.meta()?.and_then(|m| m.changes).unwrap_or(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_matches_legacy_statestore() {
        assert_eq!(STATE_TTL, 600);
    }

    #[test]
    fn insert_sql_sets_expires_from_ttl() {
        assert_eq!(
            SQL_INSERT,
            "INSERT INTO oauth_states (state, verifier, expires) VALUES (?1, ?2, ?3)"
        );
    }

    #[test]
    fn consume_is_atomic_delete_returning_verifier() {
        // 一次性消费的核心语句：DELETE（非 GET+DELETE）+ RETURNING + 过期条件
        assert!(SQL_CONSUME.starts_with("DELETE FROM oauth_states"));
        assert!(SQL_CONSUME.contains("WHERE state = ?1 AND expires > ?2"));
        assert!(SQL_CONSUME.contains("RETURNING verifier"));
        assert!(!SQL_CONSUME.to_lowercase().contains("select"));
    }

    #[test]
    fn create_params_compute_expires() {
        // expires = now + 600（由本层按 STATE_TTL 注入，调用方只给 now）
        assert_eq!(
            create_params("s".repeat(43).as_str(), "v".repeat(64).as_str(), 1_700_000_000),
            vec![
                SqlParam::text("s".repeat(43)),
                SqlParam::text("v".repeat(64)),
                SqlParam::int(1_700_000_600),
            ]
        );
    }

    #[test]
    fn consume_params_order() {
        assert_eq!(
            consume_params("s".repeat(43).as_str(), 1_700_000_000),
            vec![SqlParam::text("s".repeat(43)), SqlParam::int(1_700_000_000)]
        );
    }

    #[test]
    fn purge_covers_expired_rows_for_cron() {
        assert!(SQL_PURGE.contains("WHERE expires <= ?1"));
    }
}
