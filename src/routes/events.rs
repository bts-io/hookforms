//! Events of an inbox (Python `routers/webhooks.py`): GET
//! `/v1/hooks/{slug}/events` (paginated, newest first) and GET
//! `/v1/hooks/{slug}/events/{event_id}`. Both need the `webhooks` scope and,
//! unlike the receiver, find inactive inboxes too.
//!
//! Starlette tries routes in order, so in Python `/v1/hooks/inboxes/events`
//! is the events list of an inbox named `inboxes` (the inbox routes only
//! match it for PATCH and DELETE). axum prefers the static `inboxes` segment,
//! so this file routes those paths itself and hands the other methods to the
//! inbox router.

use axum::Json;
use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::Uri;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::auth::RequireWebhooks;
use crate::error::{self, AppError, AppResult};
use crate::models::Event;
use crate::routes::envelope::{paginated, single};
use crate::routes::hooks::find_inbox;
use crate::state::AppState;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/hooks/{slug}/events",
            get(list_events).head(error::method_not_allowed),
        )
        .route(
            "/v1/hooks/{slug}/events/{event_id}",
            get(get_event).head(error::method_not_allowed),
        )
        .route(
            "/v1/hooks/inboxes/events",
            get(list_inboxes_events)
                .head(error::method_not_allowed)
                .fallback(to_inbox_routes),
        )
        .route(
            "/v1/hooks/inboxes/events/{event_id}",
            get(get_inboxes_event).head(error::method_not_allowed),
        )
}

/// PATCH or DELETE `/v1/hooks/inboxes/events` update or delete the inbox
/// named `events`, as in Python; any other method gets 405.
async fn to_inbox_routes(State(state): State<AppState>, req: Request) -> Response {
    crate::routes::inboxes::router()
        .method_not_allowed_fallback(error::method_not_allowed)
        .fallback(error::not_found)
        .with_state(state)
        .oneshot(req)
        .await
        .into_response()
}

/// FastAPI `int` query parameter in pydantic's lax mode: surrounding
/// whitespace, a sign, `_` between digits and a zero fraction are accepted.
fn parse_int(raw: &str) -> Option<i64> {
    let s = raw.trim();
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits = match digits.split_once('.') {
        Some((int, frac)) if frac.bytes().all(|b| b == b'0') => int,
        Some(_) => return None,
        None => digits,
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut n: i64 = 0;
    for b in digits.bytes() {
        match b {
            b'_' => {}
            b'0'..=b'9' => n = n.saturating_mul(10).saturating_add((b - b'0') as i64),
            _ => return None,
        }
    }
    Some(if neg { -n } else { n })
}

/// Python `limit: int = Query(50, le=200), offset: int = Query(0)`.
fn pagination(uri: &Uri) -> AppResult<(i64, i64)> {
    let query = crate::routes::hooks::query_dict(uri.query());
    let mut details = Vec::new();
    let mut read = |name: &str, default: i64, max: Option<i64>| -> i64 {
        let Some(Value::String(raw)) = query.get(name) else {
            return default;
        };
        match parse_int(raw) {
            None => {
                details.push(json!({
                    "type": "int_parsing",
                    "loc": ["query", name],
                    "msg": "Input should be a valid integer, unable to parse string as an integer",
                    "input": raw,
                }));
                default
            }
            Some(n) if max.is_some_and(|m| n > m) => {
                details.push(json!({
                    "type": "less_than_equal",
                    "loc": ["query", name],
                    "msg": format!("Input should be less than or equal to {}", max.unwrap_or(0)),
                    "input": raw,
                }));
                default
            }
            Some(n) => n,
        }
    };
    let limit = read("limit", DEFAULT_LIMIT, Some(MAX_LIMIT));
    let offset = read("offset", 0, None);
    if !details.is_empty() {
        return Err(AppError::Validation(details));
    }
    // Postgres refuses a negative LIMIT or OFFSET, which Python answers with a 500.
    if limit < 0 || offset < 0 {
        return Err(AppError::internal("LIMIT and OFFSET must not be negative"));
    }
    Ok((limit, offset))
}

async fn list(state: &AppState, slug: &str, uri: &Uri) -> AppResult<Json<Value>> {
    let (limit, offset) = pagination(uri)?;
    let inbox = find_inbox(state, slug, false).await?;
    let (total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM webhook_events WHERE inbox_id = ?")
        .bind(&inbox.id)
        .fetch_one(&state.db)
        .await?;
    let events: Vec<Event> = sqlx::query_as(
        "SELECT * FROM webhook_events WHERE inbox_id = ? \
         ORDER BY received_at DESC, rowid DESC LIMIT ? OFFSET ?",
    )
    .bind(&inbox.id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<_> = events.iter().map(Event::response).collect();
    Ok(Json(paginated(&items, total, limit, offset)))
}

async fn get_one(state: &AppState, slug: &str, event_id: &str) -> AppResult<Json<Value>> {
    let inbox = find_inbox(state, slug, false).await?;
    // The id column is a UUID in Postgres: any accepted spelling matches, and
    // a value that is not a UUID fails in the database (a 500 in Python).
    let id = uuid::Uuid::parse_str(event_id)
        .map_err(|_| AppError::internal(format!("invalid UUID: {event_id}")))?;
    let event: Option<Event> =
        sqlx::query_as("SELECT * FROM webhook_events WHERE id = ? AND inbox_id = ?")
            .bind(id.to_string())
            .bind(&inbox.id)
            .fetch_optional(&state.db)
            .await?;
    let event = event.ok_or_else(|| AppError::not_found("Event not found"))?;
    Ok(Json(single(&event.response())))
}

async fn list_events(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Path(slug): Path<String>,
    uri: Uri,
) -> AppResult<Json<Value>> {
    list(&state, &slug, &uri).await
}

async fn get_event(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Path((slug, event_id)): Path<(String, String)>,
) -> AppResult<Json<Value>> {
    get_one(&state, &slug, &event_id).await
}

async fn list_inboxes_events(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    uri: Uri,
) -> AppResult<Json<Value>> {
    list(&state, "inboxes", &uri).await
}

async fn get_inboxes_event(
    _key: RequireWebhooks,
    State(state): State<AppState>,
    Path(event_id): Path<String>,
) -> AppResult<Json<Value>> {
    get_one(&state, "inboxes", &event_id).await
}
