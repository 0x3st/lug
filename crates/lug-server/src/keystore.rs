//! API Key 存储：JSON 文件持久化，**格式与 Python 原型完全兼容**
//! （同一 `api_keys.json` 可被两侧互换读写，回滚零成本）。
//!
//! 安全语义（与原型一致）：
//! - 明文只在创建时返回一次，落库存 SHA-256；
//! - 原子写（同目录临时文件 + rename，权限 0600）；
//! - 校验命中且 `status == "active"` 才放行；
//! - 用量按 Key 累计（requests / prompt / completion / total）。
//!
//! 并发模型：`std::sync::Mutex` 短临界区（写量 < 10KB）。SQLite 为 fast-follow
//! （§4.4），届时经同一类型边界替换。

use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::platform::base64url;

/// 单条 Key 记录（JSON 字段与原型逐一对应）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyRec {
    pub id: String,
    /// 原型早期记录没有此字段（旧密钥迁移前），缺失视为 "local"
    /// （Python `.get("owner_id", "local")` 同语义）。
    #[serde(default = "default_owner")]
    pub owner_id: String,
    pub name: String,
    pub key_hash: String,
    pub prefix: String,
    pub created_at: i64,
    pub status: String,
    pub last_used: Option<i64>,
    pub usage: Usage,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// 持久化外层结构（与原型 JSON 顶层一致：`{"keys": {...}}`）。
#[derive(Serialize, Deserialize)]
struct StoreFile {
    keys: BTreeMap<String, KeyRec>,
}

pub struct KeyStore {
    path: PathBuf,
    data: Mutex<BTreeMap<String, KeyRec>>,
}

/// 原型语义：无 owner_id 的历史记录视作 local。
fn default_owner() -> String {
    "local".to_string()
}

fn sha256_hex(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// 当前 Unix 秒（与原型 `int(time.time())` 一致）。
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 原子写：同目录临时文件 + rename，权限 0600（镜像原型 `_save`）。
fn atomic_write(path: &Path, data: &BTreeMap<String, KeyRec>) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        let body = serde_json::to_string_pretty(&StoreFile { keys: data.clone() })?;
        f.write_all(body.as_bytes())?;
        f.flush()?;
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("rename {} -> {}: {e}", tmp.display(), path.display()),
        )
    })?;
    Ok(())
}

impl KeyStore {
    /// 打开（或惰性创建）Key 存储。文件损坏时拒绝启动（不覆盖用户数据），
    /// 与原型行为一致。
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let data = if path.exists() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("Cannot read API key store: {e}"))?;
            match serde_json::from_str::<StoreFile>(&raw) {
                Ok(f) => f.keys,
                Err(e) => {
                    return Err(format!(
                        "Cannot read API key store; refusing to overwrite it. ({e})"
                    ))
                }
            }
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
        })
    }

    /// 签发新 Key：明文仅本次返回，落库 SHA-256。
    /// `raw = "sk-" + base64url(36 随机字节)`（等价 Python `token_urlsafe(36)`）。
    pub fn create(&self, name: Option<String>, owner_id: &str) -> Result<(String, KeyRec), String> {
        let mut buf = [0u8; 36];
        rand::rng().fill_bytes(&mut buf);
        let raw = format!("sk-{}", base64url(&buf));
        let mut kid_buf = [0u8; 6];
        rand::rng().fill_bytes(&mut kid_buf);
        let kid: String = kid_buf.iter().map(|x| format!("{x:02x}")).collect();
        let rec = KeyRec {
            id: kid.clone(),
            owner_id: owner_id.to_string(),
            name: {
                let n = name.unwrap_or_else(|| "default".into());
                n.chars().take(40).collect()
            },
            key_hash: sha256_hex(&raw),
            // 展示用前缀：前 11 字符 + "..." + 末 4 字符（不含完整密钥）
            prefix: format!("{}...{}", &raw[..11], &raw[raw.len() - 4..]),
            created_at: unix_now(),
            status: "active".into(),
            last_used: None,
            usage: Usage::default(),
        };
        let mut data = self.data.lock().unwrap();
        data.insert(kid, rec.clone());
        atomic_write(&self.path, &data).map_err(|e| format!("persist failed: {e}"))?;
        Ok((raw, rec))
    }

    /// 校验明文 Key：命中且 active 返回记录。
    pub fn validate(&self, raw: &str) -> Option<KeyRec> {
        if !raw.starts_with("sk-") {
            return None;
        }
        let h = sha256_hex(raw);
        let data = self.data.lock().unwrap();
        data.values()
            .find(|r| r.key_hash == h && r.status == "active")
            .cloned()
    }

    /// 记录一次成功调用的用量（先落账后播流，§3.7）。
    pub fn record_usage(&self, kid: &str, prompt: u64, completion: u64) {
        let mut data = self.data.lock().unwrap();
        if let Some(rec) = data.get_mut(kid) {
            rec.usage.requests += 1;
            rec.usage.prompt_tokens += prompt;
            rec.usage.completion_tokens += completion;
            rec.usage.total_tokens += prompt + completion;
            rec.last_used = Some(unix_now());
            let _ = atomic_write(&self.path, &data);
        }
    }

    /// 列出某 owner 的全部 Key（剥离 key_hash / owner_id，与原型 list 一致）。
    pub fn list(&self, owner_id: &str) -> Vec<KeyRec> {
        let data = self.data.lock().unwrap();
        data.values()
            .filter(|r| r.owner_id == owner_id)
            .cloned()
            .collect()
    }

    /// 列出某 owner 的 Key 的公开 JSON（不含 key_hash / owner_id，字段序同原型）。
    pub fn list_public(&self, owner_id: &str) -> Vec<serde_json::Value> {
        self.list(owner_id).iter().map(public_rec_json).collect()
    }

    /// 撤销（即时生效）；未命中返回 false。
    pub fn revoke(&self, kid: &str) -> bool {
        let mut data = self.data.lock().unwrap();
        if let Some(rec) = data.get_mut(kid) {
            rec.status = "revoked".into();
            let _ = atomic_write(&self.path, &data);
            true
        } else {
            false
        }
    }

    /// 按 owner 撤销（IDOR 防护：别人的 Key 一律未命中）；返回撤销后的记录。
    /// owner 作用域撤销：仅 active 可撤销（重复撤销 → 404 语义）；
    /// 他人的 Key 一律 None（IDOR 防护）。
    pub fn revoke_scoped(&self, kid: &str, owner_id: &str) -> Option<KeyRec> {
        let mut data = self.data.lock().unwrap();
        let rec = data.get_mut(kid)?;
        if rec.owner_id != owner_id || rec.status != "active" {
            return None;
        }
        rec.status = "revoked".into();
        let snapshot = rec.clone();
        let _ = atomic_write(&self.path, &data);
        Some(snapshot)
    }

    /// 历史单用户密钥的显式一次性迁移：`LUG_LEGACY_OWNER_GITHUB_ID` 配置后，
    /// 缺失 owner_id 的记录在启动时归入 `github:<id>`（与原型语义一致，持久化）。
    pub fn open_with_legacy_migration(
        path: PathBuf,
        legacy_owner_id: &str,
    ) -> Result<Self, String> {
        if path.exists() && !legacy_owner_id.is_empty() {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("Cannot read API key store: {e}"))?;
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&raw) {
                let mut changed = false;
                if let Some(keys) = v.get_mut("keys").and_then(|k| k.as_object_mut()) {
                    for rec in keys.values_mut() {
                        if let Some(obj) = rec.as_object_mut() {
                            if !obj.contains_key("owner_id") {
                                obj.insert(
                                    "owner_id".into(),
                                    serde_json::Value::String(format!("github:{legacy_owner_id}")),
                                );
                                changed = true;
                            }
                        }
                    }
                }
                if changed {
                    let map: BTreeMap<String, KeyRec> = serde_json::from_value(
                        v.get("keys").cloned().unwrap_or(serde_json::Value::Null),
                    )
                    .map_err(|e| {
                        format!("Cannot read API key store; refusing to overwrite it. ({e})")
                    })?;
                    atomic_write(&path, &map).map_err(|e| format!("persist failed: {e}"))?;
                }
            }
        }
        Self::open(path)
    }
}

/// 公开视图：id / name / prefix / created_at / status / last_used / usage（原型字段序）。
fn public_rec_json(r: &KeyRec) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "name": r.name,
        "prefix": r.prefix,
        "created_at": r.created_at,
        "status": r.status,
        "last_used": r.last_used,
        "usage": {
            "requests": r.usage.requests,
            "prompt_tokens": r.usage.prompt_tokens,
            "completion_tokens": r.usage.completion_tokens,
            "total_tokens": r.usage.total_tokens,
        },
    })
}
