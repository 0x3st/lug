//! lug-worker 存储层（T5-storage）：D1（api_keys / audit_log / oauth_states）
//! 与 KV（sessions）后端，替换 lug-server 的文件/内存实现。
//!
//! 类型边界对齐 `crates/lug-server/src/{keystore.rs, audit.rs, platform.rs}`
//! （只读参照，不改）：方法名与语义一一对应，差异只有三处，均为平台使然：
//! 1. 随机性注入：原实现在存储层内用 `rand` 生成 raw key / id / token；wasm32 无
//!    getrandom 后端（见 Cargo.toml 注释），随机字节一律由调用方经 Workers crypto
//!    生成后传入（T6 提供），本层只做纯派生（hash/prefix/record）。
//! 2. 时间注入：原实现内部取 `SystemTime::now()`；本层公开方法显式收 `now: i64`
//!    （Unix 秒），由调用方经 `Date::now()/1000` 供给——纯函数构造可离线单测。
//!    `audit::record` 例外：为保持 `record(&entry)` 签名，内部取 `Date::now()`。
//! 3. 异步 + `worker::Result`：所有交互方法 `async fn ... -> Result<_, worker::Error>`。
//!
//! 实现约束：Cargo.toml 禁改 ⇒ `serde`（derive）不是本 crate 直接依赖，行反序列化
//! 一律经 `first::<serde_json::Value>` / `results::<serde_json::Value>` 取回后手工
//! 解析（[`row`] 辅助函数），序列化走 `serde_json::json!`；SQL 参数用 worker 自带
//! 的 [`worker::D1Type`] + `bind_refs`，不直接触碰 `wasm_bindgen::JsValue`。
//! D1 API 约束（lug-migration/notes/workers-rs-tech-notes.md §3）：匿名/`?N` 序号
//! 参数（无命名参数）、整数经 JS number（≤2^53 精确）、`RETURNING` 行经
//! `all()` 取回。真实 D1/KV 运行时交互留 T7 集成验证；本层单测覆盖 SQL/参数
//! 构造、行解析与序列化逻辑。

pub mod audit;
pub mod keystore;
pub mod oauth_states;
pub mod sessions;

use worker::{D1Database, D1PreparedStatement, D1Type};

/// SQL 参数的可测试中间表示：各模块用它构造参数列表（单测可整表断言），
/// 运行时经 [`SqlParam::to_d1_type`] 转 `D1Type` 绑定。
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SqlParam {
    Text(String),
    /// i64 经 `D1Type::Real`（= worker 内部 `JsValue::from_f64`，与 `Integer(i32)`
    /// 的转换一致）：整数值 ≤ 2^53 精确，D1 将整型 JS number 存为 SQLite
    /// INTEGER(i64)。秒级时间戳与计数器均适用（`Integer(i32)` 会被 2038 截断，
    /// 且装不下累计 token 数，故统一走 Real）。
    Int(i64),
}

impl SqlParam {
    pub(crate) fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub(crate) fn int(v: i64) -> Self {
        Self::Int(v)
    }

    pub(crate) fn to_d1_type(&self) -> D1Type<'_> {
        match self {
            Self::Text(s) => D1Type::Text(s),
            Self::Int(v) => D1Type::Real(*v as f64),
        }
    }
}

/// prepare + 按序绑定（`?N` 匿名参数；D1 不支持 `:name` 命名参数）。
pub(crate) fn prepare<'a>(
    db: &D1Database,
    sql: &str,
    params: &'a [SqlParam],
) -> worker::Result<D1PreparedStatement> {
    let types: Vec<D1Type<'a>> = params.iter().map(SqlParam::to_d1_type).collect();
    db.prepare(sql).bind_refs(&types)
}

/// `u64 → i64` 饱和转换（SQLite INTEGER = i64；计数器超 i64::MAX 不现实，但
/// 静默回绕不可接受，钳到 i64::MAX）。
pub(crate) fn sat_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// D1 行（`serde_json::Value` 对象）的字段读取辅助。
///
/// D1 经 serde_wasm_bindgen 把 JS 行对象转成 `Value`：TEXT→`Value::String`、
/// INTEGER→`Value::Number`、NULL→`Value::Null`。列缺失（如 `list_public` 投影）
/// 与类型不符都按 `Err` 处理——行结构由 migrations 保证，这里做防线。
pub(crate) mod row {
    use serde_json::Value;
    use worker::Error;

    fn err(col: &str, why: &str) -> Error {
        Error::from(format!("d1 row: column `{col}` {why}"))
    }

    /// 不针对具体列的结构性错误（如缺失嵌套对象）。
    pub(crate) fn err_plain(msg: &str) -> Error {
        Error::from(msg.to_string())
    }

    pub(crate) fn as_object<'a>(v: &'a Value) -> Result<&'a serde_json::Map<String, Value>, Error> {
        v.as_object().ok_or_else(|| err("<row>", "is not an object"))
    }

    pub(crate) fn text(r: &serde_json::Map<String, Value>, col: &str) -> Result<String, Error> {
        r.get(col)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| err(col, "is not a string"))
    }

    pub(crate) fn i64(r: &serde_json::Map<String, Value>, col: &str) -> Result<i64, Error> {
        r.get(col)
            .and_then(Value::as_i64)
            .ok_or_else(|| err(col, "is not an integer"))
    }

    pub(crate) fn u64(r: &serde_json::Map<String, Value>, col: &str) -> Result<u64, Error> {
        r.get(col)
            .and_then(Value::as_u64)
            .ok_or_else(|| err(col, "is not a non-negative integer"))
    }

    /// NULL → `None`（如 `last_used`）；列缺失同样视为 NULL（投影容差）。
    pub(crate) fn opt_i64(
        r: &serde_json::Map<String, Value>,
        col: &str,
    ) -> Result<Option<i64>, Error> {
        match r.get(col) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_i64()
                .map(Some)
                .ok_or_else(|| err(col, "is not an integer")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sql_param_maps_to_d1_type() {
        assert!(matches!(
            SqlParam::text("sk-abc").to_d1_type(),
            D1Type::Text("sk-abc")
        ));
        assert!(matches!(SqlParam::int(42).to_d1_type(), D1Type::Real(v) if v == 42.0));
    }

    #[test]
    fn sat_i64_saturates_instead_of_wrapping() {
        assert_eq!(sat_i64(0), 0);
        assert_eq!(sat_i64(42), 42);
        assert_eq!(sat_i64(u64::MAX), i64::MAX);
    }

    #[test]
    fn row_helpers_parse_and_reject() {
        let row = json!({
            "id": "a1b2c3",
            "created_at": 1_700_000_000,
            "last_used": null,
            "usage_requests": 3,
        });
        let r = row.as_object().unwrap();
        assert_eq!(row::text(r, "id").unwrap(), "a1b2c3");
        assert_eq!(row::i64(r, "created_at").unwrap(), 1_700_000_000);
        assert_eq!(row::u64(r, "usage_requests").unwrap(), 3);
        assert_eq!(row::opt_i64(r, "last_used").unwrap(), None);
        assert!(row::opt_i64(r, "missing_column").unwrap().is_none());
        assert!(row::text(r, "created_at").is_err());
        assert!(row::i64(r, "id").is_err());
        assert!(row::as_object(&json!(42)).is_err());
    }
}
