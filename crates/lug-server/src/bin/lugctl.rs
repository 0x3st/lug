//! `lugctl`：Key 管理 CLI（v1-alpha 期间替代控制台签发流程）。
//!
//! 用法：
//! ```text
//! lugctl create-key [name] [--owner <owner_id>]   # 签发（明文仅本次打印）
//! lugctl list-keys [--owner <owner_id>]           # 列表
//! lugctl revoke-key <id>                          # 撤销
//! ```

use lug_server::keystore::KeyStore;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");
    let owner = default_owner();

    match (cmd, args.get(1).map(String::as_str)) {
        ("create-key", name) => {
            let name = name.map(str::to_string);
            match keystore().and_then(|ks| ks.create(name, &owner)) {
                Ok((raw, rec)) => {
                    println!("密钥（仅本次显示，妥善保存）:");
                    println!("  {raw}");
                    println!(
                        "记录: id={} prefix={} owner={}",
                        rec.id, rec.prefix, rec.owner_id
                    );
                }
                Err(e) => die(&e),
            }
        }
        ("list-keys", _) => match keystore() {
            Ok(ks) => {
                for rec in ks.list(&owner) {
                    println!(
                        "{}  {:<10} status={} usage={}/{} tok",
                        rec.id, rec.name, rec.status, rec.usage.requests, rec.usage.total_tokens
                    );
                }
            }
            Err(e) => die(&e),
        },
        ("revoke-key", Some(id)) => match keystore() {
            Ok(ks) => {
                if ks.revoke(id) {
                    println!("已撤销: {id}");
                } else {
                    die("未找到该 id");
                }
            }
            Err(e) => die(&e),
        },
        _ => {
            eprintln!("用法: lugctl <create-key [name] | list-keys | revoke-key <id>>");
            std::process::exit(2);
        }
    }
}

fn keystore() -> Result<KeyStore, String> {
    let path = std::env::var("API_KEYS_PATH").unwrap_or_else(|_| "api_keys.json".into());
    KeyStore::open(path.into())
}

/// v1-alpha 单租户默认 owner（与原型 list 的缺省值一致）。
fn default_owner() -> String {
    std::env::var("LUG_OWNER_ID").unwrap_or_else(|_| "local".into())
}

fn die(msg: &str) {
    eprintln!("[lugctl] {msg}");
    std::process::exit(1);
}
