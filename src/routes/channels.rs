//! Channel CRUD (Python `routers/channels.py`).
//! POST (201) and GET `/v1/hooks/inboxes/{slug}/channels`, PATCH and
//! DELETE (204) `/v1/hooks/inboxes/{slug}/channels/{channel_id}`.
//! All need `RequireWebhooks`. Detection, validation and senders live in
//! `crate::channels`.
//!
//! Order of checks follows FastAPI: auth, then body validation (422), then
//! the handler (inbox 404, type and config 400).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::auth::RequireWebhooks;
use crate::channels::ChannelRow;
use crate::channels::detect::detect_channel_type;
use crate::channels::pyfmt::{last_chars, take_chars};
use crate::channels::validate::{
    is_valid_channel_type, suggest_channel_type, validate_channel_config,
};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::routes::envelope::single;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/hooks/inboxes/{slug}/channels",
            get(list_channels)
                .post(create_channel)
                .head(crate::error::method_not_allowed),
        )
        .route(
            "/v1/hooks/inboxes/{slug}/channels/{channel_id}",
            patch(update_channel).delete(delete_channel),
        )
}

/// Python `ChannelResponse`, fields in the same order.
#[derive(Debug, Serialize)]
pub struct ChannelOut {
    pub id: String,
    pub inbox_id: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub label: Option<String>,
    pub config: Map<String, Value>,
    pub is_active: bool,
    pub created_at: String,
}

impl ChannelOut {
    fn from_row(row: &ChannelRow) -> Self {
        ChannelOut {
            id: row.id.clone(),
            inbox_id: row.inbox_id.clone(),
            channel_type: row.channel_type.clone(),
            label: row.label.clone(),
            config: row.config_map(),
            is_active: row.is_active.unwrap_or(false),
            created_at: crate::models::api_timestamp(&row.created_at),
        }
    }
}

const SENSITIVE_KEYS: &[&str] = &[
    "webhook_url",
    "url",
    "bot_url",
    "api_key",
    "client_id",
    "client_secret",
    "refresh_token",
    "credentials_path",
    "token_path",
    "password",
];

/// Python `_redact_config`: sensitive strings longer than 8 characters keep
/// their first and last 4 characters.
pub fn redact_config(config: &Map<String, Value>) -> Map<String, Value> {
    config
        .iter()
        .map(|(k, v)| {
            let out = match v {
                Value::String(s)
                    if SENSITIVE_KEYS.contains(&k.as_str()) && s.chars().count() > 8 =>
                {
                    Value::String(format!("{}***{}", take_chars(s, 4), last_chars(s, 4)))
                }
                other => other.clone(),
            };
            (k.clone(), out)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Request bodies, validated the way pydantic reports them
// ---------------------------------------------------------------------------

fn err(kind: &str, loc: Value, msg: &str, input: &Value) -> Value {
    json!({"type": kind, "loc": loc, "msg": msg, "input": input})
}

/// Parse a JSON object body or return FastAPI's 422 for it.
fn parse_object(bytes: &Bytes) -> Result<Map<String, Value>, AppError> {
    if bytes.is_empty() {
        return Err(AppError::Validation(vec![err(
            "missing",
            json!(["body"]),
            "Field required",
            &Value::Null,
        )]));
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(other) => Err(AppError::Validation(vec![err(
            "model_attributes_type",
            json!(["body"]),
            "Input should be a valid dictionary or object to extract fields from",
            &other,
        )])),
        Err(e) => {
            // Position as a character offset, like Python's JSONDecodeError.pos.
            let text = String::from_utf8_lossy(bytes);
            let pos = text
                .lines()
                .take(e.line().saturating_sub(1))
                .map(|l| l.chars().count() + 1)
                .sum::<usize>()
                + e.column().saturating_sub(1);
            Err(AppError::Validation(vec![json!({
                "type": "json_invalid",
                "loc": ["body", pos],
                "msg": "JSON decode error",
                "input": {},
            })]))
        }
    }
}

enum Field<T> {
    Unset,
    Null,
    Set(T),
}

fn opt_str(body: &Map<String, Value>, key: &str, errors: &mut Vec<Value>) -> Field<String> {
    match body.get(key) {
        None => Field::Unset,
        Some(Value::Null) => Field::Null,
        Some(Value::String(s)) => Field::Set(s.clone()),
        Some(other) => {
            errors.push(err(
                "string_type",
                json!(["body", key]),
                "Input should be a valid string",
                other,
            ));
            Field::Unset
        }
    }
}

fn opt_dict(
    body: &Map<String, Value>,
    key: &str,
    errors: &mut Vec<Value>,
) -> Field<Map<String, Value>> {
    match body.get(key) {
        None => Field::Unset,
        Some(Value::Null) => Field::Null,
        Some(Value::Object(m)) => Field::Set(m.clone()),
        Some(other) => {
            errors.push(err(
                "dict_type",
                json!(["body", key]),
                "Input should be a valid dictionary",
                other,
            ));
            Field::Unset
        }
    }
}

/// Pydantic's lax bool: true/false, 0/1, and the usual strings.
fn opt_bool(body: &Map<String, Value>, key: &str, errors: &mut Vec<Value>) -> Field<bool> {
    let loc = json!(["body", key]);
    match body.get(key) {
        None => Field::Unset,
        Some(Value::Null) => Field::Null,
        Some(Value::Bool(b)) => Field::Set(*b),
        Some(v @ Value::Number(n)) => match n.as_f64() {
            Some(0.0) => Field::Set(false),
            Some(1.0) => Field::Set(true),
            _ => {
                errors.push(err(
                    "bool_parsing",
                    loc,
                    "Input should be a valid boolean, unable to interpret input",
                    v,
                ));
                Field::Unset
            }
        },
        Some(v @ Value::String(s)) => match s.to_ascii_lowercase().as_str() {
            "1" | "true" | "t" | "yes" | "y" | "on" => Field::Set(true),
            "0" | "false" | "f" | "no" | "n" | "off" => Field::Set(false),
            _ => {
                errors.push(err(
                    "bool_parsing",
                    loc,
                    "Input should be a valid boolean, unable to interpret input",
                    v,
                ));
                Field::Unset
            }
        },
        Some(other) => {
            errors.push(err(
                "bool_type",
                loc,
                "Input should be a valid boolean",
                other,
            ));
            Field::Unset
        }
    }
}

/// Python `ChannelCreate`.
struct ChannelCreate {
    channel_type: String,
    label: Option<String>,
    config: Map<String, Value>,
}

fn parse_create(bytes: &Bytes) -> Result<ChannelCreate, AppError> {
    let body = parse_object(bytes)?;
    let input = Value::Object(body.clone());
    let mut errors = Vec::new();
    let channel_type = match opt_str(&body, "type", &mut errors) {
        Field::Set(s) => Some(s),
        Field::Unset if !body.contains_key("type") => {
            errors.push(err(
                "missing",
                json!(["body", "type"]),
                "Field required",
                &input,
            ));
            None
        }
        Field::Null => {
            errors.push(err(
                "string_type",
                json!(["body", "type"]),
                "Input should be a valid string",
                &Value::Null,
            ));
            None
        }
        Field::Unset => None,
    };
    let label = match opt_str(&body, "label", &mut errors) {
        Field::Set(s) => Some(s),
        _ => None,
    };
    let config = match opt_dict(&body, "config", &mut errors) {
        Field::Set(m) => Some(m),
        Field::Unset if !body.contains_key("config") => {
            errors.push(err(
                "missing",
                json!(["body", "config"]),
                "Field required",
                &input,
            ));
            None
        }
        Field::Null => {
            errors.push(err(
                "dict_type",
                json!(["body", "config"]),
                "Input should be a valid dictionary",
                &Value::Null,
            ));
            None
        }
        Field::Unset => None,
    };
    match (channel_type, config) {
        (Some(channel_type), Some(config)) if errors.is_empty() => Ok(ChannelCreate {
            channel_type,
            label,
            config,
        }),
        _ => Err(AppError::Validation(errors)),
    }
}

/// Python `ChannelUpdate` with `exclude_unset=True`.
struct ChannelUpdate {
    channel_type: Field<String>,
    label: Field<String>,
    config: Field<Map<String, Value>>,
    is_active: Field<bool>,
}

fn parse_update(bytes: &Bytes) -> Result<ChannelUpdate, AppError> {
    let body = parse_object(bytes)?;
    let mut errors = Vec::new();
    let update = ChannelUpdate {
        channel_type: opt_str(&body, "type", &mut errors),
        label: opt_str(&body, "label", &mut errors),
        config: opt_dict(&body, "config", &mut errors),
        is_active: opt_bool(&body, "is_active", &mut errors),
    };
    if errors.is_empty() {
        Ok(update)
    } else {
        Err(AppError::Validation(errors))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn inbox_id(state: &AppState, slug: &str) -> AppResult<String> {
    let row: Option<(String,)> = sqlx::query_as("SELECT id FROM webhook_inboxes WHERE slug = ?")
        .bind(slug)
        .fetch_optional(&state.db)
        .await?;
    row.map(|r| r.0)
        .ok_or_else(|| AppError::not_found("Inbox not found"))
}

async fn load_channel(state: &AppState, inbox_id: &str, channel_id: &str) -> AppResult<ChannelRow> {
    let row: Option<ChannelRow> = sqlx::query_as(
        "SELECT id, inbox_id, type, label, config, is_active, created_at \
         FROM notification_channels WHERE id = ? AND inbox_id = ?",
    )
    .bind(channel_id)
    .bind(inbox_id)
    .fetch_optional(&state.db)
    .await?;
    row.ok_or_else(|| AppError::not_found("Channel not found"))
}

async fn create_channel(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    bytes: Bytes,
) -> AppResult<Response> {
    let body = parse_create(&bytes)?;
    let inbox_id = inbox_id(&state, &slug).await?;

    let mut channel_type = body.channel_type.as_str();
    if !is_valid_channel_type(channel_type) {
        let hint = suggest_channel_type(channel_type)
            .map(|s| format!(" Did you mean '{s}'?"))
            .unwrap_or_default();
        return Err(AppError::bad_request(format!(
            "Invalid channel type: {channel_type}.{hint}"
        )));
    }

    // Auto-detect the type of a generic webhook from `url` or `webhook_url`.
    if channel_type == "webhook" {
        let url = match body.config.get("url") {
            Some(v) if crate::channels::pyfmt::truthy(v) => Some(v),
            _ => body.config.get("webhook_url"),
        };
        if let Some(Value::String(url)) = url {
            channel_type = detect_channel_type(url);
        }
    }

    if let Some(message) = validate_channel_config(channel_type, &body.config) {
        return Err(AppError::bad_request(message));
    }

    let id = db::new_id();
    sqlx::query(
        "INSERT INTO notification_channels (id, inbox_id, type, label, config, is_active, created_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?)",
    )
    .bind(&id)
    .bind(&inbox_id)
    .bind(channel_type)
    .bind(&body.label)
    .bind(serde_json::to_string(&body.config)?)
    .bind(db::now())
    .execute(&state.db)
    .await?;

    let row = load_channel(&state, &inbox_id, &id).await?;
    Ok((
        StatusCode::CREATED,
        Json(single(&ChannelOut::from_row(&row))),
    )
        .into_response())
}

async fn list_channels(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> AppResult<Json<Value>> {
    let inbox_id = inbox_id(&state, &slug).await?;
    let rows: Vec<ChannelRow> = sqlx::query_as(
        "SELECT id, inbox_id, type, label, config, is_active, created_at \
         FROM notification_channels WHERE inbox_id = ? ORDER BY created_at DESC",
    )
    .bind(&inbox_id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<ChannelOut> = rows
        .iter()
        .map(|row| {
            let mut out = ChannelOut::from_row(row);
            out.config = redact_config(&out.config);
            out
        })
        .collect();
    Ok(Json(json!({ "data": items })))
}

async fn update_channel(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Path((slug, channel_id)): Path<(String, String)>,
    bytes: Bytes,
) -> AppResult<Json<Value>> {
    let update = parse_update(&bytes)?;
    let inbox_id = inbox_id(&state, &slug).await?;
    let mut row = load_channel(&state, &inbox_id, &channel_id).await?;

    match &update.channel_type {
        Field::Set(t) if !is_valid_channel_type(t) => {
            return Err(AppError::bad_request(format!("Invalid channel type: {t}")));
        }
        Field::Null => {
            return Err(AppError::bad_request("Invalid channel type: None"));
        }
        _ => {}
    }
    // Python writes these as NULL and then fails (NOT NULL column, or a
    // response that needs a bool) with a 500. Answer the 500 without writing.
    if matches!(update.config, Field::Null) || matches!(update.is_active, Field::Null) {
        return Err(AppError::internal(
            "channel update with null config or is_active",
        ));
    }

    if let Field::Set(t) = update.channel_type {
        row.channel_type = t;
    }
    match update.label {
        Field::Set(l) => row.label = Some(l),
        Field::Null => row.label = None,
        Field::Unset => {}
    }
    if let Field::Set(c) = update.config {
        row.config = serde_json::to_string(&c)?;
    }
    if let Field::Set(a) = update.is_active {
        row.is_active = Some(a);
    }

    sqlx::query(
        "UPDATE notification_channels SET type = ?, label = ?, config = ?, is_active = ? WHERE id = ?",
    )
    .bind(&row.channel_type)
    .bind(&row.label)
    .bind(&row.config)
    .bind(row.is_active)
    .bind(&row.id)
    .execute(&state.db)
    .await?;

    Ok(Json(single(&ChannelOut::from_row(&row))))
}

async fn delete_channel(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Path((slug, channel_id)): Path<(String, String)>,
) -> AppResult<axum::response::Response> {
    let inbox_id = inbox_id(&state, &slug).await?;
    let row = load_channel(&state, &inbox_id, &channel_id).await?;
    sqlx::query("DELETE FROM notification_channels WHERE id = ?")
        .bind(&row.id)
        .execute(&state.db)
        .await?;
    Ok(super::envelope::no_content())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_matches_python() {
        let c = json!({
            "webhook_url": "https://discord.com/api/webhooks/1/abc",
            "url": "short",
            "chat_id": "123456789012",
            "api_key": 123456789,
            "bot_url": "123456789"
        });
        let r = redact_config(c.as_object().unwrap());
        assert_eq!(
            Value::Object(r),
            json!({
                "webhook_url": "http***/abc",
                "url": "short",
                "chat_id": "123456789012",
                "api_key": 123456789,
                "bot_url": "1234***6789"
            })
        );
    }

    #[test]
    fn create_body_errors() {
        let e = parse_create(&Bytes::from_static(br#"{"type": 5, "label": 1}"#))
            .err()
            .unwrap();
        let AppError::Validation(d) = e else { panic!() };
        assert_eq!(
            d,
            vec![
                json!({"type": "string_type", "loc": ["body", "type"], "msg": "Input should be a valid string", "input": 5}),
                json!({"type": "string_type", "loc": ["body", "label"], "msg": "Input should be a valid string", "input": 1}),
                json!({"type": "missing", "loc": ["body", "config"], "msg": "Field required", "input": {"type": 5, "label": 1}}),
            ]
        );
        let AppError::Validation(d) = parse_create(&Bytes::new()).err().unwrap() else {
            panic!()
        };
        assert_eq!(d[0]["loc"], json!(["body"]));
        let AppError::Validation(d) = parse_create(&Bytes::from_static(b"{\"a\": }"))
            .err()
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(
            d[0],
            json!({"type": "json_invalid", "loc": ["body", 6], "msg": "JSON decode error", "input": {}})
        );
        let AppError::Validation(d) = parse_create(&Bytes::from_static(b"[1]")).err().unwrap()
        else {
            panic!()
        };
        assert_eq!(d[0]["type"], "model_attributes_type");
    }

    #[test]
    fn update_body_bools() {
        let u = parse_update(&Bytes::from_static(br#"{"is_active": "off"}"#))
            .ok()
            .unwrap();
        assert!(matches!(u.is_active, Field::Set(false)));
        let AppError::Validation(d) = parse_update(&Bytes::from_static(
            br#"{"is_active": "maybe", "config": []}"#,
        ))
        .err()
        .unwrap() else {
            panic!()
        };
        assert_eq!(d[0]["type"], "dict_type");
        assert_eq!(d[1]["type"], "bool_parsing");
    }
}
