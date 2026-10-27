//! End-to-end test of `hookforms import-postgres` against a real Postgres
//! holding the Python schema (`tests/fixtures/python_schema.sql`).
//!
//! Needs a Postgres server, so it is ignored by default. Run it with:
//!
//! ```bash
//! docker run --rm -d --name hookforms-test-pg -e POSTGRES_PASSWORD=test -p 127.0.0.1:55432:5432 postgres:16
//! HOOKFORMS_TEST_PG_URL=postgres://postgres:test@127.0.0.1:55432/postgres \
//!   cargo test --test import_postgres -- --ignored
//! docker stop hookforms-test-pg
//! ```
//!
//! Each run makes its own database and drops it at the end.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use hookforms::cleanup::{self, Clock};
use hookforms::config::Config;
use hookforms::import::{self, ImportError};
use hookforms::{AppState, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
use tower::ServiceExt;

const ADMIN: &str = "test-admin";

// Keys "created in Python": the hashes come from passlib's pbkdf2_sha256.hash.
const PY_KEY: &str = "hf_python_created_key_0123456789abcdefghijklmn";
const PY_HASH: &str =
    "$pbkdf2-sha256$29000$XKuVMgYgREgJYezdW8uZEw$HuXLEBXq3H9bBq53SPjp7x9XuPQ0IaGz/nDKNgzHOw4";
const REVOKED_KEY: &str = "hf_revoked_key_000000000000000000000000000000";
const REVOKED_HASH: &str =
    "$pbkdf2-sha256$29000$XOt9D2FsDcF4r7W2Nkbo/Q$ieQop0Zk7iZqsZkmIscNilvk5OWmsoR0f1cER5uMcKs";
const LEGACY_KEY: &str = "hf_legacy_noprefix_00000000000000000000000000";
const LEGACY_HASH: &str =
    "$pbkdf2-sha256$29000$wzgnpDSGUKq1VgpBCMGYsw$jSoD5EDe0BCpwZRxzP/95lF26gjKu6x5dGEPvb66pEc";

const INBOX_A: &str = "11111111-1111-4111-8111-111111111111";
const INBOX_B: &str = "22222222-2222-4222-8222-222222222222";
const INBOX_C: &str = "33333333-3333-4333-8333-333333333333";
const EV_BIG: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const EV_NULL: &str = "aaaaaaaa-0000-4000-8000-000000000002";
const EV_JSON_NULL: &str = "aaaaaaaa-0000-4000-8000-000000000003";
const EV_TZ: &str = "aaaaaaaa-0000-4000-8000-000000000004";
/// Bulk events beyond the import's 1000-row batch size.
const BULK: i64 = 2105;

fn fixture_sql() -> String {
    format!(
        r#"
INSERT INTO webhook_inboxes VALUES
 ('{INBOX_A}', 'contact', 'Kontakt ✉ 日本語 "quoted"', 'https://example.com/hook',
  'ops@example.com, sales@example.com', '[Form]', 'Website Bot', 'turnstile-secret', true,
  '2026-01-02 03:04:05.123456+00', '2026-01-03 00:00:00+00'),
 ('{INBOX_B}', 'old-form', NULL, NULL, NULL, NULL, NULL, NULL, false,
  '2025-12-31 23:59:59.999999+00', '2025-12-31 23:59:59.999999+00'),
 ('{INBOX_C}', 'null-active', NULL, NULL, NULL, NULL, NULL, NULL, NULL,
  '2026-02-01 10:00:00+05:30', '2026-02-01 10:00:00+05:30');

INSERT INTO api_keys VALUES
 ('44444444-4444-4444-8444-444444444441', 'python key', '{PY_HASH}', 'hf_python_cr',
  '{{webhooks}}', true, '2026-05-05 05:05:05.5+00', '2026-01-01 00:00:00+00', '2026-01-01 00:00:00+00'),
 ('44444444-4444-4444-8444-444444444442', 'revoked', '{REVOKED_HASH}', 'hf_revoked_k',
  '{{webhooks,admin}}', false, NULL, '2026-01-02 00:00:00+00', '2026-01-04 00:00:00+00'),
 ('44444444-4444-4444-8444-444444444443', 'legacy', '{LEGACY_HASH}', NULL,
  '{{admin}}', true, NULL, '2026-01-03 00:00:00+00', '2026-01-03 00:00:00+00'),
 ('44444444-4444-4444-8444-444444444444', 'no scopes', '$pbkdf2-sha256$29000$AAAA$AAAA', 'hf_noscopes_',
  NULL, true, NULL, '2026-01-04 00:00:00+00', '2026-01-04 00:00:00+00');

INSERT INTO webhook_events VALUES
 ('{EV_BIG}', '{INBOX_A}', 'POST', '{{"content-type": "application/json", "x-ünï": "çødé"}}',
  (SELECT json_build_object('items', json_agg(json_build_object('n', g, 'text', repeat('x', 40) || ' ✓ 日本')))
     FROM generate_series(1, 3000) g),
  '{{"utm_source": "newsletter", "q": "a b"}}', '203.0.113.9', '2026-06-01 12:00:00.000001+00'),
 ('{EV_NULL}', '{INBOX_A}', 'GET', NULL, NULL, NULL, NULL, '2026-06-02 12:00:00+00'),
 ('{EV_JSON_NULL}', '{INBOX_A}', 'POST', '{{}}', 'null', '{{}}', '::1', '2026-06-03 12:00:00+00'),
 ('{EV_TZ}', '{INBOX_B}', 'POST', '{{}}', '[1, 2.5, "drei", true, null]', '{{}}', NULL,
  '2026-03-01 12:00:00.5+05:00');

INSERT INTO webhook_events (id, inbox_id, method, body, received_at)
 SELECT ('bbbbbbbb-0000-4000-8000-' || lpad(g::text, 12, '0'))::uuid, '{INBOX_C}', 'POST',
        json_build_object('n', g), timestamptz '2020-01-01 00:00:00+00' + g * interval '1 second'
   FROM generate_series(1, {BULK}) g;

INSERT INTO notification_channels VALUES
 ('55555555-5555-4555-8555-000000000001', '{INBOX_A}', 'email', 'Mail', '{{"to": "a@example.com"}}', true, '2026-01-05 00:00:01+00'),
 ('55555555-5555-4555-8555-000000000002', '{INBOX_A}', 'discord', NULL, '{{"webhook_url": "https://discord.com/api/webhooks/1/abc"}}', true, '2026-01-05 00:00:02+00'),
 ('55555555-5555-4555-8555-000000000003', '{INBOX_A}', 'slack', 'Slack', '{{"webhook_url": "https://hooks.slack.com/services/T/B/X"}}', true, '2026-01-05 00:00:03+00'),
 ('55555555-5555-4555-8555-000000000004', '{INBOX_A}', 'teams', 'Teams', '{{"webhook_url": "https://example.webhook.office.com/x"}}', true, '2026-01-05 00:00:04+00'),
 ('55555555-5555-4555-8555-000000000005', '{INBOX_A}', 'telegram', 'TG ✈', '{{"bot_token": "123:abc", "chat_id": "-100"}}', true, '2026-01-05 00:00:05+00'),
 ('55555555-5555-4555-8555-000000000006', '{INBOX_A}', 'ntfy', 'ntfy', '{{"topic": "forms", "server": "https://ntfy.sh"}}', true, '2026-01-05 00:00:06+00'),
 ('55555555-5555-4555-8555-000000000007', '{INBOX_A}', 'webhook', 'Hook', '{{"url": "https://example.com/in", "secret": "s"}}', false, '2026-01-05 00:00:07+00');

INSERT INTO email_providers VALUES
 ('66666666-6666-4666-8666-000000000001', NULL, 'resend', '{{"api_key": "re_secret", "from_email": "forms@example.com"}}', true, '2026-01-06 00:00:00+00'),
 ('66666666-6666-4666-8666-000000000002', '{INBOX_A}', 'smtp',
  '{{"host": "smtp.example.com", "port": 587, "username": "u", "password": "p", "from_email": "a@example.com", "use_tls": true}}',
  true, '2026-01-06 00:00:01+00');
"#
    )
}

async fn send(app: &Router, method: &str, uri: &str, key: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-api-key", key)
        .body(Body::empty())
        .unwrap();
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

struct Fixed(chrono::DateTime<Utc>);
impl Clock for Fixed {
    fn now(&self) -> chrono::DateTime<Utc> {
        self.0
    }
}

#[tokio::test]
#[ignore = "needs a Postgres server in HOOKFORMS_TEST_PG_URL"]
async fn imports_a_python_install() {
    let admin_url = std::env::var("HOOKFORMS_TEST_PG_URL").expect("HOOKFORMS_TEST_PG_URL");
    let db_name = format!("hookforms_import_{}", db::new_id().replace('-', ""));
    let mut admin = PgConnection::connect(&admin_url).await.unwrap();
    sqlx::raw_sql(&format!("CREATE DATABASE {db_name}"))
        .execute(&mut admin)
        .await
        .unwrap();
    let base = admin_url.rsplit_once('/').unwrap().0;
    let src_url = format!("{base}/{db_name}");

    let result = run_checks(&src_url).await;

    sqlx::raw_sql(&format!("DROP DATABASE {db_name} WITH (FORCE)"))
        .execute(&mut admin)
        .await
        .unwrap();
    result.unwrap();
}

async fn run_checks(src_url: &str) -> Result<(), String> {
    let mut pg = PgConnection::connect(src_url).await.unwrap();
    sqlx::raw_sql(include_str!("fixtures/python_schema.sql"))
        .execute(&mut pg)
        .await
        .unwrap();
    sqlx::raw_sql(&fixture_sql())
        .execute(&mut pg)
        .await
        .unwrap();
    let big_body: String = sqlx::query_scalar(&format!(
        "SELECT body::text FROM webhook_events WHERE id = '{EV_BIG}'"
    ))
    .fetch_one(&mut pg)
    .await
    .unwrap();
    let big_body: Value = serde_json::from_str(&big_body).unwrap();
    // An extra table is reported and skipped, never read.
    sqlx::raw_sql("CREATE TABLE stray_table (x int)")
        .execute(&mut pg)
        .await
        .unwrap();

    // The Python DATABASE_URL form is accepted as it is.
    let from = import::postgres_url(&src_url.replace("postgres://", "postgresql+asyncpg://"))
        .map_err(|e| e.to_string())?;

    let config = Config::from_map([
        ("ADMIN_API_KEY", ADMIN),
        ("DATABASE_URL", "sqlite::memory:"),
    ])
    .unwrap();
    let pool = db::init(&config.database_url).await.unwrap();
    let report = import::import_into(&pool, &from, false)
        .await
        .map_err(|e| e.to_string())?;
    let counts: Vec<(&str, i64)> = report.counts.clone();
    assert_eq!(
        counts,
        [
            ("webhook_inboxes", 3),
            ("api_keys", 4),
            ("email_providers", 2),
            ("notification_channels", 7),
            ("webhook_events", 4 + BULK),
        ]
    );
    assert_eq!(report.skipped, ["stray_table"]);
    assert!(report.missing.is_empty());
    let printed = report.to_string();
    for secret in [
        "re_secret",
        "turnstile-secret",
        PY_HASH,
        "123:abc",
        "Kontakt",
    ] {
        assert!(
            !printed.contains(secret),
            "report must not print row contents"
        );
    }

    // Stored formats.
    let (scopes, active, last_used): (Option<String>, bool, Option<String>) = sqlx::query_as(
        "SELECT scopes, is_active, last_used_at FROM api_keys WHERE name = 'python key'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(scopes.as_deref(), Some(r#"["webhooks"]"#));
    assert!(active);
    assert_eq!(last_used.as_deref(), Some("2026-05-05T05:05:05.500000Z"));
    let (scopes,): (Option<String>,) =
        sqlx::query_as("SELECT scopes FROM api_keys WHERE name = 'no scopes'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(scopes, None);
    let (tz,): (String,) = sqlx::query_as("SELECT received_at FROM webhook_events WHERE id = ?")
        .bind(EV_TZ)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(tz, "2026-03-01T07:00:00.500000Z");
    let (null_active,): (bool,) =
        sqlx::query_as("SELECT is_active FROM webhook_inboxes WHERE slug = 'null-active'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!null_active, "NULL is_active was inactive in Python");
    let (updated,): (String,) =
        sqlx::query_as("SELECT updated_at FROM webhook_inboxes WHERE slug = 'contact'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(updated, "2026-01-03T00:00:00.000000Z");

    let app = hookforms::build_app(AppState::new(
        pool.clone(),
        config.clone(),
        reqwest::Client::new(),
    ));

    // Inboxes come back field for field.
    let (s, b) = send(&app, "GET", "/v1/hooks/inboxes", ADMIN).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["meta"]["total"], 3);
    let contact = b["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["slug"] == "contact")
        .unwrap()
        .clone();
    assert_eq!(contact["id"], INBOX_A);
    assert_eq!(contact["description"], "Kontakt ✉ 日本語 \"quoted\"");
    assert_eq!(contact["forward_url"], "https://example.com/hook");
    assert_eq!(
        contact["notify_email"],
        "ops@example.com, sales@example.com"
    );
    assert_eq!(contact["is_active"], true);
    assert_eq!(contact["created_at"], "2026-01-02T03:04:05.123456Z");
    assert_eq!(contact["has_turnstile"], true);
    let old = b["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["slug"] == "old-form")
        .unwrap()
        .clone();
    assert_eq!(old["is_active"], false);
    assert_eq!(old["has_turnstile"], false);
    assert_eq!(old["description"], Value::Null);
    assert_eq!(old["created_at"], "2025-12-31T23:59:59.999999Z");

    // A key created in Python authenticates, the revoked one does not, the
    // legacy row without a prefix works through the fallback.
    let (s, _) = send(&app, "GET", "/v1/hooks/inboxes", PY_KEY).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(&app, "GET", "/v1/auth/keys", PY_KEY).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "webhooks scope only");
    let (s, _) = send(&app, "GET", "/v1/hooks/inboxes", REVOKED_KEY).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, b) = send(&app, "GET", "/v1/auth/keys", LEGACY_KEY).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["meta"]["total"], 4);
    let revoked = b["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == "revoked")
        .unwrap()
        .clone();
    assert_eq!(revoked["is_active"], false);
    assert_eq!(revoked["scopes"], json!(["webhooks", "admin"]));

    // Events: big JSON and unicode survive, SQL NULL and JSON null both read as null.
    let (s, b) = send(
        &app,
        "GET",
        &format!("/v1/hooks/contact/events/{EV_BIG}"),
        ADMIN,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["body"], big_body);
    assert_eq!(b["data"]["headers"]["x-ünï"], "çødé");
    assert_eq!(b["data"]["query_params"]["q"], "a b");
    assert_eq!(b["data"]["source_ip"], "203.0.113.9");
    assert_eq!(b["data"]["received_at"], "2026-06-01T12:00:00.000001Z");
    for id in [EV_NULL, EV_JSON_NULL] {
        let (s, b) = send(
            &app,
            "GET",
            &format!("/v1/hooks/contact/events/{id}"),
            ADMIN,
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(b["data"]["body"], Value::Null);
        assert_eq!(b["data"]["headers"], json!({}));
    }
    let (s, b) = send(
        &app,
        "GET",
        &format!("/v1/hooks/old-form/events/{EV_TZ}"),
        ADMIN,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["body"], json!([1, 2.5, "drei", true, null]));
    let (s, b) = send(&app, "GET", "/v1/hooks/null-active/events?limit=1", ADMIN).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["meta"]["total"], BULK);

    // Channels of every type, the inactive one included.
    let (s, b) = send(&app, "GET", "/v1/hooks/inboxes/contact/channels", ADMIN).await;
    assert_eq!(s, StatusCode::OK);
    let mut types: Vec<String> = b["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["type"].as_str().unwrap().to_string())
        .collect();
    types.sort();
    assert_eq!(
        types,
        [
            "discord", "email", "ntfy", "slack", "teams", "telegram", "webhook"
        ]
    );
    let hook = b["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "webhook")
        .unwrap()
        .clone();
    assert_eq!(hook["is_active"], false);
    let tg = b["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "telegram")
        .unwrap()
        .clone();
    assert_eq!(tg["label"], "TG ✈");
    assert_eq!(tg["created_at"], "2026-01-05T00:00:05Z");

    // The global and the per-inbox email provider.
    let (s, b) = send(&app, "GET", "/v1/hooks/config/email-provider", ADMIN).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["type"], "resend");
    assert_eq!(b["data"]["inbox_id"], Value::Null);
    let (s, b) = send(
        &app,
        "GET",
        "/v1/hooks/config/email-provider?inbox=contact",
        ADMIN,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["type"], "smtp");
    assert_eq!(b["data"]["inbox_id"], INBOX_A);
    assert_eq!(b["data"]["config"]["port"], 587);
    assert_eq!(b["data"]["config"]["use_tls"], true);

    // Imported timestamps compare correctly in the retention cleanup:
    // with "now" at 2026-06-02T12:00:00Z and 1 day of retention, only events
    // received before 2026-06-01T12:00:00Z go (EV_BIG is 1 us after it).
    let now = Utc.with_ymd_and_hms(2026, 6, 2, 12, 0, 0).unwrap();
    let deleted = cleanup::run_once(&pool, &Fixed(now), 1).await.unwrap();
    assert_eq!(deleted, 1 + BULK as u64, "EV_TZ and the bulk events");
    let (left,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM webhook_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 3);

    // A second import refuses without --force and replaces with it.
    let err = import::import_into(&pool, &from, false).await.unwrap_err();
    assert!(matches!(err, ImportError::TargetNotEmpty(_)));
    let again = import::import_into(&pool, &from, true)
        .await
        .map_err(|e| e.to_string())?;
    assert!(again.replaced);
    assert_eq!(again.counts, counts);

    // A source on migration 0001 only (no channel tables) still imports.
    sqlx::raw_sql("DROP TABLE notification_channels; DROP TABLE email_providers")
        .execute(&mut pg)
        .await
        .unwrap();
    let older = import::import_into(&pool, &from, true)
        .await
        .map_err(|e| e.to_string())?;
    assert_eq!(older.missing, ["email_providers", "notification_channels"]);
    assert_eq!(older.counts[2], ("email_providers", 0));
    assert_eq!(older.counts[4], ("webhook_events", 4 + BULK));
    pg.close().await.unwrap();
    Ok(())
}
