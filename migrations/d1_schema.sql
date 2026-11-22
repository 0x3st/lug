-- lug D1 schema（T5-storage）
-- 依 lug-migration/design/persistence.md §2 逐条落定；全部语句幂等（IF NOT EXISTS），
-- 可安全重复执行（wrangler d1 execute lug-db --remote/--local --file migrations/d1_schema.sql，
-- 或纳入 wrangler d1 migrations）。
--
-- 分工原则（§1）：安全关键的一次性语义放 D1（oauth_states），纯缓存型/可过期状态放 KV
-- （sessions，见 crates/lug-worker/src/storage/sessions.rs，不在此建表）。
-- 时间一律 Unix 秒（与原 JSONL/JSON 实现的 int(time.time()) 一致）。
-- 参数绑定注意（workers-rs §3）：D1Type::Integer = i32 / JS number ≤ 2^53 精确；
-- 本 schema 的 INTEGER 列在应用层一律以 i64 值经 JsValue::from_f64 绑定。

-- ---------------------------------------------------------------- api_keys（§2.1）
-- 对齐 lug-server keystore.rs 的 KeyRec：usage 嵌套对象拍平为 4 列。
CREATE TABLE IF NOT EXISTS api_keys (
  id                      TEXT    PRIMARY KEY,              -- 6 随机字节 hex（12 字符），对应 KeyRec.id
  owner_id                TEXT    NOT NULL DEFAULT 'local', -- 'github:<uid>'；历史缺省 'local'（KeyRec serde default 同语义）
  name                    TEXT    NOT NULL,                 -- ≤40 chars（应用层截断，保持不变）
  key_hash                TEXT    NOT NULL,                 -- SHA-256 hex（64 字符）；明文永不落库
  prefix                  TEXT    NOT NULL,                 -- 展示前缀 'sk-xxxxx...yyyy'
  created_at              INTEGER NOT NULL,                 -- Unix 秒
  status                  TEXT    NOT NULL DEFAULT 'active'
                          CHECK (status IN ('active','revoked')),
  last_used               INTEGER,                          -- Unix 秒；NULL=从未使用
  usage_requests          INTEGER NOT NULL DEFAULT 0,
  usage_prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  usage_completion_tokens INTEGER NOT NULL DEFAULT 0,
  usage_total_tokens      INTEGER NOT NULL DEFAULT 0
);

-- validate 点查：raw→SHA-256 在应用层，按 hash 命中且 active。
-- 唯一索引兼作完整性护栏：原 JSON 实现对重复 id/hash 是静默覆盖，D1 改为约束报错。
CREATE UNIQUE INDEX IF NOT EXISTS idx_api_keys_key_hash ON api_keys(key_hash);

-- list_public / revoke_scoped 按 owner 过滤（复合 status 便于后续按态扫描）。
CREATE INDEX IF NOT EXISTS idx_api_keys_owner ON api_keys(owner_id, status);

-- ---------------------------------------------------------------- audit_log（§2.2）
-- 对齐 lug-server audit.rs 的 JSONL 字段：usage 嵌套拍平为 2 列。
CREATE TABLE IF NOT EXISTS audit_log (
  id                INTEGER PRIMARY KEY AUTOINCREMENT, -- 单调递增；保留期删除后不复用
  ts                INTEGER NOT NULL,                  -- Unix 秒
  key_id            TEXT    NOT NULL,
  key_prefix        TEXT    NOT NULL,
  request_id        TEXT    NOT NULL,
  expression        TEXT    NOT NULL,                  -- ≤ LUG_MAX_EXPR_LEN(1024) 字符
  kind              TEXT    NOT NULL CHECK (kind IN ('ok','error')),
  result            TEXT    NOT NULL,                  -- ok: 格式化结果；error: 错误消息
  prompt_tokens     INTEGER NOT NULL DEFAULT 0,
  completion_tokens INTEGER NOT NULL DEFAULT 0,
  duration_ms       INTEGER NOT NULL
);

-- 审计主视图：按时间范围查询。
CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_log(ts);

-- 计费追溯主路径：按 Key 查询（复合索引同时覆盖 key_id 等值与 ts 排序）。
CREATE INDEX IF NOT EXISTS idx_audit_key_ts ON audit_log(key_id, ts DESC);

-- 排障：按 request_id 串链路。
CREATE INDEX IF NOT EXISTS idx_audit_request ON audit_log(request_id);

-- ---------------------------------------------------------------- oauth_states（§2.3）
-- StateStore 的新家。关键决定：**不入 KV**——KV 无原子删除且删除传播 ≤60s，
-- GET+DELETE 竞态防不住 state 重放（persistence.md §3.3）；一次性消费必须靠
-- 单条 DELETE … RETURNING 的语句级原子性（见 storage/oauth_states.rs::consume）。
--
-- 字段按 lug-server platform.rs 的 PendingState 现有字段（verifier/expires；seq 为
-- 进程内淘汰序号，随 MAX_OAUTH_STATES 淘汰一并废弃）。PKCE code_challenge 不落库：
-- platform.rs 在 /auth/github/start 时由 verifier 现算（S256 = base64url(SHA256(verifier))），
-- 存了反而是第二份真相。
CREATE TABLE IF NOT EXISTS oauth_states (
  state    TEXT PRIMARY KEY,     -- token_urlsafe(32)，43 字符 base64url
  verifier TEXT NOT NULL,        -- PKCE code_verifier，token_urlsafe(48)
  expires  INTEGER NOT NULL      -- Unix 秒 = now + 600（STATE_TTL）
);

-- 过期清理（Cron 每 5 分钟兜底：未被消费的过期行）与过期判断共用。
CREATE INDEX IF NOT EXISTS idx_oauth_states_expires ON oauth_states(expires);
