//! `/v1/hooks/config/email-provider` (Python `routers/channels.py`): GET, PUT
//! and DELETE (204), per inbox (`inbox` slug) or as the global default (no
//! slug, stored with `inbox_id` NULL). All need `RequireWebhooks`.
//!
//! The global row relies on the same trick as Python: `inbox_id` is UNIQUE but
//! NULLs never collide, so PUT first deletes every row of the scope and then
//! inserts the new one, inside one transaction here.

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::auth::RequireWebhooks;
use crate::db;
use crate::email;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/v1/hooks/config/email-provider",
        get(get_provider)
            .put(put_provider)
            .delete(delete_provider)
            .head(crate::error::method_not_allowed),
    )
}

#[derive(Debug, Deserialize)]
struct InboxQuery {
    inbox: Option<String>,
}

/// Python `_get_inbox`: the inbox id for a slug, else 404.
async fn inbox_id_for(state: &AppState, slug: &str) -> AppResult<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM webhook_inboxes WHERE slug = ?")
        .bind(slug)
        .fetch_optional(&state.db)
        .await?;
    row.map(|r| r.0)
        .ok_or_else(|| AppError::not_found("Inbox not found"))
}

/// `Some(id)` for a non-empty slug, `None` for the global scope.
async fn scope(state: &AppState, slug: Option<&str>) -> AppResult<Option<String>> {
    match slug {
        Some(s) if !s.is_empty() => Ok(Some(inbox_id_for(state, s).await?)),
        _ => Ok(None),
    }
}

type ProviderRow = (String, Option<String>, String, String, bool, String);

fn row_json(row: ProviderRow, redact: bool) -> Value {
    let (id, inbox_id, kind, config, is_active, created_at) = row;
    let config: Map<String, Value> = serde_json::from_str(&config).unwrap_or_default();
    let config = if redact {
        email::redact_config(&config)
    } else {
        config
    };
    json!({
        "id": id,
        "inbox_id": inbox_id,
        "type": kind,
        "config": config,
        "is_active": is_active,
        "created_at": crate::models::api_timestamp(&created_at),
    })
}

async fn get_provider(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Query(q): Query<InboxQuery>,
) -> AppResult<Json<Value>> {
    let inbox_id = scope(&state, q.inbox.as_deref()).await?;
    let sql_base = "SELECT id, inbox_id, type, config, is_active, created_at FROM email_providers";
    let row: Option<ProviderRow> = match &inbox_id {
        Some(id) => {
            sqlx::query_as(&format!(
                "{sql_base} WHERE inbox_id = ? AND is_active = 1 ORDER BY created_at DESC LIMIT 1"
            ))
            .bind(id)
            .fetch_optional(&state.db)
            .await?
        }
        None => sqlx::query_as(&format!(
            "{sql_base} WHERE inbox_id IS NULL AND is_active = 1 ORDER BY created_at DESC LIMIT 1"
        ))
        .fetch_optional(&state.db)
        .await?,
    };
    Ok(Json(match row {
        Some(row) => json!({ "data": row_json(row, true) }),
        None => json!({
            "data": null,
            "meta": { "fallback": if email::has_env_gmail(&state) { Some("env_gmail") } else { None } },
        }),
    }))
}

/// Parsed `EmailProviderUpsert` body.
struct Upsert {
    inbox: Option<String>,
    kind: String,
    config: Map<String, Value>,
}

fn pydantic_error(kind: &str, loc: Value, msg: &str, input: Value) -> Value {
    json!({ "type": kind, "loc": loc, "msg": msg, "input": input })
}

/// FastAPI's JSON body handling plus pydantic validation of
/// `{inbox: Optional[str], type: str, config: dict}`, with the same error list.
fn parse_upsert(headers: &HeaderMap, bytes: &Bytes) -> Result<Upsert, AppError> {
    if bytes.is_empty() {
        return Err(AppError::Validation(vec![pydantic_error(
            "missing",
            json!(["body"]),
            "Field required",
            Value::Null,
        )]));
    }
    let is_json = match headers.get("content-type").and_then(|v| v.to_str().ok()) {
        None => true,
        Some(ct) => {
            let mime = ct
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            match mime.split_once('/') {
                Some(("application", sub)) => sub == "json" || sub.ends_with("+json"),
                _ => false,
            }
        }
    };
    let body: Value = if is_json {
        match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(e) => {
                // Python reports the character offset of the decode error.
                let text = String::from_utf8_lossy(bytes);
                let pos: usize = text
                    .split_inclusive('\n')
                    .take(e.line().saturating_sub(1))
                    .map(|l| l.chars().count())
                    .sum::<usize>()
                    + e.column().saturating_sub(1);
                return Err(AppError::Validation(vec![pydantic_error(
                    "json_invalid",
                    json!(["body", pos]),
                    "JSON decode error",
                    json!({}),
                )]));
            }
        }
    } else {
        Value::String(String::from_utf8_lossy(bytes).into_owned())
    };
    let Value::Object(obj) = &body else {
        return Err(AppError::Validation(vec![pydantic_error(
            "model_attributes_type",
            json!(["body"]),
            "Input should be a valid dictionary or object to extract attributes from",
            body.clone(),
        )]));
    };

    let mut errors = Vec::new();
    let inbox = match obj.get("inbox") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => {
            errors.push(pydantic_error(
                "string_type",
                json!(["body", "inbox"]),
                "Input should be a valid string",
                other.clone(),
            ));
            None
        }
    };
    let kind = match obj.get("type") {
        Some(Value::String(s)) => s.clone(),
        None => {
            errors.push(pydantic_error(
                "missing",
                json!(["body", "type"]),
                "Field required",
                body.clone(),
            ));
            String::new()
        }
        Some(other) => {
            errors.push(pydantic_error(
                "string_type",
                json!(["body", "type"]),
                "Input should be a valid string",
                other.clone(),
            ));
            String::new()
        }
    };
    let config = match obj.get("config") {
        Some(Value::Object(m)) => m.clone(),
        None => {
            errors.push(pydantic_error(
                "missing",
                json!(["body", "config"]),
                "Field required",
                body.clone(),
            ));
            Map::new()
        }
        Some(other) => {
            errors.push(pydantic_error(
                "dict_type",
                json!(["body", "config"]),
                "Input should be a valid dictionary",
                other.clone(),
            ));
            Map::new()
        }
    };
    if !errors.is_empty() {
        return Err(AppError::Validation(errors));
    }
    Ok(Upsert {
        inbox,
        kind,
        config,
    })
}

async fn put_provider(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    headers: HeaderMap,
    bytes: Bytes,
) -> AppResult<Json<Value>> {
    let body = parse_upsert(&headers, &bytes)?;
    if !email::PROVIDER_TYPES.contains(&body.kind.as_str()) {
        return Err(AppError::bad_request(format!(
            "Invalid provider type: {}",
            body.kind
        )));
    }
    if let Some(msg) = email::validate_provider_config(&body.kind, &body.config) {
        return Err(AppError::bad_request(msg));
    }
    let inbox_id = scope(&state, body.inbox.as_deref()).await?;

    let id = db::new_id();
    let created_at = db::now();
    let config_text = serde_json::to_string(&body.config)?;
    let mut tx = state.db.begin().await?;
    match &inbox_id {
        Some(i) => {
            sqlx::query("DELETE FROM email_providers WHERE inbox_id = ?")
                .bind(i)
                .execute(&mut *tx)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM email_providers WHERE inbox_id IS NULL")
                .execute(&mut *tx)
                .await?;
        }
    }
    sqlx::query(
        "INSERT INTO email_providers (id, inbox_id, type, config, is_active, created_at) \
         VALUES (?, ?, ?, ?, 1, ?)",
    )
    .bind(&id)
    .bind(&inbox_id)
    .bind(&body.kind)
    .bind(&config_text)
    .bind(&created_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    // Python returns the stored config unredacted here.
    Ok(Json(json!({
        "data": row_json((id, inbox_id, body.kind, config_text, true, created_at), false)
    })))
}

async fn delete_provider(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Query(q): Query<InboxQuery>,
) -> AppResult<Response> {
    match scope(&state, q.inbox.as_deref()).await? {
        Some(id) => {
            sqlx::query("DELETE FROM email_providers WHERE inbox_id = ?")
                .bind(id)
                .execute(&state.db)
                .await?;
        }
        None => {
            sqlx::query("DELETE FROM email_providers WHERE inbox_id IS NULL")
                .execute(&state.db)
                .await?;
        }
    }
    Ok(super::envelope::no_content())
}
