//! `/v1/auth/keys` (Python `routers/auth.py`): POST (201) create, GET list,
//! DELETE `/{key_id}` (204) revoke. All need the `admin` scope.

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use super::envelope::{paginated, single};
use crate::auth::{RequireAdmin, generate_key, hash_key, key_prefix};
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{Fields, Page, api_timestamp, json_object, verr};
use crate::state::AppState;

const VALID_SCOPES: &[&str] = &["admin", "webhooks"];
const NAME_MAX: usize = 100;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/auth/keys",
            get(list_keys)
                .post(create_key)
                .head(crate::error::method_not_allowed),
        )
        .route("/v1/auth/keys/{key_id}", delete(revoke_key))
}

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: String,
    name: String,
    scopes: Option<String>,
    is_active: Option<bool>,
    created_at: String,
    last_used_at: Option<String>,
}

/// Python `ApiKeyResponse` (and `ApiKeyCreated` with `raw_key`).
#[derive(Serialize)]
struct KeyResponse {
    id: String,
    name: String,
    scopes: Vec<String>,
    is_active: bool,
    created_at: String,
    last_used_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_key: Option<String>,
}

impl From<KeyRow> for KeyResponse {
    fn from(r: KeyRow) -> Self {
        KeyResponse {
            id: r.id,
            name: r.name,
            scopes: r
                .scopes
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default(),
            is_active: r.is_active.unwrap_or(true),
            created_at: api_timestamp(&r.created_at),
            last_used_at: r.last_used_at.as_deref().map(api_timestamp),
            raw_key: None,
        }
    }
}

const SELECT: &str = "SELECT id, name, scopes, is_active, created_at, last_used_at FROM api_keys";

/// Python `ApiKeyCreate`: `name` (str, at most 100 characters) and `scopes`
/// (list of str, each one of `webhooks`, `admin`; default empty).
fn parse_create(headers: &HeaderMap, body: &[u8]) -> AppResult<(String, Vec<String>)> {
    let obj = json_object(headers, body)?;
    let mut f = Fields::default();
    let name = f.req_str(&obj, "name");
    if let Some(n) = &name
        && n.chars().count() > NAME_MAX
    {
        f.errors.push(verr(
            "string_too_long",
            json!(["body", "name"]),
            format!("String should have at most {NAME_MAX} characters"),
            Value::String(n.clone()),
        ));
    }
    let mut scopes = Vec::new();
    match obj.get("scopes") {
        None => {}
        Some(Value::Array(items)) => {
            let mut item_ok = true;
            for (i, item) in items.iter().enumerate() {
                match item {
                    Value::String(s) => scopes.push(s.clone()),
                    other => {
                        item_ok = false;
                        f.errors.push(verr(
                            "string_type",
                            json!(["body", "scopes", i]),
                            "Input should be a valid string",
                            other.clone(),
                        ));
                    }
                }
            }
            if item_ok {
                let mut invalid: Vec<&str> = scopes
                    .iter()
                    .map(String::as_str)
                    .filter(|s| !VALID_SCOPES.contains(s))
                    .collect();
                invalid.sort_unstable();
                invalid.dedup();
                if !invalid.is_empty() {
                    f.errors.push(verr(
                        "value_error",
                        json!(["body", "scopes"]),
                        format!(
                            "Value error, Invalid scopes: {}. Valid: {}",
                            invalid.join(", "),
                            VALID_SCOPES.join(", ")
                        ),
                        Value::Array(items.clone()),
                    ));
                }
            }
        }
        Some(other) => f.errors.push(verr(
            "list_type",
            json!(["body", "scopes"]),
            "Input should be a valid list",
            other.clone(),
        )),
    }
    f.finish()?;
    Ok((name.unwrap_or_default(), scopes))
}

async fn create_key(
    State(state): State<AppState>,
    _key: RequireAdmin,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<(StatusCode, Json<Value>)> {
    let (name, scopes) = parse_create(&headers, &body)?;
    let raw_key = generate_key();
    let to_hash = raw_key.clone();
    let key_hash = tokio::task::spawn_blocking(move || hash_key(&to_hash))
        .await
        .map_err(AppError::internal)?;
    let id = db::new_id();
    let now = db::now();
    sqlx::query(
        "INSERT INTO api_keys (id, name, key_hash, key_prefix, scopes, is_active, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&name)
    .bind(&key_hash)
    .bind(key_prefix(&raw_key))
    .bind(serde_json::to_string(&scopes)?)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;
    let row: KeyRow = sqlx::query_as(&format!("{SELECT} WHERE id = ?"))
        .bind(&id)
        .fetch_one(&state.db)
        .await?;
    let mut resp = KeyResponse::from(row);
    resp.raw_key = Some(raw_key);
    Ok((StatusCode::CREATED, Json(single(&resp))))
}

async fn list_keys(
    State(state): State<AppState>,
    _key: RequireAdmin,
    RawQuery(query): RawQuery,
) -> AppResult<Json<Value>> {
    let page = Page::from_query(query.as_deref())?;
    let (total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM api_keys")
        .fetch_one(&state.db)
        .await?;
    let rows: Vec<KeyRow> = sqlx::query_as(&format!(
        "{SELECT} ORDER BY created_at DESC LIMIT ? OFFSET ?"
    ))
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<KeyResponse> = rows.into_iter().map(KeyResponse::from).collect();
    Ok(Json(paginated(&items, total, page.limit, page.offset)))
}

/// Soft revoke: the row stays with `is_active = false`.
async fn revoke_key(
    State(state): State<AppState>,
    _key: RequireAdmin,
    Path(key_id): Path<String>,
) -> AppResult<axum::response::Response> {
    // Python compared against a UUID column, so any spelling of the UUID matched
    // and a non-UUID failed; ids are stored lowercase hyphenated here.
    let id = uuid::Uuid::parse_str(&key_id)
        .map_err(|_| AppError::not_found("API key not found"))?
        .to_string();
    let result = sqlx::query("UPDATE api_keys SET is_active = 0, updated_at = ? WHERE id = ?")
        .bind(db::now())
        .bind(&id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("API key not found"));
    }
    Ok(super::envelope::no_content())
}
