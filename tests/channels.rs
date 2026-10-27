//! Channel CRUD over HTTP and channel delivery against local mock servers.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, Uri};
use hookforms::channels::email::{ChannelMailer, SendFuture};
use hookforms::channels::{self, ChannelRow, DispatchReport, InboxInfo};
use hookforms::config::Config;
use hookforms::{AppState, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KEY: &str = "test-admin";

async fn state_with(http: reqwest::Client) -> AppState {
    let config =
        Config::from_map([("ADMIN_API_KEY", KEY), ("DATABASE_URL", "sqlite::memory:")]).unwrap();
    let pool = db::init(&config.database_url).await.unwrap();
    AppState::new(pool, config, http)
}

async fn add_inbox(state: &AppState, slug: &str, prefix: Option<&str>) -> String {
    let id = db::new_id();
    sqlx::query("INSERT INTO webhook_inboxes (id, slug, email_subject_prefix) VALUES (?, ?, ?)")
        .bind(&id)
        .bind(slug)
        .bind(prefix)
        .execute(&state.db)
        .await
        .unwrap();
    id
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    key: bool,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(uri);
    if key {
        req = req.header("x-api-key", KEY);
    }
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, v)
}

const BASE: &str = "/v1/hooks/inboxes/contact/channels";

#[tokio::test]
async fn crud_round_trip() {
    let state = state_with(reqwest::Client::new()).await;
    let inbox_id = add_inbox(&state, "contact", None).await;
    let app = hookforms::build_app(state);

    // Auth first.
    let (s, b) = call(&app, "GET", BASE, None, false).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(
        b,
        json!({"error": {"code": 401, "message": "Missing API key"}})
    );

    // Unknown inbox.
    let (s, b) = call(&app, "GET", "/v1/hooks/inboxes/nope/channels", None, true).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        b,
        json!({"error": {"code": 404, "message": "Inbox not found"}})
    );

    // Generic webhook with a Slack URL is stored as slack.
    let slack_url = "https://hooks.slack.com/services/T000/B000/XXXXXXXX";
    let (s, b) = call(
        &app,
        "POST",
        BASE,
        Some(json!({"type": "webhook", "label": "Team", "config": {"url": slack_url}})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let created = &b["data"];
    assert_eq!(created["type"], "slack");
    assert_eq!(created["label"], "Team");
    assert_eq!(created["inbox_id"], inbox_id.as_str());
    assert_eq!(created["is_active"], true);
    assert_eq!(
        created["config"],
        json!({"url": slack_url}),
        "POST answers unredacted"
    );
    let keys: Vec<&str> = created
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
            "label",
            "config",
            "is_active",
            "created_at"
        ]
    );
    let slack_id = created["id"].as_str().unwrap().to_string();

    let (s, b) = call(
        &app,
        "POST",
        BASE,
        Some(json!({"type": "email", "config": {"recipients": ["a@b.c"]}})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(b["data"]["label"], Value::Null);

    // List: newest first, sensitive values redacted, no meta.
    let (s, b) = call(&app, "GET", BASE, None, true).await;
    assert_eq!(s, StatusCode::OK);
    assert!(b.get("meta").is_none());
    let items = b["data"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["type"], "email");
    assert_eq!(items[0]["config"], json!({"recipients": ["a@b.c"]}));
    assert_eq!(items[1]["config"], json!({"url": "http***XXXX"}));

    // PATCH: partial update, no config validation (as Python).
    let uri = format!("{BASE}/{slack_id}");
    let (s, b) = call(
        &app,
        "PATCH",
        &uri,
        Some(json!({"is_active": false, "label": null})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["is_active"], false);
    assert_eq!(b["data"]["label"], Value::Null);
    assert_eq!(b["data"]["type"], "slack");
    assert_eq!(b["data"]["config"], json!({"url": slack_url}));

    let (s, b) = call(&app, "PATCH", &uri, Some(json!({"type": "pigeon"})), true).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        b,
        json!({"error": {"code": 400, "message": "Invalid channel type: pigeon"}})
    );

    let (s, b) = call(
        &app,
        "PATCH",
        &format!("{BASE}/missing"),
        Some(json!({})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(
        b,
        json!({"error": {"code": 404, "message": "Channel not found"}})
    );

    // DELETE: 204 with no body, then 404.
    let (s, b) = call(&app, "DELETE", &uri, None, true).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(b, Value::Null);
    let (s, _) = call(&app, "DELETE", &uri, None, true).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_errors_match_python() {
    let state = state_with(reqwest::Client::new()).await;
    add_inbox(&state, "contact", None).await;
    let app = hookforms::build_app(state);

    let cases = [
        (
            json!({"type": "discrod", "config": {}}),
            "Invalid channel type: discrod. Did you mean 'discord'?",
        ),
        (
            json!({"type": "pigeon", "config": {}}),
            "Invalid channel type: pigeon.",
        ),
        (
            json!({"type": "discord", "config": {"webhook_url": "https://example.com/x"}}),
            "Discord webhook_url must be a discord.com webhook URL",
        ),
        (
            json!({"type": "webhook", "config": {"url": "ftp://example.com"}}),
            "url must use http or https protocol",
        ),
        (
            json!({"type": "webhook", "config": {"url": "https://discord.com/api/webhooks/1/x", "custom_headers": 1}}),
            "Discord webhook_url must be a discord.com webhook URL",
        ),
        (
            json!({"type": "telegram", "config": {"bot_url": "https://api.telegram.org/botX/sendMessage"}}),
            "Telegram channel requires chat_id",
        ),
        (
            json!({"type": "email", "config": {"recipients": []}}),
            "Email channel requires a non-empty recipients array",
        ),
    ];
    for (body, message) in cases {
        let (s, b) = call(&app, "POST", BASE, Some(body.clone()), true).await;
        if message.starts_with("Discord") && body["type"] == "webhook" {
            // A detected Discord URL passes the Discord check; custom_headers is not
            // checked once the type changed, exactly like Python.
            assert_eq!(s, StatusCode::CREATED, "{body}");
            assert_eq!(b["data"]["type"], "discord");
            continue;
        }
        assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(b, json!({"error": {"code": 400, "message": message}}));
    }

    // Body validation comes before the inbox lookup.
    let (s, b) = call(
        &app,
        "POST",
        "/v1/hooks/inboxes/nope/channels",
        Some(json!({"type": "slack"})),
        true,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        b,
        json!({"error": {"code": 422, "message": "Validation error", "details": [
            {"type": "missing", "loc": ["body", "config"], "msg": "Field required", "input": {"type": "slack"}}
        ]}})
    );
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Hit {
    path: String,
    headers: HeaderMap,
    body: String,
}

type Hits = Arc<Mutex<Vec<Hit>>>;

async fn record(State(hits): State<Hits>, uri: Uri, headers: HeaderMap, body: Bytes) -> StatusCode {
    let path = uri.path().to_string();
    let fail = path.contains("fail");
    hits.lock().unwrap().push(Hit {
        path,
        headers,
        body: String::from_utf8_lossy(&body).into(),
    });
    if fail {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK
    }
}

/// A local receiver, reached as `http://mock.test:<port>` through a client
/// that resolves that name to 127.0.0.1 (the SSRF-safe client would refuse it).
async fn mock() -> (Hits, String, reqwest::Client) {
    let hits: Hits = Arc::default();
    let app = Router::new().fallback(record).with_state(hits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::builder()
        .resolve("mock.test", addr)
        .build()
        .unwrap();
    (hits, format!("http://mock.test:{}", addr.port()), client)
}

async fn add_channel(state: &AppState, inbox_id: &str, t: &str, config: Value, active: bool) {
    sqlx::query("INSERT INTO notification_channels (id, inbox_id, type, config, is_active) VALUES (?, ?, ?, ?, ?)")
        .bind(db::new_id())
        .bind(inbox_id)
        .bind(t)
        .bind(config.to_string())
        .bind(active)
        .execute(&state.db)
        .await
        .unwrap();
}

fn inbox(id: &str) -> InboxInfo {
    InboxInfo {
        id: id.into(),
        slug: "contact".into(),
        email_subject_prefix: Some("[Site]".into()),
        sender_name: None,
    }
}

#[tokio::test]
async fn dispatch_sends_every_channel_type() {
    let (hits, base, client) = mock().await;
    let state = state_with(client).await;
    let id = add_inbox(&state, "contact", Some("[Site]")).await;
    add_channel(
        &state,
        &id,
        "discord",
        json!({"webhook_url": format!("{base}/discord.com/api/webhooks/1/a")}),
        true,
    )
    .await;
    add_channel(
        &state,
        &id,
        "slack",
        json!({"webhook_url": format!("{base}/hooks.slack.com/services/x")}),
        true,
    )
    .await;
    add_channel(
        &state,
        &id,
        "teams",
        json!({"webhook_url": format!("{base}/teams")}),
        true,
    )
    .await;
    add_channel(
        &state,
        &id,
        "telegram",
        json!({"bot_url": format!("{base}/api.telegram.org/botT/sendMessage"), "chat_id": "42"}),
        true,
    )
    .await;
    add_channel(
        &state,
        &id,
        "ntfy",
        json!({"url": format!("{base}/ntfy")}),
        true,
    )
    .await;
    add_channel(
        &state,
        &id,
        "webhook",
        json!({"url": format!("{base}/plain"), "custom_headers": {"X-Token": "abc"}}),
        true,
    )
    .await;
    // A generic webhook whose url is a Discord URL is sent as Discord.
    add_channel(
        &state,
        &id,
        "webhook",
        json!({"url": format!("{base}/fail/discord.com/api/webhooks/2/b")}),
        true,
    )
    .await;
    add_channel(
        &state,
        &id,
        "slack",
        json!({"webhook_url": format!("{base}/inactive")}),
        false,
    )
    .await;

    let body = json!({"name": "Ada", "email": "ada@example.com", "cf-turnstile-response": "x", "empty": ""});
    let report = channels::dispatch_now(&state, &inbox(&id), &body).await;
    assert_eq!(
        report,
        DispatchReport {
            sent: 6,
            failed: 1,
            emails: 0
        }
    );

    let hits = hits.lock().unwrap().clone();
    let by = |p: &str| {
        hits.iter()
            .find(|h| h.path.starts_with(p))
            .unwrap_or_else(|| panic!("{p}"))
            .clone()
    };
    assert!(hits.iter().all(|h| !h.path.contains("inactive")));
    assert!(
        hits.iter()
            .all(|h| h.headers["x-forwarded-from"] == "hookforms/hooks/contact")
    );

    let d = by("/discord.com");
    assert_eq!(d.headers["content-type"], "application/json");
    let v: Value = serde_json::from_str(&d.body).unwrap();
    assert_eq!(v["embeds"][0]["title"], "[Site] New Submission");
    assert_eq!(
        v["embeds"][0]["fields"],
        json!([
            {"name": "Name", "value": "Ada", "inline": true},
            {"name": "Email", "value": "ada@example.com", "inline": true},
        ])
    );
    let detected: Value = serde_json::from_str(&by("/fail/discord.com").body).unwrap();
    assert_eq!(detected["embeds"][0]["fields"], v["embeds"][0]["fields"]);

    let s: Value = serde_json::from_str(&by("/hooks.slack.com").body).unwrap();
    assert_eq!(
        s["blocks"][0]["text"]["text"],
        "*name:* Ada\n*email:* ada@example.com"
    );

    let t: Value = serde_json::from_str(&by("/api.telegram.org").body).unwrap();
    assert_eq!(t["chat_id"], "42");
    assert_eq!(t["parse_mode"], "HTML");

    let n = by("/ntfy");
    assert_eq!(n.body, "Name: Ada\nEmail: ada@example.com");
    assert_eq!(n.headers["title"], "[Site] New Submission");
    assert_eq!(n.headers["tags"], "incoming_envelope");
    assert_eq!(n.headers["priority"], "default");

    let w = by("/plain");
    assert_eq!(w.headers["x-token"], "abc");
    assert_eq!(
        w.body,
        r#"{"name": "Ada", "email": "ada@example.com", "empty": ""}"#
    );

    let tm: Value = serde_json::from_str(&by("/teams").body).unwrap();
    assert_eq!(
        tm["attachments"][0]["content"]["body"][1]["facts"][0],
        json!({"title": "Name", "value": "Ada"})
    );
}

#[tokio::test]
async fn dispatch_is_fire_and_forget() {
    let (hits, base, client) = mock().await;
    let state = state_with(client).await;
    let id = add_inbox(&state, "contact", None).await;
    add_channel(
        &state,
        &id,
        "ntfy",
        json!({"url": format!("{base}/ntfy")}),
        true,
    )
    .await;

    // Empty and non-object bodies notify nobody.
    channels::dispatch(&state, &inbox(&id), &json!({})).await;
    channels::dispatch_now(&state, &inbox(&id), &json!([1, 2])).await;

    channels::dispatch(&state, &inbox(&id), &json!({"msg": "hi"})).await;
    for _ in 0..100 {
        if !hits.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let hits = hits.lock().unwrap().clone();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].body, "Msg: hi");
}

#[tokio::test]
async fn private_targets_are_refused() {
    let state = state_with(reqwest::Client::new()).await;
    let id = add_inbox(&state, "contact", None).await;
    add_channel(
        &state,
        &id,
        "webhook",
        json!({"url": "http://127.0.0.1:9/x"}),
        true,
    )
    .await;
    add_channel(
        &state,
        &id,
        "ntfy",
        json!({"url": "http://169.254.169.254/latest"}),
        true,
    )
    .await;
    let report = channels::dispatch_now(&state, &inbox(&id), &json!({"a": 1})).await;
    assert_eq!(
        report,
        DispatchReport {
            sent: 0,
            failed: 2,
            emails: 0
        }
    );
}

#[derive(Debug, Clone)]
struct Sent {
    to: String,
    subject: String,
    html: String,
    text: String,
    sender: String,
}

struct MockMailer(Mutex<Vec<Sent>>);

impl ChannelMailer for MockMailer {
    fn provider_type(&self) -> &str {
        "mock"
    }
    fn send_email<'a>(
        &'a self,
        to: &'a str,
        subject: &'a str,
        html: &'a str,
        text: &'a str,
        sender: &'a str,
    ) -> SendFuture<'a> {
        Box::pin(async move {
            if to.starts_with("bad") {
                return Err("rejected".into());
            }
            self.0.lock().unwrap().push(Sent {
                to: to.into(),
                subject: subject.into(),
                html: html.into(),
                text: text.into(),
                sender: sender.into(),
            });
            Ok(())
        })
    }
}

#[tokio::test]
async fn email_channels_use_provider_and_rate_limit() {
    let state = state_with(reqwest::Client::new()).await;
    let id = add_inbox(&state, "contact", None).await;
    let rows = vec![
        ChannelRow {
            id: "1".into(),
            inbox_id: id.clone(),
            channel_type: "email".into(),
            label: None,
            config: json!({"recipients": ["a@x.io", "bad@x.io"]}).to_string(),
            is_active: Some(true),
            created_at: String::new(),
        },
        ChannelRow {
            id: "2".into(),
            inbox_id: id.clone(),
            channel_type: "email".into(),
            label: None,
            config: json!({"recipients": "c@x.io, d@x.io"}).to_string(),
            is_active: Some(true),
            created_at: String::new(),
        },
    ];
    let mailer = MockMailer(Mutex::default());
    let body = json!({"name": "Ada", "email": "ada@example.com"});
    let info = InboxInfo {
        id: id.clone(),
        slug: "contact".into(),
        email_subject_prefix: None,
        sender_name: Some("Acme".into()),
    };

    // No provider: nothing sent and the rate counter is untouched.
    let r = channels::dispatch_channels(&state, &info, &rows, &body, None).await;
    assert_eq!(r.emails, 0);

    let r = channels::dispatch_channels(&state, &info, &rows, &body, Some(&mailer)).await;
    assert_eq!(r.emails, 3);
    {
        let sent = mailer.0.lock().unwrap();
        let to: Vec<&str> = sent.iter().map(|s| s.to.as_str()).collect();
        assert_eq!(to, ["a@x.io", "c@x.io", "d@x.io"]);
        assert_eq!(sent[0].subject, "[contact] from Ada");
        assert_eq!(sent[0].sender, "Acme");
        // Every send carries the designed HTML and its plain-text twin.
        assert!(sent[0].html.contains("Reply to Ada</a>"));
        assert!(sent[0].html.contains("<title>[contact] from Ada</title>"));
        assert!(
            sent[0]
                .text
                .contains("Reply to Ada: mailto:ada@example.com")
        );
        assert!(!sent[0].text.contains("Delivered by"));
    }

    // 10 dispatches per 10 minutes; the first one above already counted.
    for _ in 0..9 {
        assert_eq!(
            channels::dispatch_channels(&state, &info, &rows, &body, Some(&mailer))
                .await
                .emails,
            3
        );
    }
    assert_eq!(
        channels::dispatch_channels(&state, &info, &rows, &body, Some(&mailer))
            .await
            .emails,
        0
    );
}
