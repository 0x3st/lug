//! 会话存储，Workers KV 后端（绑定 `KV_SESSIONS`）。对齐
//! `lug-server/src/platform.rs` 的 `SessionStore`（create/get/revoke），语义
//! 对照见 persistence.md 附录 B #7。
//!
//! # 选型与形态（persistence.md §3.1）
//!
//! 写一次（登录）读多次（每请求）、TTL 自动过期免清理 —— 纯缓存型可过期
//! 状态，放 KV；原 `MAX_SESSIONS=4096` 按 seq 淘汰**废弃**（KV 无法廉价全局
//! 计数；签发必须走完整 GitHub OAuth 往返，刷写面已被 GitHub 侧限速抑制）。
//!
//! - Key：`sess:<token>`（token = 43 字符 base64url，总长 ≪ KV 512B 上限）；
//! - Value：JSON `{"user":{"provider","id","login"},"csrf_token","created_at"}`；
//!   原 `SessionRecord.expires` 字段由 KV 绝对过期承载，不再入值；
//! - TTL：`expiration = now + 604800`（7d，与原固定 expires 一致，不滑动续期；
//!   ≥ KV 最小 TTL 60s）；
//! - `get`：空值即未登录（原「过期顺手 remove」由 KV 自动过期替代）；
//! - `revoke`：`delete`（登出）。撤销非即时（≤60s 传播，R1，可接受）。
//!
//! # 登录闪断的缓解（persistence.md §3.2 缓解点 1）
//!
//! KV 写/删全球传播 ≤60s：callback 写 session → 302 → 浏览器下一请求读 KV
//! 可能 miss。[`SessionStore::put_confirmed`] 做**有界回读确认**（put → 回读，
//! 最多 3 次 × ~250ms，非空即放行跳转）；仍未命中返回 `false`，由调用方决定
//! 是否放行（同 PoP 通常首次即命中，回读只覆盖最坏情况）。
//!
//! # 整体切 D1 的备选路径（§3.2，类型边界保留）
//!
//! 若线上观测到回读缓解不了的闪断、或出现管理员强制下线需求：SessionStore
//! 改 D1 `sessions` 表（token PK / csrf_token / provider / uid / login / expires
//! 列，读时懒删过期行 + cron 清理）。本类型边界（put/get/revoke）即替换面，
//! 调用方（routes）无需感知后端切换。

use std::time::Duration;

use worker::{Delay, KvStore};

use super::row;

/// 会话有效期（秒），对齐 lug-server `platform.rs::SESSION_TTL`；
/// ≥ KV 最小 TTL 60s。
pub const SESSION_TTL: u64 = 7 * 24 * 3600;

/// KV key 前缀：独立命名空间内再按前缀分域，不与未来缓存混用。
pub const KEY_PREFIX: &str = "sess:";

/// 会话值 JSON 形态（persistence.md §3.1）。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRecord {
    pub user: SessionUser,
    pub csrf_token: String,
    /// Unix 秒（签发时刻；过期由 KV TTL 承载，仅作审计信息）。
    pub created_at: i64,
}

/// 已验证的 GitHub 身份（↔ lug-server `User`；access token 不持久化、不下发）。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionUser {
    pub provider: String,
    pub id: String,
    pub login: String,
}

/// `put` 的绝对过期（Unix 秒）：`now + SESSION_TTL`，负钟钳 0、u64 饱和。
pub(crate) fn absolute_expiration(now: i64) -> u64 {
    (now.max(0) as u64).saturating_add(SESSION_TTL)
}

/// KV key：`sess:<token>`。
pub(crate) fn session_key(token: &str) -> String {
    format!("{KEY_PREFIX}{token}")
}

impl SessionRecord {
    pub(crate) fn to_json_value(&self) -> serde_json::Value {
        serde_json::json!({
            "user": {
                "provider": self.user.provider,
                "id": self.user.id,
                "login": self.user.login,
            },
            "csrf_token": self.csrf_token,
            "created_at": self.created_at,
        })
    }

    /// KV 值（JSON 文本反序列化产物）→ 会话记录。
    pub(crate) fn from_value(v: &serde_json::Value) -> worker::Result<Self> {
        let r = row::as_object(v)?;
        let user = row::as_object(r.get("user").ok_or_else(|| row::err_plain("session: missing `user`"))?)?;
        Ok(Self {
            user: SessionUser {
                provider: row::text(user, "provider")?,
                id: row::text(user, "id")?,
                login: row::text(user, "login")?,
            },
            csrf_token: row::text(r, "csrf_token")?,
            created_at: row::i64(r, "created_at")?,
        })
    }
}

pub struct SessionStore {
    kv: KvStore,
}

impl SessionStore {
    pub fn open(kv: KvStore) -> Self {
        Self { kv }
    }

    /// 签发/写入会话（token 由调用方经 Workers crypto 生成；owner 校验——
    /// 拒绝匿名/本地身份——留在应用层，同原 `SessionStore::create` 的入口约束）。
    pub async fn put(
        &self,
        token: &str,
        user: &SessionUser,
        csrf_token: &str,
        now: i64,
    ) -> worker::Result<()> {
        let rec = SessionRecord {
            user: user.clone(),
            csrf_token: csrf_token.to_string(),
            created_at: now,
        };
        let value = serde_json::to_string(&rec.to_json_value())?;
        self.kv
            .put(&session_key(token), value)?
            .expiration(absolute_expiration(now))
            .execute()
            .await?;
        Ok(())
    }

    /// 写入 + 有界回读确认（登录闪断缓解，见模块注释）。
    /// 返回 `false` = 3 次回读均 miss（极罕见的跨 PoP 最坏情况），
    /// 由调用方决定放行策略。
    pub async fn put_confirmed(
        &self,
        token: &str,
        user: &SessionUser,
        csrf_token: &str,
        now: i64,
    ) -> worker::Result<bool> {
        self.put(token, user, csrf_token, now).await?;
        for attempt in 0..3u32 {
            if attempt > 0 {
                // Workers 环境专用定时器（setTimeout），非 tokio——本 crate 无 tokio。
                Delay::from(Duration::from_millis(250)).await;
            }
            if self.get(token).await?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 读取会话；空值即未登录（过期由 KV 自动过期替代原「顺手 remove」）。
    pub async fn get(&self, token: &str) -> worker::Result<Option<SessionRecord>> {
        match self.kv.get(&session_key(token)).json::<serde_json::Value>().await? {
            Some(v) => SessionRecord::from_value(&v).map(Some),
            None => Ok(None),
        }
    }

    /// 撤销（登出）。传播延迟 ≤60s（R1，当前可接受；强需求则切 D1，见模块注释）。
    pub async fn revoke(&self, token: &str) -> worker::Result<()> {
        self.kv.delete(&session_key(token)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TOKEN: &str = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFG"; // 43 字符

    #[test]
    fn ttl_matches_legacy_sessionstore_and_kv_minimum() {
        assert_eq!(SESSION_TTL, 7 * 24 * 3600);
        assert!(SESSION_TTL >= 60, "KV expirationTtl 最小 60s");
    }

    #[test]
    fn key_namespacing_and_length_bound() {
        assert_eq!(session_key(TOKEN), format!("sess:{TOKEN}"));
        assert_eq!(session_key(TOKEN).len(), 48); // ≪ KV 512B key 上限
        assert!(session_key("").starts_with(KEY_PREFIX)); // 空 token 也落前缀（防御性）
    }

    #[test]
    fn expiration_is_absolute_7d() {
        assert_eq!(absolute_expiration(1_700_000_000), 1_700_000_000 + 604_800);
        assert_eq!(absolute_expiration(0), 604_800); // 负钟/零钟钳到 TTL
        assert_eq!(absolute_expiration(-5), 604_800);
        // i64::MAX 不饱和（u64 域装得下），仅保证不回绕
        assert_eq!(absolute_expiration(i64::MAX), (i64::MAX as u64) + 604_800);
    }

    #[test]
    fn record_json_shape_matches_design() {
        let rec = SessionRecord {
            user: SessionUser {
                provider: "github".into(),
                id: "12345".into(),
                login: "octo".into(),
            },
            csrf_token: "csrf-1".into(),
            created_at: 1_700_000_000,
        };
        // 字段序与设计 §3.1 一致（preserve_order）
        assert_eq!(
            rec.to_json_value(),
            json!({
                "user": {"provider": "github", "id": "12345", "login": "octo"},
                "csrf_token": "csrf-1",
                "created_at": 1_700_000_000,
            })
        );
        // 往返
        assert_eq!(SessionRecord::from_value(&rec.to_json_value()).unwrap(), rec);
    }

    #[test]
    fn record_parse_rejects_malformed_values() {
        assert!(SessionRecord::from_value(&json!(null)).is_err());
        assert!(SessionRecord::from_value(&json!({"csrf_token": "c", "created_at": 1})).is_err());
        assert!(SessionRecord::from_value(&json!({
            "user": {"provider": "github", "id": 12345}, // login 缺失
            "csrf_token": "c", "created_at": 1,
        }))
        .is_err());
    }
}
