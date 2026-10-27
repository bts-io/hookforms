//! Public webhook receiver `/hooks/{slug}` (Python `routers/webhooks.py`,
//! `receive_webhook`). Methods GET, POST, PUT, PATCH and DELETE; HEAD gets
//! Starlette's 405 like every FastAPI route (axum would answer it with GET).
//!
//! Order of work, as in Python: find the active inbox (404), parse the body
//! (`body.rs`), check Turnstile, store the event, then, when the body is
//! truthy, hand it to the inbox's active channels (`crate::channels::dispatch`)
//! or, for inboxes without channels, the legacy `forward_url` (`legacy.rs`) and
//! `notify_email` (`crate::email::notify`, which applies the limit of 10 per
//! inbox per 10 minutes).
//! Notifications run in the background and never change the answer.

pub mod body;
pub mod legacy;
#[cfg(test)]
mod tests;

use std::net::SocketAddr;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::{delete, get, head, patch, post, put};
use serde_json::{Map, Value, json};

use crate::error::{self, AppError, AppResult};
use crate::middleware::MAX_BODY_SIZE;
use crate::models::{Event, Inbox};
use crate::state::AppState;

const TURNSTILE_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const TURNSTILE_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
tokio::task_local! {
    /// Lets tests point the Turnstile check at a local mock server.
    pub(crate) static TURNSTILE_URL_OVERRIDE: String;
}

fn turnstile_url() -> String {
    #[cfg(test)]
    if let Ok(url) = TURNSTILE_URL_OVERRIDE.try_with(Clone::clone) {
        return url;
    }
    if let Some(base) = crate::security::test_upstream_base() {
        return format!("{base}/turnstile/v0/siteverify");
    }
    TURNSTILE_URL.to_string()
}

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/hooks/{slug}",
        get(receive)
            .merge(post(receive))
            .merge(put(receive))
            .merge(patch(receive))
            .merge(delete(receive))
            .merge(head(error::method_not_allowed)),
    )
}

/// Find an inbox by slug. `active_only` adds Python's `is_active IS TRUE`.
pub async fn find_inbox(state: &AppState, slug: &str, active_only: bool) -> AppResult<Inbox> {
    let sql = if active_only {
        "SELECT * FROM webhook_inboxes WHERE slug = ? AND is_active = 1"
    } else {
        "SELECT * FROM webhook_inboxes WHERE slug = ?"
    };
    sqlx::query_as::<_, Inbox>(sql)
        .bind(slug)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::not_found("Inbox not found"))
}

/// Header values as Starlette decodes them (latin-1).
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// `dict(request.headers)`: lowercase names in arrival order, first value of a repeated name.
fn headers_dict(headers: &HeaderMap) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, value) in headers {
        if !out.contains_key(name.as_str()) {
            out.insert(
                name.as_str().to_string(),
                Value::String(latin1(value.as_bytes())),
            );
        }
    }
    out
}

/// `dict(request.query_params)`: last value of a repeated key, first key's position.
pub(crate) fn query_dict(query: Option<&str>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, v) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        out.insert(k.into_owned(), Value::String(v.into_owned()));
    }
    out
}

/// Turnstile scalar as httpx form-encodes it.
fn form_scalar(v: &Value) -> AppResult<String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.to_string()),
        // httpx refuses containers in form data: an unhandled error in Python.
        other => Err(AppError::internal(format!(
            "unsupported Turnstile token type: {other}"
        ))),
    }
}

async fn verify_turnstile(
    state: &AppState,
    secret: &str,
    token: &Value,
    headers: &HeaderMap,
) -> AppResult<()> {
    let remoteip = headers
        .get("cf-connecting-ip")
        .map(|v| latin1(v.as_bytes()))
        .unwrap_or_default();
    let form = [
        ("secret", secret.to_string()),
        ("response", form_scalar(token)?),
        ("remoteip", remoteip),
    ];
    let unavailable = || {
        AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Turnstile verification unavailable",
        )
    };
    let resp = state
        .http
        .post(turnstile_url())
        .form(&form)
        .timeout(TURNSTILE_TIMEOUT)
        .send()
        .await
        .map_err(|_| unavailable())?;
    let bytes = resp.bytes().await.map_err(|_| unavailable())?;
    // A body that is not a JSON object fails in Python outside the httpx
    // error handler, so it becomes a 500.
    let result: Value = serde_json::from_slice(&bytes)?;
    let Value::Object(result) = result else {
        return Err(AppError::internal("Turnstile answer is not an object"));
    };
    if result.get("success").is_some_and(body::truthy) {
        Ok(())
    } else {
        Err(AppError::forbidden("Turnstile verification failed"))
    }
}

async fn receive(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    req: Request,
) -> AppResult<Json<Value>> {
    let inbox = find_inbox(&state, &slug, true).await?;

    let (parts, req_body) = req.into_parts();
    let content_type = parts
        .headers
        .get("content-type")
        .map(|v| latin1(v.as_bytes()))
        .unwrap_or_default();
    // The size middleware only sees Content-Length; this also caps chunked bodies.
    let raw = axum::body::to_bytes(req_body, MAX_BODY_SIZE as usize)
        .await
        .map_err(|_| {
            AppError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                format!(
                    "Request body too large. Max size is {} MB.",
                    MAX_BODY_SIZE / (1024 * 1024)
                ),
            )
        })?;
    let mut parsed = body::parse(&content_type, &raw).await;

    if let Some(secret) = inbox.turnstile_secret.as_deref().filter(|s| !s.is_empty())
        && let Some(Value::Object(map)) = parsed.as_mut().filter(|b| body::truthy(b))
    {
        let token = map.shift_remove("cf-turnstile-response");
        match token.filter(body::truthy) {
            None => {
                return Err(AppError::bad_request(
                    "Missing Turnstile verification token",
                ));
            }
            Some(token) => verify_turnstile(&state, secret, &token, &parts.headers).await?,
        }
    }

    let event = Event {
        id: crate::db::new_id(),
        inbox_id: inbox.id.clone(),
        method: parts.method.as_str().to_string(),
        headers: headers_dict(&parts.headers),
        body: parsed,
        query_params: query_dict(parts.uri.query()),
        source_ip: parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string()),
        received_at: crate::db::now(),
    };
    sqlx::query(
        "INSERT INTO webhook_events \
         (id, inbox_id, method, headers, body, query_params, source_ip, received_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&event.id)
    .bind(&event.inbox_id)
    .bind(&event.method)
    .bind(serde_json::to_string(&event.headers)?)
    .bind(event.body.as_ref().map(serde_json::to_string).transpose()?)
    .bind(serde_json::to_string(&event.query_params)?)
    .bind(&event.source_ip)
    .bind(&event.received_at)
    .execute(&state.db)
    .await?;

    if event.body.as_ref().is_some_and(body::truthy) {
        notify(&state, &inbox, &event, parts.method).await?;
    }

    Ok(Json(json!({ "status": "received", "event_id": event.id })))
}

/// Channels when the inbox has any active ones, else the legacy fields.
async fn notify(state: &AppState, inbox: &Inbox, event: &Event, method: Method) -> AppResult<()> {
    if crate::channels::has_active_channels(state, &inbox.id).await? {
        crate::channels::dispatch(state, inbox, event).await;
        return Ok(());
    }

    if inbox.forward_url.as_deref().is_some_and(|u| !u.is_empty())
        && let Some(body) = event.body.clone()
    {
        tokio::spawn(legacy::forward(
            state.http.clone(),
            inbox.clone(),
            method,
            body,
        ));
    }

    // `email::notify` applies the `webhook_email_rate:{inbox_id}` limit itself
    // (10 per inbox per 600 s), so it is counted once per event, as in Python.
    if inbox.notify_email.as_deref().is_some_and(|e| !e.is_empty()) {
        crate::email::notify(state, inbox, event).await;
    }
    Ok(())
}
