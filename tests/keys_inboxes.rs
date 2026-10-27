//! HTTP tests for step 2: API keys, auth lockout and inbox CRUD.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use hookforms::config::Config;
use hookforms::{AppState, auth, db};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

const ADMIN: &str = "test-admin";

async fn app() -> (Router, SqlitePool) {
    let config = Config::from_map([
        ("ADMIN_API_KEY", ADMIN),
        ("DATABASE_URL", "sqlite::memory:"),
    ])
    .unwrap();
    let pool = db::init(&config.database_url).await.unwrap();
    let app = hookforms::build_app(AppState::new(pool.clone(), config, reqwest::Client::new()));
    (app, pool)
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

fn req(method: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        b = b.header("x-api-key", k);
    }
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

fn err(code: u16, message: &str) -> Value {
    json!({"error": {"code": code, "message": message}})
}

fn is_ts(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| s.len() >= 20 && s.ends_with('Z') && s.as_bytes()[10] == b'T')
}

// ---------------------------------------------------------------------------
// auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_and_invalid_keys() {
    let (app, _) = app().await;
    let (s, b) = send(&app, req("GET", "/v1/hooks/inboxes", None, None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b, err(401, "Missing API key"));
    let (s, b) = send(&app, req("GET", "/v1/hooks/inboxes", Some("hf_nope"), None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(b, err(401, "Invalid API key"));
}

#[tokio::test]
async fn lockout_after_ten_failures_per_ip() {
    let (app, _) = app().await;
    let with_ip = |key: &str, ip: &str| {
        Request::get("/v1/hooks/inboxes")
            .header("x-api-key", key)
            .header("cf-connecting-ip", ip)
            .body(Body::empty())
            .unwrap()
    };
    for _ in 0..10 {
        let (s, _) = send(&app, with_ip("wrong", "203.0.113.9")).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
    }
    // Locked out, even with the right key.
    let (s, b) = send(&app, with_ip(ADMIN, "203.0.113.9")).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        b,
        err(
            429,
            "Too many failed authentication attempts. Try again later."
        )
    );
    // Other IPs are unaffected, and a success clears that IP's count.
    for _ in 0..9 {
        send(&app, with_ip("wrong", "198.51.100.7")).await;
    }
    let (s, _) = send(&app, with_ip(ADMIN, "198.51.100.7")).await;
    assert_eq!(s, StatusCode::OK);
    for _ in 0..9 {
        send(&app, with_ip("wrong", "198.51.100.7")).await;
    }
    let (s, _) = send(&app, with_ip(ADMIN, "198.51.100.7")).await;
    assert_eq!(s, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// /v1/auth/keys
// ---------------------------------------------------------------------------

#[tokio::test]
async fn key_lifecycle() {
    let (app, pool) = app().await;
    let (s, b) = send(
        &app,
        req(
            "POST",
            "/v1/auth/keys",
            Some(ADMIN),
            Some(json!({"name": "site", "scopes": ["webhooks"]})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let d = &b["data"];
    let mut keys: Vec<&str> = d.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "created_at",
            "id",
            "is_active",
            "last_used_at",
            "name",
            "raw_key",
            "scopes"
        ]
    );
    assert_eq!(d["name"], "site");
    assert_eq!(d["scopes"], json!(["webhooks"]));
    assert_eq!(d["is_active"], true);
    assert_eq!(d["last_used_at"], Value::Null);
    assert!(is_ts(&d["created_at"]));
    let raw = d["raw_key"].as_str().unwrap().to_string();
    let id = d["id"].as_str().unwrap().to_string();
    assert!(raw.starts_with("hf_") && raw.len() == 46);
    uuid::Uuid::parse_str(&id).unwrap();

    let (hash, prefix): (String, String) =
        sqlx::query_as("SELECT key_hash, key_prefix FROM api_keys WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(hash.starts_with("$pbkdf2-sha256$29000$"));
    assert_eq!(prefix, &raw[..12]);

    // The new key works for its scope and not for admin routes.
    let (s, _) = send(&app, req("GET", "/v1/hooks/inboxes", Some(&raw), None)).await;
    assert_eq!(s, StatusCode::OK);
    let (s, b) = send(&app, req("GET", "/v1/auth/keys", Some(&raw), None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(b, err(403, "Key lacks required scope: admin"));

    // Listed without raw_key, with last_used_at set.
    let (s, b) = send(&app, req("GET", "/v1/auth/keys", Some(ADMIN), None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["meta"], json!({"total": 1, "limit": 50, "offset": 0}));
    let item = &b["data"][0];
    assert!(item.get("raw_key").is_none());
    assert_eq!(item["id"], id.as_str());
    assert!(is_ts(&item["last_used_at"]));

    // Revoke (any UUID spelling), then the key stops working but stays listed.
    let upper = id.to_uppercase();
    let (s, b) = send(
        &app,
        req(
            "DELETE",
            &format!("/v1/auth/keys/{upper}"),
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(b, Value::Null);
    let (s, _) = send(&app, req("GET", "/v1/hooks/inboxes", Some(&raw), None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (_, b) = send(&app, req("GET", "/v1/auth/keys", Some(ADMIN), None)).await;
    assert_eq!(b["data"][0]["is_active"], false);

    let missing = uuid::Uuid::new_v4();
    for path in [
        format!("/v1/auth/keys/{missing}"),
        "/v1/auth/keys/not-a-uuid".into(),
    ] {
        let (s, b) = send(&app, req("DELETE", &path, Some(ADMIN), None)).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(b, err(404, "API key not found"));
    }
}

#[tokio::test]
async fn admin_scope_key_reaches_everything_and_default_scopes_empty() {
    let (app, _) = app().await;
    let (_, b) = send(
        &app,
        req(
            "POST",
            "/v1/auth/keys",
            Some(ADMIN),
            Some(json!({"name": "ops", "scopes": ["admin"]})),
        ),
    )
    .await;
    let admin_raw = b["data"]["raw_key"].as_str().unwrap().to_string();
    let (s, _) = send(&app, req("GET", "/v1/auth/keys", Some(&admin_raw), None)).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = send(
        &app,
        req("GET", "/v1/hooks/inboxes", Some(&admin_raw), None),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let (s, b) = send(
        &app,
        req(
            "POST",
            "/v1/auth/keys",
            Some(ADMIN),
            Some(json!({"name": "bare"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(b["data"]["scopes"], json!([]));
    let raw = b["data"]["raw_key"].as_str().unwrap().to_string();
    let (s, b) = send(&app, req("GET", "/v1/hooks/inboxes", Some(&raw), None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(b, err(403, "Key lacks required scope: webhooks"));
}

#[tokio::test]
async fn legacy_key_without_prefix_is_backfilled() {
    let (app, pool) = app().await;
    let raw = auth::generate_key();
    let id = db::new_id();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, scopes) VALUES (?, 'old', ?, '[\"webhooks\"]')",
    )
    .bind(&id)
    .bind(auth::hash_key(&raw))
    .execute(&pool)
    .await
    .unwrap();
    let (s, _) = send(&app, req("GET", "/v1/hooks/inboxes", Some(&raw), None)).await;
    assert_eq!(s, StatusCode::OK);
    let (prefix, used): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT key_prefix, last_used_at FROM api_keys WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(prefix.as_deref(), Some(&raw[..12]));
    assert!(used.is_some());
}

#[tokio::test]
async fn key_create_validation_matches_pydantic() {
    let (app, _) = app().await;
    let post = |body: Option<Value>| req("POST", "/v1/auth/keys", Some(ADMIN), body);

    let (s, b) = send(
        &app,
        post(Some(
            json!({"name": "x", "scopes": ["webhooks", "root", "god", "root"]}),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        b,
        json!({"error": {"code": 422, "message": "Validation error", "details": [{
            "type": "value_error", "loc": ["body", "scopes"],
            "msg": "Value error, Invalid scopes: god, root. Valid: admin, webhooks",
            "input": ["webhooks", "root", "god", "root"]
        }]}})
    );

    let long = "n".repeat(101);
    let (s, b) = send(&app, post(Some(json!({"name": long, "scopes": "admin"})))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        b["error"]["details"],
        json!([
            {"type": "string_too_long", "loc": ["body", "name"],
             "msg": "String should have at most 100 characters", "input": long},
            {"type": "list_type", "loc": ["body", "scopes"],
             "msg": "Input should be a valid list", "input": "admin"}
        ])
    );

    let (_, b) = send(&app, post(Some(json!({"scopes": [1]})))).await;
    assert_eq!(
        b["error"]["details"],
        json!([
            {"type": "missing", "loc": ["body", "name"], "msg": "Field required", "input": {"scopes": [1]}},
            {"type": "string_type", "loc": ["body", "scopes", 0],
             "msg": "Input should be a valid string", "input": 1}
        ])
    );

    let (s, b) = send(&app, post(None)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        b["error"]["details"],
        json!([{"type": "missing", "loc": ["body"], "msg": "Field required", "input": null}])
    );

    // Auth comes before body validation.
    let (s, _) = send(&app, req("POST", "/v1/auth/keys", None, None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn key_list_pagination() {
    let (app, _) = app().await;
    for n in 0..3 {
        send(
            &app,
            req(
                "POST",
                "/v1/auth/keys",
                Some(ADMIN),
                Some(json!({"name": format!("k{n}")})),
            ),
        )
        .await;
    }
    let (s, b) = send(
        &app,
        req("GET", "/v1/auth/keys?limit=2&offset=0", Some(ADMIN), None),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["meta"], json!({"total": 3, "limit": 2, "offset": 0}));
    // Newest first.
    assert_eq!(b["data"][0]["name"], "k2");
    assert_eq!(b["data"].as_array().unwrap().len(), 2);
    let (_, b) = send(
        &app,
        req("GET", "/v1/auth/keys?limit=2&offset=2", Some(ADMIN), None),
    )
    .await;
    assert_eq!(b["data"][0]["name"], "k0");
    let (s, b) = send(
        &app,
        req("GET", "/v1/auth/keys?limit=500", Some(ADMIN), None),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["error"]["details"][0]["loc"], json!(["query", "limit"]));
}

// ---------------------------------------------------------------------------
// /v1/hooks/inboxes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inbox_crud() {
    let (app, pool) = app().await;
    let (s, b) = send(
        &app,
        req(
            "POST",
            "/v1/hooks/inboxes",
            Some(ADMIN),
            Some(json!({
                "slug": "contact",
                "description": "Contact form",
                "notify_email": "a@example.com, b@example.com",
                "turnstile_secret": "0x-secret",
                "unknown": "ignored"
            })),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let d = b["data"].clone();
    let created_at = d["created_at"].clone();
    assert!(is_ts(&created_at));
    let id = d["id"].as_str().unwrap().to_string();
    assert_eq!(
        d,
        json!({
            "id": id, "slug": "contact", "description": "Contact form",
            "forward_url": null, "notify_email": "a@example.com, b@example.com",
            "email_subject_prefix": null, "sender_name": null,
            "has_turnstile": true, "is_active": true, "created_at": created_at
        })
    );
    let (secret,): (String,) = sqlx::query_as("SELECT turnstile_secret FROM webhook_inboxes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(secret, "0x-secret");

    // Duplicate slug.
    let (s, b) = send(
        &app,
        req(
            "POST",
            "/v1/hooks/inboxes",
            Some(ADMIN),
            Some(json!({"slug": "contact"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b, err(409, "Inbox slug already exists"));

    // List.
    send(
        &app,
        req(
            "POST",
            "/v1/hooks/inboxes",
            Some(ADMIN),
            Some(json!({"slug": "second"})),
        ),
    )
    .await;
    let (s, b) = send(&app, req("GET", "/v1/hooks/inboxes", Some(ADMIN), None)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["meta"], json!({"total": 2, "limit": 50, "offset": 0}));
    assert_eq!(b["data"][0]["slug"], "second");
    assert_eq!(b["data"][1]["slug"], "contact");
    assert_eq!(b["data"][0]["has_turnstile"], false);

    // Patch: only present fields change, null clears, lax bool.
    let (s, b) = send(
        &app,
        req(
            "PATCH",
            "/v1/hooks/inboxes/contact",
            Some(ADMIN),
            Some(json!({"turnstile_secret": null, "sender_name": "Acme", "is_active": "false"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let d = &b["data"];
    assert_eq!(d["has_turnstile"], false);
    assert_eq!(d["sender_name"], "Acme");
    assert_eq!(d["is_active"], false);
    assert_eq!(d["description"], "Contact form");
    assert_eq!(d["created_at"], created_at);

    // Empty patch returns the inbox unchanged.
    let (s, b) = send(
        &app,
        req(
            "PATCH",
            "/v1/hooks/inboxes/contact",
            Some(ADMIN),
            Some(json!({})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["data"]["sender_name"], "Acme");

    let (s, b) = send(
        &app,
        req(
            "PATCH",
            "/v1/hooks/inboxes/nope",
            Some(ADMIN),
            Some(json!({})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(b, err(404, "Inbox not found"));

    // Body validation comes before the lookup.
    let (s, b) = send(
        &app,
        req(
            "PATCH",
            "/v1/hooks/inboxes/nope",
            Some(ADMIN),
            Some(json!({"is_active": [], "description": 5})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        b["error"]["details"],
        json!([
            {"type": "string_type", "loc": ["body", "description"],
             "msg": "Input should be a valid string", "input": 5},
            {"type": "bool_type", "loc": ["body", "is_active"],
             "msg": "Input should be a valid boolean", "input": []}
        ])
    );

    // Delete cascades to events.
    sqlx::query("INSERT INTO webhook_events (id, inbox_id, method) VALUES (?, ?, 'POST')")
        .bind(db::new_id())
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    let (s, b) = send(
        &app,
        req("DELETE", "/v1/hooks/inboxes/contact", Some(ADMIN), None),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(b, Value::Null);
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM webhook_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let (s, b) = send(
        &app,
        req("DELETE", "/v1/hooks/inboxes/contact", Some(ADMIN), None),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(b, err(404, "Inbox not found"));
}

#[tokio::test]
async fn inbox_forward_url_is_checked() {
    let (app, _) = app().await;
    let (s, b) = send(
        &app,
        req(
            "POST",
            "/v1/hooks/inboxes",
            Some(ADMIN),
            Some(json!({"slug": "x", "forward_url": "http://127.0.0.1/hook"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        b,
        err(
            400,
            "Invalid forward_url: URL resolves to private/reserved IP address"
        )
    );

    // Empty string skips the check, like Python.
    let (s, b) = send(
        &app,
        req(
            "POST",
            "/v1/hooks/inboxes",
            Some(ADMIN),
            Some(json!({"slug": "x", "forward_url": ""})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(b["data"]["forward_url"], "");

    let (s, b) = send(
        &app,
        req(
            "PATCH",
            "/v1/hooks/inboxes/x",
            Some(ADMIN),
            Some(json!({"forward_url": "ftp://example.com/"})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        b,
        err(
            400,
            "Invalid forward_url: Scheme 'ftp' not allowed. Use http or https."
        )
    );
}

#[tokio::test]
async fn inbox_create_validation() {
    let (app, _) = app().await;
    let (s, b) = send(
        &app,
        req(
            "POST",
            "/v1/hooks/inboxes",
            Some(ADMIN),
            Some(json!({"description": null})),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        b,
        json!({"error": {"code": 422, "message": "Validation error", "details": [
            {"type": "missing", "loc": ["body", "slug"], "msg": "Field required",
             "input": {"description": null}}
        ]}})
    );
    let bad_json = Request::post("/v1/hooks/inboxes")
        .header("x-api-key", ADMIN)
        .header("content-type", "application/json")
        .body(Body::from("{\"slug\": "))
        .unwrap();
    let (s, b) = send(&app, bad_json).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["error"]["details"][0]["type"], "json_invalid");
    assert_eq!(b["error"]["details"][0]["msg"], "JSON decode error");

    let (s, b) = send(
        &app,
        req("GET", "/v1/hooks/inboxes?offset=-1", Some(ADMIN), None),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(b["error"]["details"][0]["loc"], json!(["query", "offset"]));
}
