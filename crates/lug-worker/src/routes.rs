//! 路由与处理器：与 lug-server/src/routes.rs 契约逐项对齐（路由表 / CORS /
//! 错误结构 / fallback）。本模块为**公共纯逻辑**：不依赖 `worker`，host
//! 可编译可测（wasm 侧经 `entry` 挂到 `#[event(fetch)]`）。
//!
//! T4 骨架：/healthz 验活 + 其余路由 501 stub（T7 接业务逻辑）。
//! 持久化（D1/KV）由 T5 接入，OAuth 由 T6 接入——均经 fetch 的 `env` 取绑定。

use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

// ------------------------------------------------------------------ router

/// lug 路由表（与 lug-server `router()` 同表）。
/// T5/T6 接入持久化与 OAuth 后，此处追加 `.with_state(...)`。
pub fn router() -> Router {
    Router::new()
        // 验活（JSON 状态体）
        .route("/healthz", get(healthz))
        // 静态页面：T4 先挂 501 stub 占位；T7 决定去留——若改由 Workers
        // Static Assets 承载（wrangler.toml [assets]），此处可整体删除。
        .route("/", get(stub))
        .route("/index.html", get(stub))
        .route("/docs", get(stub))
        .route("/platform", get(stub))
        .route("/assets/{name}", get(stub))
        // 计算 API
        .route("/f/a/o/operator", get(stub))
        .route("/f/a/o/calculate", post(stub))
        // 控制台（GitHub OAuth + Key 管理）
        .route("/f/a/o/platform/auth", get(stub))
        .route("/f/a/o/platform/me", get(stub))
        .route("/f/a/o/platform/usage", get(stub))
        .route("/f/a/o/platform/auth/github", get(stub))
        .route("/f/a/o/platform/auth/github/callback", get(stub))
        .route("/f/a/o/platform/logout", post(stub))
        .route("/f/a/o/platform/keys", get(stub).post(stub))
        .route("/f/a/o/platform/keys/{id}", axum::routing::delete(stub))
        // 静态页面（/ /docs /platform /assets/*）由 Workers Static Assets
        // 承载（run_worker_first 只拦 /f/a/o/*），axum 不再重复路由。
        .fallback(fallback)
        // 已注册路径的未支持方法（如 OPTIONS/DELETE）也走统一兜底
        .method_not_allowed_fallback(fallback)
        .layer(axum::middleware::from_fn(cors_middleware))
}

// ------------------------------------------------------------------ CORS 中间件

/// CORS/安全头中间件（镜像原型 `_cors`）。注意：axum 0.8 的
/// `method_not_allowed_fallback` 不经过 `Router::layer`，因此 fallback
/// 响应需自行调用 [`apply_cors`]。
pub async fn cors_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let mut res = next.run(req).await;
    apply_cors(&path, res.headers_mut());
    res
}

/// CORS/安全头公共实现：平台路由收紧（no-store / no-referrer），
/// 计算 API 公开 CORS。
pub fn apply_cors(path: &str, headers: &mut HeaderMap) {
    if path.starts_with("/f/a/o/platform/") {
        headers.insert("Cache-Control", HeaderValue::from_static("no-store"));
        headers.insert("Referrer-Policy", HeaderValue::from_static("no-referrer"));
    } else {
        headers.insert("Access-Control-Allow-Origin", HeaderValue::from_static("*"));
        headers.insert(
            "Access-Control-Allow-Headers",
            HeaderValue::from_static("Authorization, Content-Type"),
        );
        headers.insert(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
    }
}

// ------------------------------------------------------------------ 错误结构

/// OpenAI 风格错误结构（`{"error": {message, type, param, code}}`），
/// 与 lug-server/Python 原型逐字节契约一致。
pub fn api_error(
    status: StatusCode,
    message: &str,
    err_type: &str,
    code: Option<&str>,
) -> Response {
    let body = json!({
        "error": {"message": message, "type": err_type, "param": null, "code": code}
    });
    (status, Json(body)).into_response()
}

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

// ------------------------------------------------------------------ handlers

/// 验活：200 + JSON 状态体（自定结构，供部署探针/冒烟用）。
async fn healthz() -> Response {
    json_response(
        StatusCode::OK,
        json!({"ok": true, "service": "lug-worker", "phase": "skeleton"}),
    )
}

/// T7 集成前的占位：路由表已就位，业务逻辑待迁移。
/// 统一 api_error 结构 + 自定 `not_implemented_error` type（与计费类错误区分）。
async fn stub(req: axum::extract::Request) -> Response {
    api_error(
        StatusCode::NOT_IMPLEMENTED,
        &format!(
            "{} is registered but its handler has not been migrated to Workers yet (T4 skeleton).",
            req.uri().path()
        ),
        "not_implemented_error",
        None,
    )
}

/// 全部未匹配路由：OPTIONS → 204 + CORS；其余 → OpenAI 风格 404
/// （文案列出 v1 实际可用路由——文档写对原则，平移自 lug-server）。
async fn fallback(method: axum::http::Method, req: axum::extract::Request) -> Response {
    let path = req.uri().path().to_string();
    let mut res = match method {
        axum::http::Method::OPTIONS => (StatusCode::NO_CONTENT, "").into_response(),
        axum::http::Method::POST => api_error(
            StatusCode::NOT_FOUND,
            &format!("Unknown request URL (POST {path}). Only POST /f/a/o/calculate is available."),
            "invalid_request_error",
            None,
        ),
        method => api_error(
            StatusCode::NOT_FOUND,
            &format!(
                "Unknown request URL ({method} {path}). Available: / /docs /platform, \
                 GET /f/a/o/operator, GET /f/a/o/platform/auth|me|keys|usage, \
                 POST /f/a/o/calculate"
            ),
            "invalid_request_error",
            None,
        ),
    };
    // axum 0.8 的 method_not_allowed_fallback 不经过 Router::layer，CORS 需手动补
    apply_cors(&path, res.headers_mut());
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body as AxumBody;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower_service::Service;

    async fn send(method: &str, uri: &str) -> Response {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .body(AxumBody::empty())
            .unwrap();
        let mut svc = router();
        svc.call(req).await.unwrap()
    }

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = BodyExt::collect(res.into_body()).await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn healthz_returns_200_json_ok() {
        let res = send("GET", "/healthz").await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers()["content-type"],
            "application/json"
        );
        let v = body_json(res).await;
        assert_eq!(v["ok"], true);
        assert_eq!(v["service"], "lug-worker");
    }

    #[tokio::test]
    async fn every_registered_route_is_501_not_implemented() {
        let table = [
            ("GET", "/"),
            ("GET", "/index.html"),
            ("GET", "/docs"),
            ("GET", "/platform"),
            ("GET", "/assets/app.css"),
            ("GET", "/f/a/o/operator"),
            ("POST", "/f/a/o/calculate"),
            ("GET", "/f/a/o/platform/auth"),
            ("GET", "/f/a/o/platform/me"),
            ("GET", "/f/a/o/platform/auth/github"),
            ("GET", "/f/a/o/platform/auth/github/callback"),
            ("POST", "/f/a/o/platform/logout"),
            ("GET", "/f/a/o/platform/keys"),
            ("POST", "/f/a/o/platform/keys"),
            ("DELETE", "/f/a/o/platform/keys/k-123"),
            ("GET", "/f/a/o/platform/usage"),
        ];
        for (m, u) in table {
            let res = send(m, u).await;
            assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED, "{m} {u}");
            let v = body_json(res).await;
            assert_eq!(v["error"]["type"], "not_implemented_error", "{m} {u}");
            assert!(v["error"]["param"].is_null(), "{m} {u}");
            assert!(!v["error"]["message"].as_str().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn public_api_gets_open_cors_headers() {
        // OPTIONS 打在仅 POST 的路由上 → method_not_allowed_fallback → 204 + CORS
        let res = send("OPTIONS", "/f/a/o/calculate").await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(res.headers()["Access-Control-Allow-Origin"], "*");
        assert_eq!(
            res.headers()["Access-Control-Allow-Headers"],
            "Authorization, Content-Type"
        );
        assert_eq!(
            res.headers()["Access-Control-Allow-Methods"],
            "GET, POST, OPTIONS"
        );
    }

    #[tokio::test]
    async fn platform_routes_get_no_store_no_referrer() {
        let res = send("GET", "/f/a/o/platform/me").await;
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(res.headers()["Cache-Control"], "no-store");
        assert_eq!(res.headers()["Referrer-Policy"], "no-referrer");
        assert!(res.headers().get("Access-Control-Allow-Origin").is_none());
    }

    #[tokio::test]
    async fn unknown_route_is_contract_404_with_cors() {
        let res = send("GET", "/nope").await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(res.headers()["Access-Control-Allow-Origin"], "*");
        let v = body_json(res).await;
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("Unknown request URL (GET /nope)"));
    }
}
