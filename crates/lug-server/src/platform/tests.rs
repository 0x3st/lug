//! platform 模块单元测试：配置校验 / 身份归一 / state 一次性。

use super::*;

fn cfg(public_url: &str) -> Result<AuthConfig, String> {
    AuthConfig::from_parts("cid", "secret", public_url, "")
}

#[test]
fn auth_config_closed_without_credentials_but_not_admin_only() {
    assert!(!AuthConfig::default().github_enabled());
    assert!(cfg("https://lug.example.com").unwrap().github_enabled());
    // 本机 HTTP 亦可用（本地开发）
    let local = cfg("http://127.0.0.1:8125").unwrap();
    assert!(local.github_enabled());
    assert_eq!(
        local.redirect_uri(),
        "http://127.0.0.1:8125/f/a/o/platform/auth/github/callback"
    );
    // 公网地址末尾多余斜杠会被剥离
    assert_eq!(
        cfg("https://lug.example.com/").unwrap().public_url,
        "https://lug.example.com"
    );
}

#[test]
fn auth_config_rejects_non_origin_public_url() {
    for bad in [
        "ftp://x.com",
        "https://x.com/path",
        "https://x.com?q=1",
        "https://user:pw@x.com",
        "https://",
        "not-a-url",
    ] {
        assert!(cfg(bad).is_err(), "should reject {bad}");
    }
}

#[test]
fn owner_id_normalizes_and_validates() {
    let ok = |id: &str| {
        owner_id(&User {
            provider: "github".into(),
            id: id.into(),
            login: "x".into(),
        })
    };
    assert_eq!(ok("101").unwrap(), "github:101");
    assert_eq!(ok("007").unwrap(), "github:7");
    assert!(ok("0").is_err());
    assert!(ok("-1").is_err());
    assert!(ok("").is_err());
    assert!(ok("١٢").is_err()); // 非 ASCII 数字
    assert!(owner_id(&User {
        provider: "local".into(),
        id: "1".into(),
        login: "x".into(),
    })
    .is_err());
}

#[test]
fn oauth_state_is_bound_one_time() {
    let states = StateStore::new();
    let (state, verifier) = states.create();
    // 浏览器 Cookie 不匹配 / 缺失 → 拒绝
    assert_eq!(
        states.consume(&state, ""),
        Err(OAuthError::InvalidOauthState)
    );
    assert_eq!(
        states.consume(&state, "tampered"),
        Err(OAuthError::InvalidOauthState)
    );
    assert!(states.consume(&state, &state).is_ok());
    // 重放 → 拒绝
    assert_eq!(
        states.consume(&state, &state),
        Err(OAuthError::InvalidOauthState)
    );
    assert!(!verifier.is_empty());
}

#[test]
fn session_store_expires_and_revokes() {
    let store = SessionStore::new(1);
    let user = User {
        provider: "github".into(),
        id: "42".into(),
        login: "dev".into(),
    };
    let token = store.create(user.clone()).unwrap();
    let rec = store.get(&token).unwrap();
    assert_eq!(rec.user, user);
    assert!(!rec.csrf_token.is_empty());
    store.revoke(&token);
    assert!(store.get(&token).is_none());
    // 匿名/本地身份绝不发会话
    assert!(store
        .create(User {
            provider: "local".into(),
            id: "1".into(),
            login: "x".into(),
        })
        .is_err());
}
