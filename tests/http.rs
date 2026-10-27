//! HTTP-level tests against the full router on an in-memory database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hookforms::config::Config;
use hookforms::{AppState, db};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

async fn app() -> Router {
    let config = Config::from_map([
        ("ADMIN_API_KEY", "test-admin"),
        ("DATABASE_URL", "sqlite::memory:"),
    ])
    .unwrap();
    let pool = db::init(&config.database_url).await.unwrap();
    hookforms::build_app(AppState::new(pool, config, reqwest::Client::new()))
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, headers, body)
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn health_is_ok() {
    let app = app().await;
    let (status, headers, body) = send(&app, get("/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "healthy");
    assert_eq!(body["version"], "0.2.0");
    assert_eq!(body["checks"]["database"], "ok");
    assert!(body["timestamp"].as_str().unwrap().ends_with("+00:00"));
    assert_eq!(headers["x-frame-options"], "DENY");
    assert!(
        headers.get("x-ratelimit-limit").is_none(),
        "health skips the rate limit"
    );
}

#[tokio::test]
async fn root_reports_name_status_version() {
    let app = app().await;
    let (status, headers, body) = send(&app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        serde_json::json!({"name": "HookForms", "status": "ok", "version": "0.2.0"})
    );
    assert_eq!(headers["x-ratelimit-limit"], "100");
    assert_eq!(headers["x-ratelimit-remaining"], "99");
}

#[tokio::test]
async fn routing_errors_use_starlette_shape() {
    let app = app().await;
    let (status, _, body) = send(&app, get("/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, serde_json::json!({"detail": "Not Found"}));

    let req = Request::post("/health").body(Body::empty()).unwrap();
    let (status, _, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body, serde_json::json!({"detail": "Method Not Allowed"}));
}

/// Starlette answers a wrong method with `Allow` listing the methods of the
/// first route whose path matches (one route per FastAPI endpoint, in the
/// order the Python app registers them). Expected values come from the
/// Python app under TestClient.
#[tokio::test]
async fn method_not_allowed_sends_allow_like_starlette() {
    let app = app().await;
    let cases = [
        ("PUT", "/v1/auth/keys", "POST"),
        ("PATCH", "/v1/auth/keys", "POST"),
        ("GET", "/v1/auth/keys/abc", "DELETE"),
        ("PUT", "/v1/hooks/inboxes", "GET"),
        ("HEAD", "/v1/hooks/inboxes", "GET"),
        ("GET", "/v1/hooks/inboxes/abc", "PATCH"),
        ("POST", "/v1/hooks/abc/events", "GET"),
        ("DELETE", "/v1/hooks/abc/events/e1", "GET"),
        ("PUT", "/v1/hooks/inboxes/abc/channels", "POST"),
        ("GET", "/v1/hooks/inboxes/abc/channels/c1", "PATCH"),
        ("POST", "/v1/hooks/config/email-provider", "GET"),
        ("POST", "/v1/hooks/inboxes/events", "PATCH"),
        ("HEAD", "/hooks/abc", "GET, POST, PUT, PATCH, DELETE"),
        ("OPTIONS", "/hooks/abc", "GET, POST, PUT, PATCH, DELETE"),
        ("POST", "/", "GET"),
        ("POST", "/health", "GET"),
        // FastAPI routes do not answer HEAD for GET endpoints.
        ("HEAD", "/", "GET"),
        ("HEAD", "/health", "GET"),
        ("HEAD", "/v1/auth/keys", "POST"),
        ("HEAD", "/v1/hooks/inboxes/abc/channels", "POST"),
        ("HEAD", "/v1/hooks/config/email-provider", "GET"),
        ("HEAD", "/v1/hooks/abc/events/e1", "GET"),
    ];
    for (method, uri, allow) in cases {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-api-key", "test-admin")
            .body(Body::empty())
            .unwrap();
        let (status, headers, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        let got: Vec<_> = headers.get_all("allow").iter().collect();
        assert_eq!(got.len(), 1, "{method} {uri}: one Allow header");
        assert_eq!(got[0], allow, "{method} {uri}");
    }
}

#[tokio::test]
async fn auth_and_large_bodies_use_app_error_shape() {
    let app = app().await;
    let (status, _, body) = send(&app, get("/v1/auth/keys")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        serde_json::json!({"error": {"code": 401, "message": "Missing API key"}})
    );

    let req = Request::post("/hooks/contact")
        .header("content-length", (3 * 1024 * 1024).to_string())
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        body["error"]["message"],
        "Request body too large. Max size is 2 MB."
    );
}

#[tokio::test]
async fn rate_limit_after_100_requests() {
    let app = app().await;
    for _ in 0..100 {
        let req = Request::get("/")
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(&app, req).await.0, StatusCode::OK);
    }
    let req = Request::get("/")
        .header("x-forwarded-for", "203.0.113.9")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], 429);
    assert_eq!(headers["x-ratelimit-remaining"], "0");
}

#[tokio::test]
async fn options_without_preflight_headers_is_405_like_starlette() {
    let app = app().await;
    let options = |uri: &str| Request::options(uri);

    // No Origin / Access-Control-Request-Method: the router answers.
    let (status, _, body) = send(&app, options("/health").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body["detail"], "Method Not Allowed");

    // Origin alone is not a preflight either.
    let req = options("/health")
        .header("origin", "https://example.com")
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::METHOD_NOT_ALLOWED);

    // Unknown path: 404 as Starlette.
    let (status, _, body) = send(&app, options("/nope").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "Not Found");

    // A real preflight is still answered by the CORS layer.
    let req = options("/health")
        .header("origin", "https://example.com")
        .header("access-control-request-method", "GET")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send_raw(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers["access-control-allow-origin"],
        "https://example.com"
    );
}

async fn send_raw(app: &Router, req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn cors_matches_starlette() {
    let app = app().await;

    // No Origin: no CORS headers at all.
    let (_, headers, _) = send(&app, get("/")).await;
    assert!(!headers.contains_key("access-control-allow-credentials"));
    assert!(!headers.contains_key("access-control-allow-origin"));
    assert!(!headers.contains_key("vary"));

    // Simple request with Origin: `*` plus credentials, no Vary.
    let req = Request::get("/")
        .header("origin", "https://site.example")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["access-control-allow-origin"], "*");
    assert_eq!(headers["access-control-allow-credentials"], "true");
    assert!(!headers.contains_key("vary"));

    // Preflight: text/plain "OK", every method, max-age 600, Vary: Origin, no Allow.
    let req = Request::options("/v1/hooks/inboxes")
        .header("origin", "https://site.example")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "x-api-key,content-type")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send_raw(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "OK");
    assert_eq!(headers["content-type"], "text/plain; charset=utf-8");
    assert_eq!(
        headers["access-control-allow-methods"],
        "DELETE, GET, HEAD, OPTIONS, PATCH, POST, PUT"
    );
    assert_eq!(headers["access-control-max-age"], "600");
    assert_eq!(headers["vary"], "Origin");
    assert_eq!(
        headers["access-control-allow-origin"],
        "https://site.example"
    );
    assert_eq!(
        headers["access-control-allow-headers"],
        "x-api-key,content-type"
    );
    assert_eq!(headers["access-control-allow-credentials"], "true");
    assert!(!headers.contains_key("allow"));
    // The rate limit and security headers still apply, as in Python.
    assert_eq!(headers["x-frame-options"], "DENY");
    assert!(headers.contains_key("x-ratelimit-limit"));

    // Unknown method in a preflight: 400 like Starlette.
    let req = Request::options("/")
        .header("origin", "https://site.example")
        .header("access-control-request-method", "BREW")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send_raw(&app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, "Disallowed CORS method");
}

#[tokio::test]
async fn trailing_slash_redirects_like_starlette() {
    let app = app().await;
    let req = Request::get("/v1/hooks/inboxes/?limit=1")
        .header("host", "127.0.0.1:18800")
        .body(Body::empty())
        .unwrap();
    let (status, headers, body) = send_raw(&app, req).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        headers["location"],
        "http://127.0.0.1:18800/v1/hooks/inboxes?limit=1"
    );
    assert_eq!(body, "");
    assert!(!headers.contains_key("content-type"));
    assert_eq!(headers["x-frame-options"], "DENY");

    // Any method, any route, several slashes.
    let req = Request::post("/hooks/some-form//")
        .header("host", "h.example")
        .body(Body::empty())
        .unwrap();
    let (status, headers, _) = send_raw(&app, req).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(headers["location"], "http://h.example/hooks/some-form");

    // No route without the slash either: still 404.
    let (status, _, body) = send(&app, get("/nope/")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "Not Found");
}

#[tokio::test]
async fn unhandled_errors_skip_middleware_headers() {
    let app = app().await;
    let admin = |m: &str, uri: &str, body: &str| {
        Request::builder()
            .method(m)
            .uri(uri)
            .header("x-api-key", "test-admin")
            .header("content-type", "application/json")
            .header("origin", "https://site.example")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (status, _, _) = send(&app, admin("POST", "/v1/hooks/inboxes", r#"{"slug":"s"}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    // A non-UUID event id fails in the database in Python: a plain 500.
    let (status, headers, body) =
        send(&app, admin("GET", "/v1/hooks/s/events/not-a-uuid", "")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["message"], "Internal server error");
    // content-length may be added by the server, as uvicorn does.
    let names: Vec<&str> = headers
        .keys()
        .map(|k| k.as_str())
        .filter(|k| *k != "content-length")
        .collect();
    assert_eq!(names, ["content-type"]);
}

#[tokio::test]
async fn no_content_answers_carry_json_content_type() {
    let app = app().await;
    let admin = |m: &str, uri: &str, body: &str| {
        Request::builder()
            .method(m)
            .uri(uri)
            .header("x-api-key", "test-admin")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    send(
        &app,
        admin("POST", "/v1/hooks/inboxes", r#"{"slug":"gone"}"#),
    )
    .await;
    let (status, headers, body) =
        send_raw(&app, admin("DELETE", "/v1/hooks/inboxes/gone", "")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(body, "");
    let (status, headers, _) =
        send_raw(&app, admin("DELETE", "/v1/hooks/config/email-provider", "")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["content-type"], "application/json");
}
