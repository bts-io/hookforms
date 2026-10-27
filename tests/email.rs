//! Step 5: `/v1/hooks/config/email-provider`, the email providers against
//! local mock servers (HTTP and SMTP), and the notify paths. No network.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, Uri};
use axum::routing::post;
use hookforms::config::Config;
use hookforms::email::providers::{ApiProvider, GmailProvider, Provider, ReplyTo};
use hookforms::email::{self, InboxInfo};
use hookforms::{AppState, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tower::ServiceExt;

const ADMIN: &str = "test-admin";

fn temp_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("hookforms-email-{}-{name}", uuid::Uuid::new_v4()))
}

async fn state_with(extra: &[(&str, &str)]) -> AppState {
    let mut pairs = vec![
        ("ADMIN_API_KEY".to_string(), ADMIN.to_string()),
        ("DATABASE_URL".to_string(), "sqlite::memory:".to_string()),
        (
            "GMAIL_TOKEN_PATH".to_string(),
            temp_path("missing.json").display().to_string(),
        ),
    ];
    for (k, v) in extra {
        pairs.push((k.to_string(), v.to_string()));
    }
    let config = Config::from_map(pairs).unwrap();
    let pool = db::init(&config.database_url).await.unwrap();
    AppState::new(pool, config, reqwest::Client::new())
}

async fn add_inbox(pool: &SqlitePool, slug: &str, notify: Option<&str>) -> String {
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO webhook_inboxes (id, slug, notify_email, sender_name) VALUES (?, ?, ?, 'Acme')",
    )
    .bind(&id)
    .bind(slug)
    .bind(notify)
    .execute(pool)
    .await
    .unwrap();
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

fn req(method: &str, uri: &str, body: Option<Value>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-api-key", ADMIN);
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

const PATH: &str = "/v1/hooks/config/email-provider";

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn needs_api_key() {
    let state = state_with(&[]).await;
    let app = hookforms::build_app(state);
    let r = Request::get(PATH).body(Body::empty()).unwrap();
    let (status, body) = send(&app, r).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        json!({"error": {"code": 401, "message": "Missing API key"}})
    );
}

#[tokio::test]
async fn global_provider_crud() {
    let state = state_with(&[]).await;
    let pool = state.db.clone();
    let app = hookforms::build_app(state);

    let (status, body) = send(&app, req("GET", PATH, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"data": null, "meta": {"fallback": null}}));

    let cfg = json!({"api_key": "re_secret_key_123", "from_email": "noreply@example.com"});
    let (status, body) = send(
        &app,
        req("PUT", PATH, Some(json!({"type": "resend", "config": cfg}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["type"], "resend");
    assert_eq!(data["inbox_id"], Value::Null);
    assert_eq!(data["is_active"], true);
    assert_eq!(data["config"], cfg, "PUT answers unredacted, like Python");
    assert!(data["created_at"].as_str().unwrap().ends_with('Z'));
    let keys: Vec<&str> = data
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "id",
            "inbox_id",
            "type",
            "config",
            "is_active",
            "created_at"
        ]
    );

    let (status, body) = send(&app, req("GET", PATH, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["config"]["api_key"], "re_s***_123");
    assert_eq!(body["data"]["config"]["from_email"], "noreply@example.com");
    assert!(body.get("meta").is_none());

    // A second global PUT replaces the NULL-scoped row instead of adding one.
    let (status, _) = send(
        &app,
        req(
            "PUT",
            PATH,
            Some(json!({"inbox": "", "type": "sendgrid", "config": {"api_key": "SG.x", "from_email": "a@b.c"}})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM email_providers WHERE inbox_id IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
    let (_, body) = send(&app, req("GET", PATH, None)).await;
    assert_eq!(body["data"]["type"], "sendgrid");

    let (status, body) = send(&app, req("DELETE", PATH, None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(body, Value::Null);
    let (_, body) = send(&app, req("GET", PATH, None)).await;
    assert_eq!(body["data"], Value::Null);
}

#[tokio::test]
async fn inbox_scoped_provider() {
    let state = state_with(&[]).await;
    let pool = state.db.clone();
    let inbox_id = add_inbox(&pool, "contact", None).await;
    let app = hookforms::build_app(state);

    let smtp = json!({"host": "smtp.example.com", "port": 587, "username": "u", "password": "supersecretpw", "from_email": "a@b.c"});
    let (status, body) = send(
        &app,
        req(
            "PUT",
            PATH,
            Some(json!({"inbox": "contact", "type": "smtp", "config": smtp})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["inbox_id"], inbox_id.as_str());

    // The global scope stays empty; an inbox without its own row does not fall back.
    let (_, body) = send(&app, req("GET", PATH, None)).await;
    assert_eq!(body["data"], Value::Null);
    let (_, body) = send(&app, req("GET", &format!("{PATH}?inbox=contact"), None)).await;
    assert_eq!(body["data"]["type"], "smtp");
    assert_eq!(body["data"]["config"]["password"], "supe***etpw");
    assert_eq!(body["data"]["config"]["port"], 587);

    for (method, body) in [("GET", None), ("DELETE", None)] {
        let (status, b) = send(&app, req(method, &format!("{PATH}?inbox=nope"), body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            b,
            json!({"error": {"code": 404, "message": "Inbox not found"}})
        );
    }
    let (status, b) = send(
        &app,
        req(
            "PUT",
            PATH,
            Some(json!({"inbox": "nope", "type": "smtp", "config": smtp})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(b["error"]["message"], "Inbox not found");

    let (status, _) = send(&app, req("DELETE", &format!("{PATH}?inbox=contact"), None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = send(&app, req("GET", &format!("{PATH}?inbox=contact"), None)).await;
    assert_eq!(body, json!({"data": null, "meta": {"fallback": null}}));
}

#[tokio::test]
async fn put_errors_match_python() {
    let state = state_with(&[]).await;
    let app = hookforms::build_app(state);

    let (status, body) = send(
        &app,
        req("PUT", PATH, Some(json!({"type": "mailgun", "config": {}}))),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body,
        json!({"error": {"code": 400, "message": "Invalid provider type: mailgun"}})
    );

    let (status, body) = send(
        &app,
        req(
            "PUT",
            PATH,
            Some(json!({"type": "smtp", "config": {"host": "h", "from_email": "a@b.c"}})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "Missing required field: port");

    let (status, body) = send(
        &app,
        req("PUT", PATH, Some(json!({"inbox": 5, "config": []}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body,
        json!({"error": {"code": 422, "message": "Validation error", "details": [
            {"type": "string_type", "loc": ["body", "inbox"], "msg": "Input should be a valid string", "input": 5},
            {"type": "missing", "loc": ["body", "type"], "msg": "Field required", "input": {"inbox": 5, "config": []}},
            {"type": "dict_type", "loc": ["body", "config"], "msg": "Input should be a valid dictionary", "input": []}
        ]}})
    );

    let (status, body) = send(&app, req("PUT", PATH, None)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"]["details"],
        json!([{"type": "missing", "loc": ["body"], "msg": "Field required", "input": null}])
    );

    let bad = Request::put(PATH)
        .header("x-api-key", ADMIN)
        .header("content-type", "application/json")
        .body(Body::from("{\"type\": }"))
        .unwrap();
    let (status, body) = send(&app, bad).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"]["details"],
        json!([{"type": "json_invalid", "loc": ["body", 9], "msg": "JSON decode error", "input": {}}])
    );

    let (status, body) = send(&app, req("PUT", PATH, Some(json!(["x"])))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["details"][0]["type"], "model_attributes_type");
}

#[tokio::test]
async fn env_gmail_fallback_is_reported() {
    let token = temp_path("token.json");
    std::fs::write(&token, "{}").unwrap();
    let state = state_with(&[
        ("GMAIL_TOKEN_PATH", token.to_str().unwrap()),
        ("GMAIL_SENDER_EMAIL", "me@example.com"),
    ])
    .await;
    let app = hookforms::build_app(state);
    let (_, body) = send(&app, req("GET", PATH, None)).await;
    assert_eq!(
        body,
        json!({"data": null, "meta": {"fallback": "env_gmail"}})
    );
    std::fs::remove_file(token).ok();
}

// ---------------------------------------------------------------------------
// Mock HTTP server for Resend, SendGrid, Gmail and the Google token endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    auth: String,
    content_type: String,
    body: String,
}

#[derive(Clone, Default)]
struct Mock {
    seen: Arc<Mutex<Vec<Seen>>>,
    /// Status codes to answer on the send endpoint, in order (then 200).
    send_status: Arc<Mutex<Vec<u16>>>,
}

async fn mock_handler(
    State(m): State<Mock>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    m.seen.lock().unwrap().push(Seen {
        path: uri.path().to_string(),
        auth: h("authorization"),
        content_type: h("content-type"),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    if uri.path() == "/token" {
        return (
            StatusCode::OK,
            json!({"access_token": "fresh-token", "expires_in": 3599, "token_type": "Bearer"})
                .to_string(),
        );
    }
    let code = {
        let mut s = m.send_status.lock().unwrap();
        if s.is_empty() { 200 } else { s.remove(0) }
    };
    (
        StatusCode::from_u16(code).unwrap(),
        json!({"id": "msg-1", "message": "nope"}).to_string(),
    )
}

async fn mock_server() -> (String, Mock) {
    let mock = Mock::default();
    let app = Router::new()
        .route("/{*path}", post(mock_handler))
        .with_state(mock.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), mock)
}

#[tokio::test]
async fn resend_request_shape() {
    let (base, mock) = mock_server().await;
    let p = Provider::Resend(ApiProvider {
        api_key: "re_key".into(),
        from_email: "noreply@example.com".into(),
        api_base: base,
    });
    p.send(
        &reqwest::Client::new(),
        "to@example.com",
        "Subj",
        "<b>x</b>",
        "x",
        Some("Acme"),
        None,
    )
    .await
    .unwrap();
    let seen = mock.seen.lock().unwrap().clone();
    assert_eq!(seen[0].path, "/emails");
    assert_eq!(seen[0].auth, "Bearer re_key");
    assert_eq!(seen[0].content_type, "application/json");
    let body: Value = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(
        body,
        json!({"from": "Acme <noreply@example.com>", "to": ["to@example.com"], "subject": "Subj",
               "html": "<b>x</b>", "text": "x"})
    );

    mock.send_status.lock().unwrap().push(422);
    let err = p
        .send(
            &reqwest::Client::new(),
            "to@example.com",
            "S",
            "h",
            "t",
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.starts_with("Resend send failed: 422 "), "{err}");
    let body: Value = serde_json::from_str(&mock.seen.lock().unwrap()[1].body).unwrap();
    assert_eq!(body["from"], "HookForms <noreply@example.com>");
}

#[tokio::test]
async fn sendgrid_request_shape() {
    let (base, mock) = mock_server().await;
    let p = Provider::SendGrid(ApiProvider {
        api_key: "SG.key".into(),
        from_email: "noreply@example.com".into(),
        api_base: base,
    });
    p.send(
        &reqwest::Client::new(),
        "to@example.com",
        "Subj",
        "<p>h</p>",
        "h",
        None,
        None,
    )
    .await
    .unwrap();
    let seen = mock.seen.lock().unwrap().clone();
    assert_eq!(seen[0].path, "/v3/mail/send");
    assert_eq!(seen[0].auth, "Bearer SG.key");
    let body: Value = serde_json::from_str(&seen[0].body).unwrap();
    assert_eq!(
        body,
        json!({
            "personalizations": [{"to": [{"email": "to@example.com"}]}],
            "from": {"email": "noreply@example.com", "name": "HookForms"},
            "subject": "Subj",
            "content": [
                {"type": "text/plain", "value": "h"},
                {"type": "text/html", "value": "<p>h</p>"}
            ]
        })
    );
}

#[tokio::test]
async fn api_providers_strip_control_characters_from_header_fields() {
    let evil = "Eve\r\nBcc: victim@example.com\r\nX-Injected: 1";
    let has_control = |v: &Value| v.as_str().unwrap().chars().any(|c| c.is_control());

    let (base, mock) = mock_server().await;
    let resend = Provider::Resend(ApiProvider {
        api_key: "re_key".into(),
        from_email: "noreply@example.com".into(),
        api_base: base.clone(),
    });
    resend
        .send(
            &reqwest::Client::new(),
            "to@example.com\r\nBcc: x@example.com",
            &format!("[x] from {evil}"),
            "<b>x</b>",
            "x",
            Some(evil),
            None,
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&mock.seen.lock().unwrap()[0].body).unwrap();
    for v in [&body["from"], &body["subject"], &body["to"][0]] {
        assert!(!has_control(v), "{v}");
    }
    assert_eq!(body["to"][0], "to@example.com");

    let sendgrid = Provider::SendGrid(ApiProvider {
        api_key: "SG.key".into(),
        from_email: "noreply@example.com".into(),
        api_base: base,
    });
    sendgrid
        .send(
            &reqwest::Client::new(),
            "to@example.com\r\nBcc: x@example.com",
            &format!("[x] from {evil}"),
            "<p>h</p>",
            "h",
            Some(evil),
            None,
        )
        .await
        .unwrap();
    let body: Value = serde_json::from_str(&mock.seen.lock().unwrap()[1].body).unwrap();
    for v in [
        &body["subject"],
        &body["from"]["name"],
        &body["personalizations"][0]["to"][0]["email"],
    ] {
        assert!(!has_control(v), "{v}");
    }
}

fn decode_raw(body: &str) -> String {
    use base64::Engine;
    let v: Value = serde_json::from_str(body).unwrap();
    let raw = v["raw"].as_str().unwrap();
    String::from_utf8(
        base64::engine::general_purpose::URL_SAFE
            .decode(raw)
            .unwrap(),
    )
    .unwrap()
}

/// The parts of a `multipart/alternative` message as (content type, decoded
/// body), in order. Handles 7bit, quoted-printable and base64 parts.
fn mime_parts(message: &str) -> Vec<(String, String)> {
    use base64::Engine;
    let message = message.replace("\r\n", "\n");
    assert!(
        message.contains("Content-Type: multipart/alternative"),
        "{message}"
    );
    let boundary = message
        .split("boundary=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("boundary");
    let delimiter = format!("--{boundary}");
    let mut out = Vec::new();
    for chunk in message.split(&delimiter).skip(1) {
        if chunk.starts_with("--") {
            break;
        }
        let (head, body) = chunk.trim_start_matches('\n').split_once("\n\n").unwrap();
        let header = |name: &str| {
            head.lines()
                .find_map(|l| l.strip_prefix(name))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        };
        let ctype = header("Content-Type:");
        let ctype = ctype.split(';').next().unwrap().to_string();
        let body = body.strip_suffix('\n').unwrap_or(body);
        let decoded = match header("Content-Transfer-Encoding:").as_str() {
            "base64" => {
                let joined: String = body.split_whitespace().collect();
                String::from_utf8(
                    base64::engine::general_purpose::STANDARD
                        .decode(joined)
                        .unwrap(),
                )
                .unwrap()
            }
            "quoted-printable" => {
                let joined = body.replace("=\n", "").replace('\n', "\r\n");
                let bytes = joined.as_bytes();
                let mut buf = Vec::new();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'=' && i + 2 < bytes.len() {
                        let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                        buf.push(u8::from_str_radix(hex, 16).unwrap());
                        i += 3;
                    } else {
                        buf.push(bytes[i]);
                        i += 1;
                    }
                }
                String::from_utf8(buf).unwrap()
            }
            _ => body.to_string(),
        };
        out.push((ctype, decoded));
    }
    out
}

#[tokio::test]
async fn gmail_refreshes_expired_token_and_sends() {
    let (base, mock) = mock_server().await;
    let token = temp_path("token.json");
    std::fs::write(
        &token,
        json!({
            "token": "old-token",
            "refresh_token": "refresh-1",
            "token_uri": format!("{base}/token"),
            "client_id": "cid",
            "client_secret": "csecret",
            "scopes": ["https://www.googleapis.com/auth/gmail.send"],
            "expiry": "2020-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
    let mut g = GmailProvider::new(&token, "me@example.com");
    g.api_base = base;
    let p = Provider::Gmail(g);
    p.send(
        &reqwest::Client::new(),
        "to@example.com",
        "Hello",
        "<p>Hi</p>",
        "Hi there",
        Some("Acme"),
        None,
    )
    .await
    .unwrap();

    let seen = mock.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].path, "/token");
    assert_eq!(seen[0].content_type, "application/x-www-form-urlencoded");
    for part in [
        "grant_type=refresh_token",
        "client_id=cid",
        "client_secret=csecret",
        "refresh_token=refresh-1",
    ] {
        assert!(seen[0].body.contains(part), "{part}");
    }
    assert_eq!(seen[1].path, "/gmail/v1/users/me/messages/send");
    assert_eq!(seen[1].auth, "Bearer fresh-token");
    let mime = decode_raw(&seen[1].body);
    assert!(mime.contains("From: Acme <me@example.com>"), "{mime}");
    assert!(mime.contains("To: to@example.com"));
    assert!(mime.contains("Subject: Hello"));
    assert_eq!(
        mime_parts(&mime),
        [
            ("text/plain".to_string(), "Hi there".to_string()),
            ("text/html".to_string(), "<p>Hi</p>".to_string())
        ]
    );

    // The refreshed token is written back, other fields kept.
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&token).unwrap()).unwrap();
    assert_eq!(saved["token"], "fresh-token");
    assert_eq!(saved["refresh_token"], "refresh-1");
    assert_eq!(saved["client_id"], "cid");
    assert!(saved["expiry"].as_str().unwrap() > "2026");

    // A valid token is used as is; a 401 triggers one refresh and a retry.
    mock.send_status.lock().unwrap().push(401);
    p.send(
        &reqwest::Client::new(),
        "to@example.com",
        "Again",
        "x",
        "x",
        None,
        None,
    )
    .await
    .unwrap();
    let paths: Vec<String> = mock.seen.lock().unwrap()[2..]
        .iter()
        .map(|s| s.path.clone())
        .collect();
    assert_eq!(
        paths,
        [
            "/gmail/v1/users/me/messages/send",
            "/token",
            "/gmail/v1/users/me/messages/send"
        ]
    );
    std::fs::remove_file(token).ok();
}

#[tokio::test]
async fn gmail_missing_token_file() {
    let p = Provider::Gmail(GmailProvider::new(
        "/nonexistent/token.json",
        "me@example.com",
    ));
    let err = p
        .send(
            &reqwest::Client::new(),
            "to@example.com",
            "S",
            "h",
            "t",
            None,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        err,
        "Gmail token not found at /nonexistent/token.json. Run 'python scripts/gmail_auth.py' to authorize."
    );
}

// ---------------------------------------------------------------------------
// Local SMTP test server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct Mail {
    auth: Option<String>,
    from: String,
    rcpt: Vec<String>,
    data: String,
}

async fn smtp_server(starttls: bool) -> (u16, Arc<Mutex<Vec<Mail>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mails: Arc<Mutex<Vec<Mail>>> = Arc::default();
    let store = mails.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let store = store.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut lines = BufReader::new(r).lines();
                w.write_all(b"220 test ESMTP\r\n").await.unwrap();
                let mut mail = Mail::default();
                while let Ok(Some(line)) = lines.next_line().await {
                    let upper = line.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") {
                        if starttls {
                            b"250-test\r\n250-STARTTLS\r\n250 AUTH PLAIN LOGIN\r\n"
                        } else {
                            b"250-test\r\n250 AUTH PLAIN LOGIN\r\n"
                        }
                    } else if upper.starts_with("AUTH") {
                        mail.auth = Some(line.clone());
                        b"235 ok\r\n"
                    } else if upper.starts_with("MAIL FROM") {
                        mail.from = line.clone();
                        b"250 ok\r\n"
                    } else if upper.starts_with("RCPT TO") {
                        mail.rcpt.push(line.clone());
                        b"250 ok\r\n"
                    } else if upper == "DATA" {
                        w.write_all(b"354 go\r\n").await.unwrap();
                        while let Ok(Some(l)) = lines.next_line().await {
                            if l == "." {
                                break;
                            }
                            // Undo SMTP dot-stuffing, as a real server does.
                            mail.data.push_str(l.strip_prefix('.').unwrap_or(&l));
                            mail.data.push('\n');
                        }
                        store.lock().unwrap().push(std::mem::take(&mut mail));
                        b"250 queued\r\n"
                    } else if upper == "QUIT" {
                        w.write_all(b"221 bye\r\n").await.ok();
                        break;
                    } else {
                        b"502 no\r\n"
                    };
                    w.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, mails)
}

async fn put_smtp(pool: &SqlitePool, inbox_id: Option<&str>, port: u16) {
    sqlx::query(
        "INSERT INTO email_providers (id, inbox_id, type, config) VALUES (?, ?, 'smtp', ?)",
    )
    .bind(db::new_id())
    .bind(inbox_id)
    .bind(
        json!({"host": "127.0.0.1", "port": port, "username": "user", "password": "pw",
                   "use_tls": false, "from_email": "forms@example.com"})
        .to_string(),
    )
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn smtp_plain_with_login() {
    let (port, mails) = smtp_server(false).await;
    let p = Provider::from_record(
        "smtp",
        json!({"host": "127.0.0.1", "port": port, "username": "user", "password": "pw",
               "use_tls": false, "from_email": "forms@example.com"})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    p.send(
        &reqwest::Client::new(),
        "to@example.com",
        "Hi there",
        "<p>Body</p>",
        "Body",
        Some("Acme"),
        None,
    )
    .await
    .unwrap();
    let m = mails.lock().unwrap()[0].clone();
    assert!(m.auth.unwrap().starts_with("AUTH PLAIN"));
    assert!(m.from.contains("<forms@example.com>"));
    assert_eq!(m.rcpt, ["RCPT TO:<to@example.com>"]);
    assert!(
        m.data.contains("From: Acme <forms@example.com>"),
        "{}",
        m.data
    );
    assert!(m.data.contains("Subject: Hi there"));
    assert_eq!(
        mime_parts(&m.data),
        [
            ("text/plain".to_string(), "Body".to_string()),
            ("text/html".to_string(), "<p>Body</p>".to_string())
        ]
    );
}

/// Non-ASCII parts arrive intact, each decoded from its own part.
#[tokio::test]
async fn smtp_non_ascii_parts_round_trip() {
    let (port, mails) = smtp_server(false).await;
    let p = Provider::from_record(
        "smtp",
        json!({"host": "127.0.0.1", "port": port, "use_tls": false,
               "from_email": "forms@example.com"})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    let html = "<p>Zo\u{eb} \u{65e5}\u{672c}</p>\n<p>h\u{e9}llo</p>";
    let text = "Zo\u{eb} \u{65e5}\u{672c}\nh\u{e9}llo";
    p.send(
        &reqwest::Client::new(),
        "to@example.com",
        "S",
        html,
        text,
        None,
        None,
    )
    .await
    .unwrap();
    let m = mails.lock().unwrap()[0].clone();
    assert_eq!(
        mime_parts(&m.data),
        [
            ("text/plain".to_string(), text.to_string()),
            ("text/html".to_string(), html.to_string())
        ]
    );
}

/// A full rendered notification goes out with both parts, long lines and all.
#[tokio::test]
async fn smtp_sends_rendered_notification_as_alternative() {
    let (port, mails) = smtp_server(false).await;
    let p = Provider::from_record(
        "smtp",
        json!({"host": "127.0.0.1", "port": port, "use_tls": false,
               "from_email": "forms@example.com"})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    let body = json!({"name": "Sam", "email": "sam@example.com", "message": "L".repeat(1500)});
    let r = email::render::render("contact", "[contact] from Sam", body.as_object().unwrap());
    p.send(
        &reqwest::Client::new(),
        "to@example.com",
        &r.subject,
        &r.html,
        &r.text,
        None,
        None,
    )
    .await
    .unwrap();
    let m = mails.lock().unwrap()[0].clone();
    assert!(m.data.contains("multipart/alternative"), "{}", m.data);
    let parts = mime_parts(&m.data);
    assert_eq!(parts.len(), 2);
    assert_eq!(
        parts[0],
        ("text/plain".to_string(), r.text.replace('\n', "\r\n"))
    );
    assert_eq!(parts[1].0, "text/html");
    assert_eq!(parts[1].1.replace("\r\n", "\n"), r.html);
}

#[tokio::test]
async fn smtp_tls_refuses_server_without_starttls() {
    let (port, mails) = smtp_server(false).await;
    let p = Provider::from_record(
        "smtp",
        json!({"host": "127.0.0.1", "port": port, "from_email": "forms@example.com"})
            .as_object()
            .unwrap(),
    )
    .unwrap();
    let err = p
        .send(
            &reqwest::Client::new(),
            "to@example.com",
            "S",
            "h",
            "t",
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err.starts_with("SMTP send failed"), "{err}");
    assert!(mails.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// notify paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_notify_sends_rate_limits_and_skips_channel_inboxes() {
    let (port, mails) = smtp_server(false).await;
    let state = state_with(&[]).await;
    let inbox_id = add_inbox(&state.db, "contact", Some("a@example.com, b@example.com")).await;
    put_smtp(&state.db, None, port).await;
    let info = InboxInfo {
        id: inbox_id.clone(),
        slug: "contact".into(),
        notify_email: Some("a@example.com, b@example.com".into()),
        email_subject_prefix: None,
        sender_name: Some("Acme".into()),
    };
    let body = json!({"name": "Ann", "email": "ann@example.com", "message": "Hello"});

    assert_eq!(email::deliver_legacy(&state, &info, &body).await, 2);
    {
        let m = mails.lock().unwrap();
        assert_eq!(m.len(), 2);
        assert!(m[0].data.contains("Subject: [contact] from Ann"));
        assert!(m[0].data.contains("From: Acme <forms@example.com>"));
    }
    // Empty bodies send nothing and do not count.
    assert_eq!(email::deliver_legacy(&state, &info, &json!({})).await, 0);

    // 10 per inbox per 10 minutes.
    for _ in 0..9 {
        assert_eq!(email::deliver_legacy(&state, &info, &body).await, 2);
    }
    assert_eq!(email::deliver_legacy(&state, &info, &body).await, 0);
    assert_eq!(
        state
            .counters
            .get(&format!("webhook_email_rate:{inbox_id}")),
        11
    );

    // An inbox with active channels leaves email to the dispatcher.
    let other = add_inbox(&state.db, "other", Some("c@example.com")).await;
    sqlx::query("INSERT INTO notification_channels (id, inbox_id, type, config) VALUES (?, ?, 'email', '{}')")
        .bind(db::new_id())
        .bind(&other)
        .execute(&state.db)
        .await
        .unwrap();
    let info2 = InboxInfo {
        id: other,
        slug: "other".into(),
        notify_email: Some("c@example.com".into()),
        ..Default::default()
    };
    assert_eq!(email::deliver_legacy(&state, &info2, &body).await, 0);
}

// ---------------------------------------------------------------------------
// Reply-To: replying to a notification answers the submitter
// ---------------------------------------------------------------------------

fn ada() -> ReplyTo {
    ReplyTo {
        address: "ada@example.com".into(),
        name: Some("Ada Lovelace".into()),
    }
}

/// A Reply-To whose address and name try to add a header.
fn dirty() -> ReplyTo {
    ReplyTo {
        address: "ada@example.com\r\nBcc: victim@example.com".into(),
        name: Some("Ada\r\nBcc: victim@example.com".into()),
    }
}

#[tokio::test]
async fn api_providers_send_reply_to() {
    let (base, mock) = mock_server().await;
    let api = |base: &str| ApiProvider {
        api_key: "k".into(),
        from_email: "noreply@example.com".into(),
        api_base: base.to_string(),
    };
    let http = reqwest::Client::new();
    let resend = Provider::Resend(api(&base));
    let sendgrid = Provider::SendGrid(api(&base));
    let comma = ReplyTo {
        address: "ada@example.com".into(),
        name: Some("Lovelace, Ada".into()),
    };
    for (p, reply) in [
        (&resend, &ada()),
        (&resend, &dirty()),
        (&resend, &comma),
        (&sendgrid, &ada()),
        (&sendgrid, &dirty()),
    ] {
        p.send(&http, "to@example.com", "S", "h", "t", None, Some(reply))
            .await
            .unwrap();
    }
    let seen = mock.seen.lock().unwrap().clone();
    let body = |i: usize| serde_json::from_str::<Value>(&seen[i].body).unwrap();
    assert_eq!(body(0)["reply_to"], "Ada Lovelace <ada@example.com>");
    assert_eq!(
        body(1)["reply_to"],
        "\"Ada Bcc: victim@example.com\" <ada@example.com>"
    );
    assert_eq!(body(2)["reply_to"], "\"Lovelace, Ada\" <ada@example.com>");
    assert_eq!(
        body(3)["reply_to"],
        json!({"email": "ada@example.com", "name": "Ada Lovelace"})
    );
    assert_eq!(
        body(4)["reply_to"],
        json!({"email": "ada@example.com", "name": "Ada Bcc: victim@example.com"})
    );

    // An address-only Reply-To has no name; none at all leaves the field out.
    let bare = ReplyTo {
        address: "ada@example.com".into(),
        name: None,
    };
    resend
        .send(&http, "to@example.com", "S", "h", "t", None, Some(&bare))
        .await
        .unwrap();
    sendgrid
        .send(&http, "to@example.com", "S", "h", "t", None, Some(&bare))
        .await
        .unwrap();
    resend
        .send(&http, "to@example.com", "S", "h", "t", None, None)
        .await
        .unwrap();
    sendgrid
        .send(&http, "to@example.com", "S", "h", "t", None, None)
        .await
        .unwrap();
    let seen = mock.seen.lock().unwrap().clone();
    let body = |i: usize| serde_json::from_str::<Value>(&seen[i].body).unwrap();
    assert_eq!(body(5)["reply_to"], "ada@example.com");
    assert_eq!(body(6)["reply_to"], json!({"email": "ada@example.com"}));
    assert!(body(7).get("reply_to").is_none());
    assert!(body(8).get("reply_to").is_none());
}

/// The header block of a raw message, with folded lines joined.
fn header_block(message: &str) -> String {
    let message = message.replace("\r\n", "\n");
    let head = message.split("\n\n").next().unwrap_or_default();
    head.replace("\n ", " ").replace("\n\t", " ")
}

#[tokio::test]
async fn smtp_sends_reply_to() {
    let (port, mails) = smtp_server(false).await;
    let p = Provider::from_record(
        "smtp",
        json!({"host": "127.0.0.1", "port": port, "use_tls": false,
               "from_email": "forms@example.com"})
        .as_object()
        .unwrap(),
    )
    .unwrap();
    let http = reqwest::Client::new();
    for reply in [Some(ada()), Some(dirty()), None] {
        p.send(&http, "to@example.com", "S", "h", "t", None, reply.as_ref())
            .await
            .unwrap();
    }
    let m = mails.lock().unwrap().clone();
    let named = header_block(&m[0].data);
    assert!(
        named.contains("Reply-To: \"Ada Lovelace\" <ada@example.com>")
            || named.contains("Reply-To: Ada Lovelace <ada@example.com>"),
        "{named}"
    );
    let cleaned = header_block(&m[1].data);
    assert!(cleaned.contains("Reply-To: "), "{cleaned}");
    assert!(cleaned.contains("<ada@example.com>"), "{cleaned}");
    assert!(!cleaned.lines().any(|l| l.starts_with("Bcc")), "{cleaned}");
    assert!(!header_block(&m[2].data).contains("Reply-To"));
    // Never the sender.
    assert!(
        named.contains("From: HookForms <forms@example.com>"),
        "{named}"
    );
}

#[tokio::test]
async fn gmail_sends_reply_to() {
    let (base, mock) = mock_server().await;
    let token = temp_path("token.json");
    std::fs::write(&token, json!({"token": "t"}).to_string()).unwrap();
    let mut g = GmailProvider::new(&token, "me@example.com");
    g.api_base = base;
    let p = Provider::Gmail(g);
    let http = reqwest::Client::new();
    p.send(&http, "to@example.com", "S", "h", "t", None, Some(&ada()))
        .await
        .unwrap();
    p.send(&http, "to@example.com", "S", "h", "t", None, None)
        .await
        .unwrap();
    let seen = mock.seen.lock().unwrap().clone();
    let named = header_block(&decode_raw(&seen[0].body));
    assert!(named.contains("<ada@example.com>"), "{named}");
    assert!(named.contains("Reply-To: "), "{named}");
    assert!(named.contains("Ada Lovelace"), "{named}");
    assert!(!header_block(&decode_raw(&seen[1].body)).contains("Reply-To"));
    std::fs::remove_file(token).ok();
}

#[tokio::test]
async fn legacy_notify_replies_to_the_submitter() {
    let (port, mails) = smtp_server(false).await;
    let state = state_with(&[]).await;
    let inbox_id = add_inbox(&state.db, "contact", Some("a@example.com")).await;
    put_smtp(&state.db, None, port).await;
    let info = InboxInfo {
        id: inbox_id,
        slug: "contact".into(),
        notify_email: Some("a@example.com".into()),
        email_subject_prefix: None,
        sender_name: Some("Acme".into()),
    };
    let with = json!({"name": "Ann", "email": "ann@example.com", "message": "Hello"});
    let without = json!({"name": "Ann", "email": "not an address", "message": "Hello"});
    assert_eq!(email::deliver_legacy(&state, &info, &with).await, 1);
    assert_eq!(email::deliver_legacy(&state, &info, &without).await, 1);
    let m = mails.lock().unwrap().clone();
    let first = header_block(&m[0].data);
    assert!(first.contains("Reply-To: Ann <ann@example.com>"), "{first}");
    assert!(first.contains("From: Acme <forms@example.com>"), "{first}");
    assert!(!header_block(&m[1].data).contains("Reply-To"));
}

#[tokio::test]
async fn notify_is_fire_and_forget() {
    let (port, mails) = smtp_server(false).await;
    let state = state_with(&[]).await;
    let inbox_id = add_inbox(&state.db, "contact", Some("a@example.com")).await;
    put_smtp(&state.db, Some(&inbox_id), port).await;
    let info = InboxInfo {
        id: inbox_id,
        slug: "contact".into(),
        notify_email: Some("a@example.com".into()),
        ..Default::default()
    };
    email::notify(&state, &info, &json!({"message": "hi"})).await;
    for _ in 0..100 {
        if !mails.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let m = mails.lock().unwrap();
    assert_eq!(m.len(), 1);
    assert!(m[0].data.contains("Subject: [contact] New Submission"));
    assert!(m[0].data.contains("From: HookForms <forms@example.com>"));
}
