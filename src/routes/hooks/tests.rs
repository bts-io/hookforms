//! HTTP tests for the receiver and the events routes, against the full router
//! on an in-memory database. Outgoing calls go to local mock servers.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Request as AxumRequest};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tower::ServiceExt;

use super::TURNSTILE_URL_OVERRIDE;
use crate::config::Config;
use crate::{AppState, db};

const KEY: &str = "test-admin";

async fn state() -> AppState {
    let config =
        Config::from_map([("ADMIN_API_KEY", KEY), ("DATABASE_URL", "sqlite::memory:")]).unwrap();
    let pool = db::init(&config.database_url).await.unwrap();
    // Mock servers are reached as `mock.test` (IP literals like 127.0.0.1 are
    // refused as SSRF targets, as in Python).
    let http = reqwest::Client::builder()
        .resolve("mock.test", "127.0.0.1:0".parse().unwrap())
        .build()
        .unwrap();
    AppState::new(pool, config, http)
}

async fn add_inbox(state: &AppState, slug: &str, extra: &[(&str, &str)]) -> String {
    let id = db::new_id();
    sqlx::query("INSERT INTO webhook_inboxes (id, slug) VALUES (?, ?)")
        .bind(&id)
        .bind(slug)
        .execute(&state.db)
        .await
        .unwrap();
    for (col, val) in extra {
        sqlx::query(&format!(
            "UPDATE webhook_inboxes SET {col} = ? WHERE id = ?"
        ))
        .bind(*val)
        .bind(&id)
        .execute(&state.db)
        .await
        .unwrap();
    }
    id
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn req(method: &str, uri: &str, content_type: Option<&str>, body: &str) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(ct) = content_type {
        b = b.header("content-type", ct);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

fn authed(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("x-api-key", KEY)
        .body(Body::empty())
        .unwrap()
}

/// What a mock server saw.
#[derive(Debug)]
struct Seen {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Bytes,
}

/// A local HTTP server that records every request and answers with `reply`.
async fn mock(status: StatusCode, reply: &'static str) -> (String, mpsc::UnboundedReceiver<Seen>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let app = Router::new().fallback(move |req: AxumRequest| {
        let tx = tx.clone();
        async move {
            let (parts, body) = req.into_parts();
            let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
            let _ = tx.send(Seen {
                method: parts.method,
                path: parts.uri.path().to_string(),
                headers: parts.headers,
                body,
            });
            (status, reply)
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://mock.test:{}", addr.port()), rx)
}

async fn next(rx: &mut mpsc::UnboundedReceiver<Seen>) -> Seen {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("mock got no request")
        .unwrap()
}

async fn stored_events(app: &Router, slug: &str) -> Value {
    let (status, body) = send(app, authed("GET", &format!("/v1/hooks/{slug}/events"))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

#[tokio::test]
async fn json_post_is_stored_and_listed() {
    let state = state().await;
    add_inbox(&state, "contact", &[]).await;
    let app = crate::build_app(state);

    let mut r = req(
        "POST",
        "/hooks/contact?ref=home&ref=nav&x=",
        Some("application/json"),
        r#"{"name":"Ann","email":"a@b.c","n":1}"#,
    );
    r.headers_mut().append("x-multi", "one".parse().unwrap());
    r.headers_mut().append("x-multi", "two".parse().unwrap());
    r.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "203.0.113.9:5000".parse().unwrap(),
    ));
    let (status, body) = send(&app, r).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "received");
    let event_id = body["event_id"].as_str().unwrap().to_string();
    assert_eq!(body.as_object().unwrap().len(), 2);

    let list = stored_events(&app, "contact").await;
    assert_eq!(list["meta"], json!({"total": 1, "limit": 50, "offset": 0}));
    let e = &list["data"][0];
    let keys: Vec<&str> = e.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "id",
            "method",
            "headers",
            "body",
            "query_params",
            "source_ip",
            "received_at"
        ]
    );
    assert_eq!(e["id"], event_id);
    assert_eq!(e["method"], "POST");
    assert_eq!(e["headers"]["content-type"], "application/json");
    assert_eq!(
        e["headers"]["x-multi"], "one",
        "first value of a repeated header"
    );
    assert_eq!(
        serde_json::to_string(&e["body"]).unwrap(),
        r#"{"name":"Ann","email":"a@b.c","n":1}"#
    );
    assert_eq!(e["query_params"], json!({"ref": "nav", "x": ""}));
    assert_eq!(e["source_ip"], "203.0.113.9");
    assert!(e["received_at"].as_str().unwrap().ends_with('Z'));

    let (status, one) = send(
        &app,
        authed("GET", &format!("/v1/hooks/contact/events/{event_id}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["data"], *e);
    // Any UUID spelling finds it, as Postgres would.
    let upper = event_id.replace('-', "").to_uppercase();
    let (status, _) = send(
        &app,
        authed("GET", &format!("/v1/hooks/contact/events/{upper}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn every_method_and_body_kind() {
    let state = state().await;
    add_inbox(&state, "f", &[]).await;
    let app = crate::build_app(state);
    for (method, ct, body) in [
        ("GET", None, ""),
        (
            "PUT",
            Some("application/x-www-form-urlencoded"),
            "name=Bo&msg=hi+there",
        ),
        ("PATCH", Some("text/plain"), "just text"),
        ("DELETE", Some("application/json"), "null"),
    ] {
        let (status, b) = send(&app, req(method, "/hooks/f", ct, body)).await;
        assert_eq!(status, StatusCode::OK, "{method}: {b}");
    }
    let list = stored_events(&app, "f").await;
    let bodies: Vec<(&str, &Value)> = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["method"].as_str().unwrap(), &e["body"]))
        .collect();
    assert_eq!(
        bodies,
        [
            ("DELETE", &Value::Null),
            ("PATCH", &json!({"raw": "just text"})),
            ("PUT", &json!({"name": "Bo", "msg": "hi there"})),
            ("GET", &Value::Null),
        ]
    );
}

#[tokio::test]
async fn unknown_inactive_and_head() {
    let state = state().await;
    add_inbox(&state, "off", &[("is_active", "0")]).await;
    let app = crate::build_app(state);
    let not_found = json!({"error": {"code": 404, "message": "Inbox not found"}});
    let (status, body) = send(&app, req("POST", "/hooks/nope", None, "{}")).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found.clone()));
    let (status, body) = send(&app, req("POST", "/hooks/off", None, "{}")).await;
    assert_eq!((status, body), (StatusCode::NOT_FOUND, not_found));
    let (status, _) = send(&app, req("HEAD", "/hooks/off", None, "")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, body) = send(&app, req("TRACE", "/hooks/off", None, "")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body, json!({"detail": "Method Not Allowed"}));
    // Events still list for an inactive inbox.
    let (status, _) = send(&app, authed("GET", "/v1/hooks/off/events")).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, authed("HEAD", "/v1/hooks/off/events")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn turnstile_checks() {
    let state = state().await;
    add_inbox(&state, "t", &[("turnstile_secret", "sekret")]).await;
    let app = crate::build_app(state);

    // Missing or empty token.
    for body in [
        r#"{"name":"A"}"#,
        r#"{"name":"A","cf-turnstile-response":""}"#,
    ] {
        let (status, b) = send(
            &app,
            req("POST", "/hooks/t", Some("application/json"), body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            b,
            json!({"error": {"code": 400, "message": "Missing Turnstile verification token"}})
        );
    }
    // An empty body skips the check, as in Python.
    let (status, _) = send(&app, req("POST", "/hooks/t", None, "")).await;
    assert_eq!(status, StatusCode::OK);

    let form = "name=A&cf-turnstile-response=tok&z=1";
    let ct = Some("application/x-www-form-urlencoded");

    let (url, mut rx) = mock(StatusCode::OK, r#"{"success": true}"#).await;
    let mut r = req("POST", "/hooks/t", ct, form);
    r.headers_mut()
        .insert("cf-connecting-ip", "198.51.100.7".parse().unwrap());
    let (status, _) = TURNSTILE_URL_OVERRIDE
        .scope(format!("{url}/verify"), send(&app, r))
        .await;
    assert_eq!(status, StatusCode::OK);
    let seen = next(&mut rx).await;
    assert_eq!(seen.method, Method::POST);
    assert_eq!(seen.path, "/verify");
    assert_eq!(
        seen.headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    assert_eq!(
        std::str::from_utf8(&seen.body).unwrap(),
        "secret=sekret&response=tok&remoteip=198.51.100.7"
    );

    let (url, _rx) = mock(StatusCode::OK, r#"{"success": false}"#).await;
    let (status, b) = TURNSTILE_URL_OVERRIDE
        .scope(url, send(&app, req("POST", "/hooks/t", ct, form)))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(b["error"]["message"], "Turnstile verification failed");

    let (url, _rx) = mock(StatusCode::BAD_GATEWAY, "<html>").await;
    let (status, b) = TURNSTILE_URL_OVERRIDE
        .scope(url, send(&app, req("POST", "/hooks/t", ct, form)))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(b["error"]["message"], "Internal server error");

    let (status, b) = TURNSTILE_URL_OVERRIDE
        .scope(
            "http://127.0.0.1:1/".to_string(),
            send(&app, req("POST", "/hooks/t", ct, form)),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(b["error"]["message"], "Turnstile verification unavailable");

    // The token is not stored; key order of the rest is kept.
    let list = stored_events(&app, "t").await;
    assert_eq!(list["meta"]["total"], 2);
    assert_eq!(
        serde_json::to_string(&list["data"][0]["body"]).unwrap(),
        r#"{"name":"A","z":"1"}"#
    );
}

#[tokio::test]
async fn legacy_forward_generic_and_discord() {
    let state = state().await;
    let (url, mut rx) = mock(StatusCode::OK, "ok").await;
    add_inbox(&state, "g", &[("forward_url", &format!("{url}/in"))]).await;
    add_inbox(
        &state,
        "d",
        &[(
            "forward_url",
            &format!("{url}/discord.com/api/webhooks/1/x"),
        )],
    )
    .await;
    let app = crate::build_app(state);

    let (status, _) = send(
        &app,
        req(
            "PUT",
            "/hooks/g",
            Some("application/json"),
            r#"{"b":"é","a":[1]}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seen = next(&mut rx).await;
    assert_eq!(seen.method, Method::PUT);
    assert_eq!(seen.path, "/in");
    assert_eq!(seen.headers["x-forwarded-from"], "hookforms/hooks/g");
    assert_eq!(seen.headers["content-type"], "application/json");
    assert_eq!(
        std::str::from_utf8(&seen.body).unwrap(),
        r#"{"b":"é","a":[1]}"#
    );

    let (status, _) = send(
        &app,
        req(
            "GET",
            "/hooks/d",
            Some("application/json"),
            r#"{"first_name":"Ann"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seen = next(&mut rx).await;
    assert_eq!(seen.method, Method::POST);
    let sent: Value = serde_json::from_slice(&seen.body).unwrap();
    assert_eq!(sent["embeds"][0]["title"], "[d] New Submission");
    assert_eq!(
        sent["embeds"][0]["fields"],
        json!([{"name": "First Name", "value": "Ann", "inline": true}])
    );
    assert!(
        sent["embeds"][0]["timestamp"]
            .as_str()
            .unwrap()
            .ends_with("+00:00")
    );

    // A falsy body is stored but not forwarded.
    let (status, _) = send(
        &app,
        req("POST", "/hooks/g", Some("application/json"), "{}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn channels_take_over_from_legacy_fields() {
    let state = state().await;
    let (url, mut rx) = mock(StatusCode::OK, "ok").await;
    let id = add_inbox(&state, "c", &[("forward_url", &url)]).await;
    sqlx::query(
        "INSERT INTO notification_channels (id, inbox_id, type, config) VALUES (?, ?, 'ntfy', '{}')",
    )
    .bind(db::new_id())
    .bind(&id)
    .execute(&state.db)
    .await
    .unwrap();
    let app = crate::build_app(state);
    let (status, _) = send(
        &app,
        req("POST", "/hooks/c", Some("application/json"), r#"{"a":1}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), rx.recv())
            .await
            .is_err(),
        "forward_url is ignored once the inbox has channels"
    );
}

#[tokio::test]
async fn legacy_email_rate_key() {
    let state = state().await;
    let id = add_inbox(&state, "e", &[("notify_email", "a@b.c")]).await;
    add_inbox(&state, "quiet", &[]).await;
    let app = crate::build_app(state.clone());
    for _ in 0..12 {
        let (status, _) = send(
            &app,
            req("POST", "/hooks/e", Some("application/json"), r#"{"a":1}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    // `email::notify` counts in its spawned task: wait for all 12.
    let key = format!("webhook_email_rate:{id}");
    for _ in 0..200 {
        if state.counters.get(&key) >= 12 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(state.counters.get(&key), 12);
    // Empty bodies do not count.
    let (status, _) = send(&app, req("POST", "/hooks/e", None, "")).await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(state.counters.get(&key), 12);
}

#[tokio::test]
async fn events_auth_pagination_and_validation() {
    let state = state().await;
    add_inbox(&state, "p", &[]).await;
    let app = crate::build_app(state);
    for i in 0..3 {
        send(
            &app,
            req(
                "POST",
                "/hooks/p",
                Some("application/json"),
                &format!(r#"{{"i":{i}}}"#),
            ),
        )
        .await;
    }

    let (status, body) = send(&app, req("GET", "/v1/hooks/p/events", None, "")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        json!({"error": {"code": 401, "message": "Missing API key"}})
    );

    let (status, body) = send(&app, authed("GET", "/v1/hooks/p/events?limit=2&offset=1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"], json!({"total": 3, "limit": 2, "offset": 1}));
    let is: Vec<&Value> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["body"]["i"])
        .collect();
    assert_eq!(is, [&json!(1), &json!(0)], "newest first");

    let (status, body) = send(&app, authed("GET", "/v1/hooks/p/events?limit=+1_0.0")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["limit"], 10);

    let (status, body) = send(&app, authed("GET", "/v1/hooks/p/events?limit=300&offset=x")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body,
        json!({"error": {"code": 422, "message": "Validation error", "details": [
            {"type": "less_than_equal", "loc": ["query", "limit"],
             "msg": "Input should be less than or equal to 200", "input": "300"},
            {"type": "int_parsing", "loc": ["query", "offset"],
             "msg": "Input should be a valid integer, unable to parse string as an integer",
             "input": "x"},
        ]}})
    );

    // Validation runs before the inbox lookup, as in FastAPI.
    let (status, _) = send(&app, authed("GET", "/v1/hooks/none/events?limit=a")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = send(&app, authed("GET", "/v1/hooks/none/events")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["message"], "Inbox not found");

    let (status, _) = send(&app, authed("GET", "/v1/hooks/p/events?offset=-1")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let missing = db::new_id();
    let (status, body) = send(
        &app,
        authed("GET", &format!("/v1/hooks/p/events/{missing}")),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body,
        json!({"error": {"code": 404, "message": "Event not found"}})
    );
    let (status, _) = send(&app, authed("GET", "/v1/hooks/p/events/not-a-uuid")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn inboxes_events_path_matches_python() {
    let state = state().await;
    let app = crate::build_app(state.clone());
    let (status, body) = send(&app, authed("GET", "/v1/hooks/inboxes/events")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["message"], "Inbox not found");

    add_inbox(&state, "inboxes", &[]).await;
    send(
        &app,
        req(
            "POST",
            "/hooks/inboxes",
            Some("application/json"),
            r#"{"a":1}"#,
        ),
    )
    .await;
    let (status, body) = send(&app, authed("GET", "/v1/hooks/inboxes/events")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["meta"]["total"], 1);
    let id = body["data"][0]["id"].as_str().unwrap().to_string();
    let (status, _) = send(
        &app,
        authed("GET", &format!("/v1/hooks/inboxes/events/{id}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Other methods reach the inbox routes (an inbox named "events") or get 405.
    let (status, body) = send(&app, authed("POST", "/v1/hooks/inboxes/events")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(body, json!({"detail": "Method Not Allowed"}));
    let (status, _) = send(&app, authed("DELETE", "/v1/hooks/inboxes/events")).await;
    assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED);
    let (status, _) = send(&app, authed("HEAD", "/v1/hooks/inboxes/events")).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
}
