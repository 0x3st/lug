//! API Key 存储，D1 后端。类型边界对齐 `lug-server/src/keystore.rs` 的 `KeyStore`
//! （open/create/validate/record_usage/list_public/revoke/revoke_scoped），
//! 语义对照见 lug-migration/design/persistence.md 附录 B #1–#5。
//!
//! 与原实现的关键语义差异（均为设计文档明确要求）：
//! - `create`：随机性由调用方注入（`id` = 6 随机字节 hex、`raw` = "sk-"+base64url(36B)，
//!   T6 经 Workers crypto 生成）；同 id/key_hash 冲突从原 JSON 的静默覆盖收紧为约束报错。
//! - `record_usage`：单条 `UPDATE x = x + ?` 语句级原子（WAL 行锁），多 isolate/wrangler
//!   并发无丢失更新（原为进程内 RMW + 全量重写，跨进程本就丢更新）；未命中即 no-op，
//!   与原 `if let Some(rec)` 静默语义一致。调用契约：await 后**失败仅记日志不阻断**
//!   （保持「先落账后播流」顺序，容错同原 `let _ = atomic_write`）。
//! - `revoke`：即时全局生效（原仅本进程快照）；命中返回 true（含重复撤销，原语义；
//!   SQLite 对 WHERE 命中的行计数，与值是否实际变化无关）。
//! - `revoke_scoped`：IDOR 防护与「仅 active 可撤销」条件化进 WHERE，
//!   `RETURNING` 0 行 ⇒ 未命中/他人 Key/已撤销 → `None`（调用方映射 404）。
//! - `list_public`：权限靠 SQL 投影（不选 key_hash/owner_id），不靠应用层剔除；
//!   `ORDER BY id` 保持原 `BTreeMap` 迭代序。

use worker::D1Database;

use super::{prepare, row, sat_i64, SqlParam};

/// D1 `api_keys` 全列行 ↔ lug-server `KeyRec`（usage 拍平为 4 列）。
///
/// 本 crate 无 serde derive（Cargo.toml 禁改），行解析经 [`KeyRec::from_row`]
/// 手工完成；`to_public_json` 用 `serde_json::json!`（字段序=插入序，workspace
/// serde_json 开 preserve_order）。
#[derive(Clone, Debug, PartialEq)]
pub struct KeyRec {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub key_hash: String,
    pub prefix: String,
    pub created_at: i64,
    pub status: String,
    pub last_used: Option<i64>,
    pub usage_requests: u64,
    pub usage_prompt_tokens: u64,
    pub usage_completion_tokens: u64,
    pub usage_total_tokens: u64,
}

/// `list_public` 的投影行：无 key_hash / owner_id（投影即权限）。
#[derive(Clone, Debug, PartialEq)]
pub struct KeyPublic {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub created_at: i64,
    pub status: String,
    pub last_used: Option<i64>,
    pub usage_requests: u64,
    pub usage_prompt_tokens: u64,
    pub usage_completion_tokens: u64,
    pub usage_total_tokens: u64,
}

// ------------------------------------------------------------------ SQL

pub(crate) const SQL_INSERT: &str = "INSERT INTO api_keys \
        (id, owner_id, name, key_hash, prefix, created_at, status) \
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active')";

pub(crate) const SQL_VALIDATE: &str = "SELECT id, owner_id, name, key_hash, prefix, \
        created_at, status, last_used, usage_requests, usage_prompt_tokens, \
        usage_completion_tokens, usage_total_tokens \
        FROM api_keys WHERE key_hash = ?1 AND status = 'active' LIMIT 1";

pub(crate) const SQL_RECORD_USAGE: &str = "UPDATE api_keys \
        SET usage_requests = usage_requests + ?1, \
            usage_prompt_tokens = usage_prompt_tokens + ?2, \
            usage_completion_tokens = usage_completion_tokens + ?3, \
            usage_total_tokens = usage_total_tokens + ?2 + ?3, \
            last_used = ?4 \
        WHERE id = ?5";

pub(crate) const SQL_LIST_PUBLIC: &str = "SELECT id, name, prefix, created_at, status, \
        last_used, usage_requests, usage_prompt_tokens, usage_completion_tokens, \
        usage_total_tokens \
        FROM api_keys WHERE owner_id = ?1 ORDER BY id";

pub(crate) const SQL_REVOKE: &str = "UPDATE api_keys SET status = 'revoked' WHERE id = ?1";

pub(crate) const SQL_REVOKE_SCOPED: &str = "UPDATE api_keys SET status = 'revoked' \
        WHERE id = ?1 AND owner_id = ?2 AND status = 'active' \
        RETURNING id, owner_id, name, key_hash, prefix, created_at, status, last_used, \
                  usage_requests, usage_prompt_tokens, usage_completion_tokens, usage_total_tokens";

// ------------------------------------------------------------------ 纯派生（单测面）

/// raw → SHA-256 hex（64 字符）；明文永不落库（同原 `sha256_hex`）。
pub fn key_hash(raw: &str) -> String {
    use sha2::{Digest, Sha256};
    let d = Sha256::digest(raw.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// 展示前缀：前 11 字符 + "..." + 末 4 字符（同原实现）。
/// 前置条件 `raw.len() >= 15`——由签发方保证（"sk-" + 43 字符 base64url），
/// 退化输入与原实现一样直接 panic（字节切片越界）。
pub fn key_prefix(raw: &str) -> String {
    format!("{}...{}", &raw[..11], &raw[raw.len() - 4..])
}

/// name 截断：缺省 "default"，超长按 **字符**（非字节）取前 40（同原实现）。
pub fn clamp_name(name: Option<&str>) -> String {
    name.unwrap_or("default").chars().take(40).collect()
}

/// 由 raw/id 派生完整记录（`create` 的纯部分）。
pub(crate) fn new_key_rec(
    id: &str,
    raw: &str,
    name: Option<&str>,
    owner_id: &str,
    now: i64,
) -> KeyRec {
    KeyRec {
        id: id.to_string(),
        owner_id: owner_id.to_string(),
        name: clamp_name(name),
        key_hash: key_hash(raw),
        prefix: key_prefix(raw),
        created_at: now,
        status: "active".into(),
        last_used: None,
        usage_requests: 0,
        usage_prompt_tokens: 0,
        usage_completion_tokens: 0,
        usage_total_tokens: 0,
    }
}

/// `create` 的绑定参数（顺序 = `SQL_INSERT` 占位符序）。
pub(crate) fn insert_params(rec: &KeyRec) -> Vec<SqlParam> {
    vec![
        SqlParam::text(&rec.id),
        SqlParam::text(&rec.owner_id),
        SqlParam::text(&rec.name),
        SqlParam::text(&rec.key_hash),
        SqlParam::text(&rec.prefix),
        SqlParam::int(rec.created_at),
    ]
}

/// `record_usage` 的绑定参数（顺序 = `SQL_RECORD_USAGE` 占位符序；
/// ?1 恒为 1 = 每次调用 requests +1）。
pub(crate) fn record_usage_params(
    kid: &str,
    prompt: u64,
    completion: u64,
    now: i64,
) -> Vec<SqlParam> {
    vec![
        SqlParam::int(1),
        SqlParam::int(sat_i64(prompt)),
        SqlParam::int(sat_i64(completion)),
        SqlParam::int(now),
        SqlParam::text(kid),
    ]
}

// ------------------------------------------------------------------ 行解析

impl KeyRec {
    /// D1 行对象（`results::<serde_json::Value>` 产物）→ 记录。
    /// 列集与 `SQL_VALIDATE` / `SQL_REVOKE_SCOPED` 的 RETURNING 严格一致。
    pub(crate) fn from_row(v: &serde_json::Value) -> worker::Result<Self> {
        let r = row::as_object(v)?;
        Ok(Self {
            id: row::text(r, "id")?,
            owner_id: row::text(r, "owner_id")?,
            name: row::text(r, "name")?,
            key_hash: row::text(r, "key_hash")?,
            prefix: row::text(r, "prefix")?,
            created_at: row::i64(r, "created_at")?,
            status: row::text(r, "status")?,
            last_used: row::opt_i64(r, "last_used")?,
            usage_requests: row::u64(r, "usage_requests")?,
            usage_prompt_tokens: row::u64(r, "usage_prompt_tokens")?,
            usage_completion_tokens: row::u64(r, "usage_completion_tokens")?,
            usage_total_tokens: row::u64(r, "usage_total_tokens")?,
        })
    }
}

impl KeyPublic {
    /// D1 行对象 → 公开投影记录（列集与 `SQL_LIST_PUBLIC` 投影一致）。
    pub(crate) fn from_row(v: &serde_json::Value) -> worker::Result<Self> {
        let r = row::as_object(v)?;
        Ok(Self {
            id: row::text(r, "id")?,
            name: row::text(r, "name")?,
            prefix: row::text(r, "prefix")?,
            created_at: row::i64(r, "created_at")?,
            status: row::text(r, "status")?,
            last_used: row::opt_i64(r, "last_used")?,
            usage_requests: row::u64(r, "usage_requests")?,
            usage_prompt_tokens: row::u64(r, "usage_prompt_tokens")?,
            usage_completion_tokens: row::u64(r, "usage_completion_tokens")?,
            usage_total_tokens: row::u64(r, "usage_total_tokens")?,
        })
    }
}

// ------------------------------------------------------------------ 存储

/// lug-server `KeyStore` 的 D1 backend。`open` 不可失败：D1 无文件可损坏，
/// schema 由 migrations 建立，坏库会在其后第一条语句上报错。
pub struct KeyStore {
    db: D1Database,
}

impl KeyStore {
    pub fn open(db: D1Database) -> Self {
        Self { db }
    }

    /// 签发新 Key：明文 `raw` 仅本次经返回的 [`KeyRec`] 外流转（本层不返回明文，
    /// 调用方自留）；落库 SHA-256。冲突（重复 id / key_hash）→ `Err`（原 JSON 为
    /// 静默覆盖，语义收紧，persistence.md §5-行1）。
    pub async fn create(
        &self,
        id: &str,
        raw: &str,
        name: Option<&str>,
        owner_id: &str,
        now: i64,
    ) -> worker::Result<KeyRec> {
        let rec = new_key_rec(id, raw, name, owner_id, now);
        prepare(&self.db, SQL_INSERT, &insert_params(&rec))?
            .run()
            .await?;
        Ok(rec)
    }

    /// 校验明文 Key：命中且 active 返回记录（`sk-` 前缀拦截同原实现）。
    pub async fn validate(&self, raw: &str) -> worker::Result<Option<KeyRec>> {
        if !raw.starts_with("sk-") {
            return Ok(None);
        }
        match prepare(&self.db, SQL_VALIDATE, &[SqlParam::text(key_hash(raw))])?
            .first::<serde_json::Value>(None)
            .await?
        {
            Some(v) => KeyRec::from_row(&v).map(Some),
            None => Ok(None),
        }
    }

    /// 记一次成功调用的用量：`x = x + ?` 原子累加 + `last_used`。未命中 no-op。
    pub async fn record_usage(
        &self,
        kid: &str,
        prompt: u64,
        completion: u64,
        now: i64,
    ) -> worker::Result<()> {
        prepare(
            &self.db,
            SQL_RECORD_USAGE,
            &record_usage_params(kid, prompt, completion, now),
        )?
        .run()
        .await?;
        Ok(())
    }

    /// 列出某 owner 的 Key（投影排除 key_hash / owner_id，`ORDER BY id` = 原
    /// BTreeMap 序）。
    pub async fn list_public(&self, owner_id: &str) -> worker::Result<Vec<KeyPublic>> {
        let res = prepare(&self.db, SQL_LIST_PUBLIC, &[SqlParam::text(owner_id)])?
            .all()
            .await?;
        let rows: Vec<serde_json::Value> = res.results()?;
        rows.iter().map(KeyPublic::from_row).collect()
    }

    /// 全局撤销（lugctl/wrangler 路径）：命中即 true（含重复撤销，原语义）。
    pub async fn revoke(&self, kid: &str) -> worker::Result<bool> {
        let res = prepare(&self.db, SQL_REVOKE, &[SqlParam::text(kid)])?
            .run()
            .await?;
        Ok(res.meta()?.and_then(|m| m.changes).unwrap_or(0) > 0)
    }

    /// owner 作用域撤销（HTTP 路径）：仅本人且仅 active 可撤销；
    /// 返回撤销**后**的记录（status='revoked'，同原快照语义）。
    /// `RETURNING` 0 行 = 未命中 / 他人的 Key / 已撤销 → `None`（404/IDOR 语义）。
    pub async fn revoke_scoped(&self, kid: &str, owner_id: &str) -> worker::Result<Option<KeyRec>> {
        let res = prepare(
            &self.db,
            SQL_REVOKE_SCOPED,
            &[SqlParam::text(kid), SqlParam::text(owner_id)],
        )?
        .all()
        .await?;
        let rows: Vec<serde_json::Value> = res.results()?;
        rows.first().map(KeyRec::from_row).transpose()
    }
}

// ------------------------------------------------------------------ 公开视图

impl KeyRec {
    /// 剥离 key_hash / owner_id 的公开视图。
    pub fn public(&self) -> KeyPublic {
        KeyPublic {
            id: self.id.clone(),
            name: self.name.clone(),
            prefix: self.prefix.clone(),
            created_at: self.created_at,
            status: self.status.clone(),
            last_used: self.last_used,
            usage_requests: self.usage_requests,
            usage_prompt_tokens: self.usage_prompt_tokens,
            usage_completion_tokens: self.usage_completion_tokens,
            usage_total_tokens: self.usage_total_tokens,
        }
    }

    /// 公开 JSON（字段序同 lug-server `public_rec_json`；preserve_order 下插入序
    /// 即输出序）。
    pub fn to_public_json(&self) -> serde_json::Value {
        self.public().to_public_json()
    }
}

impl KeyPublic {
    /// 公开 JSON（字段序同 lug-server `public_rec_json`）。
    pub fn to_public_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "name": self.name,
            "prefix": self.prefix,
            "created_at": self.created_at,
            "status": self.status,
            "last_used": self.last_used,
            "usage": {
                "requests": self.usage_requests,
                "prompt_tokens": self.usage_prompt_tokens,
                "completion_tokens": self.usage_completion_tokens,
                "total_tokens": self.usage_total_tokens,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use worker::D1Type;

    const RAW: &str = "sk-Ab3dEf6hIj9kLm2nPq5rSt8uVw1xYz4AaBbCcDdEeFfGg";

    #[test]
    fn key_hash_matches_known_sha256_vector() {
        // 独立已知向量（SHA-256("abc")），不与实现互证。
        assert_eq!(
            key_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(key_hash(RAW).len(), 64);
        assert!(key_hash(RAW).bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn key_prefix_is_head11_ellipsis_tail4() {
        assert_eq!(key_prefix(RAW), format!("{}...{}", &RAW[..11], &RAW[RAW.len() - 4..]));
        assert_eq!(key_prefix(RAW).len(), 11 + 3 + 4);
        // 独立手算样本："sk-abcdefghijklmnop"（19 字符）→ 前 11 "sk-abcdefgh" + 末 4 "mnop"
        assert_eq!(key_prefix("sk-abcdefghijklmnop"), "sk-abcdefgh...mnop");
    }

    #[test]
    fn clamp_name_defaults_and_truncates_by_chars() {
        assert_eq!(clamp_name(None), "default");
        assert_eq!(clamp_name(Some("short")), "short");
        assert_eq!(clamp_name(Some(&"x".repeat(40))), "x".repeat(40));
        assert_eq!(clamp_name(Some(&"x".repeat(41))).len(), 40);
        // 按 40 个字符截断，多字节字符不被劈半
        let cjk = "键".repeat(45);
        assert_eq!(clamp_name(Some(&cjk)), "键".repeat(40));
    }

    #[test]
    fn new_key_rec_derivation() {
        let rec = new_key_rec("a1b2c3", RAW, Some("my key"), "github:12345", 1_700_000_000);
        assert_eq!(rec.id, "a1b2c3");
        assert_eq!(rec.owner_id, "github:12345");
        assert_eq!(rec.name, "my key");
        assert_eq!(rec.key_hash, key_hash(RAW));
        assert_eq!(rec.prefix, key_prefix(RAW));
        assert_eq!(rec.created_at, 1_700_000_000);
        assert_eq!(rec.status, "active");
        assert_eq!(rec.last_used, None);
        assert_eq!(
            (
                rec.usage_requests,
                rec.usage_prompt_tokens,
                rec.usage_completion_tokens,
                rec.usage_total_tokens
            ),
            (0, 0, 0, 0)
        );
    }

    #[test]
    fn insert_sql_and_params_match_schema_columns() {
        let rec = new_key_rec("a1b2c3", RAW, None, "local", 1_700_000_000);
        assert_eq!(
            insert_params(&rec),
            vec![
                SqlParam::text("a1b2c3"),
                SqlParam::text("local"),
                SqlParam::text("default"),
                SqlParam::text(key_hash(RAW)),
                SqlParam::text(key_prefix(RAW)),
                SqlParam::int(1_700_000_000),
            ]
        );
        assert!(SQL_INSERT.starts_with("INSERT INTO api_keys"));
        assert!(SQL_INSERT.contains("(id, owner_id, name, key_hash, prefix, created_at, status)"));
        assert!(SQL_INSERT.contains("VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active')"));
    }

    #[test]
    fn record_usage_is_atomic_increment_sql() {
        assert!(SQL_RECORD_USAGE.contains("usage_requests = usage_requests + ?1"));
        assert!(SQL_RECORD_USAGE.contains("usage_prompt_tokens = usage_prompt_tokens + ?2"));
        assert!(SQL_RECORD_USAGE.contains("usage_completion_tokens = usage_completion_tokens + ?3"));
        assert!(SQL_RECORD_USAGE.contains("usage_total_tokens = usage_total_tokens + ?2 + ?3"));
        assert!(SQL_RECORD_USAGE.contains("last_used = ?4"));
        assert!(SQL_RECORD_USAGE.contains("WHERE id = ?5"));
        assert_eq!(
            record_usage_params("a1b2c3", 11, 22, 1_700_000_001),
            vec![
                SqlParam::int(1),
                SqlParam::int(11),
                SqlParam::int(22),
                SqlParam::int(1_700_000_001),
                SqlParam::text("a1b2c3"),
            ]
        );
    }

    #[test]
    fn validate_sql_is_active_only_point_lookup() {
        assert!(SQL_VALIDATE.contains("WHERE key_hash = ?1 AND status = 'active' LIMIT 1"));
        // 投影含全列（KeyRec::from_row 解析需要）
        for col in [
            "id",
            "owner_id",
            "name",
            "key_hash",
            "prefix",
            "last_used",
            "usage_total_tokens",
        ] {
            assert!(SQL_VALIDATE.contains(col), "missing column {col}");
        }
    }

    #[test]
    fn revoke_scoped_conditional_update_with_returning() {
        assert!(SQL_REVOKE.starts_with("UPDATE api_keys SET status = 'revoked' WHERE id = ?1"));
        assert!(
            SQL_REVOKE_SCOPED.contains("WHERE id = ?1 AND owner_id = ?2 AND status = 'active'")
        );
        assert!(SQL_REVOKE_SCOPED.contains("RETURNING"));
    }

    #[test]
    fn list_public_projection_excludes_secret_columns() {
        assert!(SQL_LIST_PUBLIC.contains("WHERE owner_id = ?1 ORDER BY id"));
        // SELECT 投影（FROM 之前）不得出现敏感列；owner_id 只允许出现在 WHERE
        let projection = SQL_LIST_PUBLIC.split("FROM").next().unwrap();
        assert!(!projection.contains("key_hash"));
        assert!(!projection.contains("owner_id"));
    }

    #[test]
    fn key_rec_from_row_parses_d1_style_row() {
        let rec = KeyRec::from_row(&json!({
            "id": "a1b2c3",
            "owner_id": "github:12345",
            "name": "default",
            "key_hash": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            "prefix": "sk-x...y",
            "created_at": 1_700_000_000,
            "status": "active",
            "last_used": null,
            "usage_requests": 3,
            "usage_prompt_tokens": 100,
            "usage_completion_tokens": 200,
            "usage_total_tokens": 300,
        }))
        .unwrap();
        assert_eq!(rec.last_used, None);
        assert_eq!(rec.usage_total_tokens, 300);

        // 有 last_used 的行
        let mut row = json!({
            "id": "a1b2c3", "owner_id": "local", "name": "n", "key_hash": "h",
            "prefix": "p", "created_at": 1, "status": "revoked",
            "last_used": 1_700_000_100, "usage_requests": 0,
            "usage_prompt_tokens": 0, "usage_completion_tokens": 0, "usage_total_tokens": 0,
        });
        let rec2 = KeyRec::from_row(&row).unwrap();
        assert_eq!(rec2.last_used, Some(1_700_000_100));

        // 缺列 / 类型不符 → Err（防线）
        if let Some(obj) = row.as_object_mut() {
            obj.remove("key_hash");
        }
        assert!(KeyRec::from_row(&row).is_err());
        assert!(KeyRec::from_row(&json!(42)).is_err());
    }

    #[test]
    fn key_public_from_row_rejects_full_row_semantics() {
        // list_public 投影行（无 key_hash/owner_id）可解析
        let pub_row = KeyPublic::from_row(&json!({
            "id": "a1b2c3", "name": "default", "prefix": "sk-x...y",
            "created_at": 1_700_000_000, "status": "active", "last_used": null,
            "usage_requests": 0, "usage_prompt_tokens": 0,
            "usage_completion_tokens": 0, "usage_total_tokens": 0,
        }))
        .unwrap();
        assert_eq!(pub_row.id, "a1b2c3");
    }

    #[test]
    fn public_json_matches_legacy_field_contract() {
        let rec = KeyRec {
            id: "a1b2c3".into(),
            owner_id: "github:12345".into(),
            name: "default".into(),
            key_hash: "h".into(),
            prefix: "sk-x...y".into(),
            created_at: 1_700_000_000,
            status: "active".into(),
            last_used: Some(1_700_000_100),
            usage_requests: 3,
            usage_prompt_tokens: 100,
            usage_completion_tokens: 200,
            usage_total_tokens: 300,
        };
        let v = rec.to_public_json();
        let obj = v.as_object().unwrap();
        // 字段序与 lug-server public_rec_json 一致（preserve_order 下插入序=输出序）
        let expected_order = ["id", "name", "prefix", "created_at", "status", "last_used", "usage"];
        let keys: Vec<String> = obj.keys().cloned().collect();
        assert_eq!(keys, expected_order.to_vec());
        // 不含敏感列
        assert!(obj.get("key_hash").is_none() && obj.get("owner_id").is_none());
        assert_eq!(
            obj["usage"],
            json!({
                "requests": 3, "prompt_tokens": 100,
                "completion_tokens": 200, "total_tokens": 300
            })
        );
        // KeyRec::public() 与 list_public 投影行等价
        assert_eq!(
            rec.public(),
            KeyPublic::from_row(&json!({
                "id": "a1b2c3", "name": "default", "prefix": "sk-x...y",
                "created_at": 1_700_000_000, "status": "active",
                "last_used": 1_700_000_100,
                "usage_requests": 3, "usage_prompt_tokens": 100,
                "usage_completion_tokens": 200, "usage_total_tokens": 300,
            }))
            .unwrap()
        );
    }

    /// D1Type 绑定烟测：`create` 参数在 i64 全域有效（时间戳 > i32::MAX 前的
    /// 2038 问题上限由 Real 承载）。
    #[test]
    fn params_map_to_d1_types() {
        let params = record_usage_params("kid", u64::MAX, 5, 1_700_000_000);
        assert!(matches!(params[0].to_d1_type(), D1Type::Real(v) if v == 1.0));
        assert!(matches!(params[1].to_d1_type(), D1Type::Real(v) if v == i64::MAX as f64));
        assert!(matches!(params[4].to_d1_type(), D1Type::Text("kid")));
    }
}
