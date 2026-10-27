//! Inbox CRUD (Python `routers/webhooks.py`, `/inboxes`): GET and POST (201)
//! `/v1/hooks/inboxes`, PATCH and DELETE (204) `/v1/hooks/inboxes/{slug}`.
//! All need the `webhooks` scope.

use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde_json::Value;

use super::envelope::{paginated, single};
use crate::auth::RequireWebhooks;
use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{Fields, Inbox, InboxResponse, Page, json_object};
use crate::security::is_safe_url;
use crate::state::AppState;

/// The optional text columns, in schema order (Python `WebhookInboxUpdate`).
const TEXT_FIELDS: &[&str] = &[
    "description",
    "forward_url",
    "notify_email",
    "email_subject_prefix",
    "sender_name",
    "turnstile_secret",
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/hooks/inboxes",
            get(list_inboxes)
                .post(create_inbox)
                .head(crate::error::method_not_allowed),
        )
        .route(
            "/v1/hooks/inboxes/{slug}",
            patch(update_inbox).delete(delete_inbox),
        )
}

async fn check_forward_url(url: Option<&str>) -> AppResult<()> {
    if let Some(url) = url.filter(|u| !u.is_empty())
        && let Err(reason) = is_safe_url(url).await
    {
        return Err(AppError::bad_request(format!(
            "Invalid forward_url: {reason}"
        )));
    }
    Ok(())
}

async fn list_inboxes(
    State(state): State<AppState>,
    _key: RequireWebhooks,
    RawQuery(query): RawQuery,
) -> AppResult<Json<Value>> {
    let page = Page::from_query(query.as_deref())?;
    let (total,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM webhook_inboxes")
        .fetch_one(&state.db)
        .await?;
    let rows: Vec<Inbox> = sqlx::query_as(&format!(
        "SELECT {} FROM webhook_inboxes ORDER BY created_at DESC LIMIT ? OFFSET ?",
        Inbox::COLUMNS
    ))
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<InboxResponse> = rows.iter().map(Inbox::response).collect();
    Ok(Json(paginated(&items, total, page.limit, page.offset)))
}

async fn create_inbox(
    State(state): State<AppState>,
    _key: RequireWebhooks,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<(StatusCode, Json<Value>)> {
    let obj = json_object(&headers, &body)?;
    let mut f = Fields::default();
    let slug = f.req_str(&obj, "slug");
    let values: Vec<Option<String>> = TEXT_FIELDS
        .iter()
        .map(|field| f.opt_str(&obj, field).flatten())
        .collect();
    f.finish()?;
    let slug = slug.unwrap_or_default();
    check_forward_url(values[1].as_deref()).await?;

    if Inbox::by_slug(&state.db, &slug).await?.is_some() {
        return Err(AppError::conflict("Inbox slug already exists"));
    }

    let id = db::new_id();
    let now = db::now();
    let mut q = sqlx::query(
        "INSERT INTO webhook_inboxes (id, slug, description, forward_url, notify_email, \
         email_subject_prefix, sender_name, turnstile_secret, is_active, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&slug);
    for v in &values {
        q = q.bind(v.clone());
    }
    q.bind(&now).bind(&now).execute(&state.db).await?;

    let inbox = Inbox::by_id(&state.db, &id)
        .await?
        .ok_or_else(|| AppError::internal("inbox vanished after insert"))?;
    Ok((StatusCode::CREATED, Json(single(&inbox.response()))))
}

async fn update_inbox(
    State(state): State<AppState>,
    _key: RequireWebhooks,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<Value>> {
    // FastAPI validates the body before the handler looks the inbox up.
    let obj = json_object(&headers, &body)?;
    let mut f = Fields::default();
    // Only the fields present in the body change (pydantic `exclude_unset`);
    // an explicit null clears a text field.
    let mut sets: Vec<(&str, Option<String>)> = Vec::new();
    for field in TEXT_FIELDS {
        if let Some(v) = f.opt_str(&obj, field) {
            sets.push((field, v));
        }
    }
    let is_active = f.opt_bool(&obj, "is_active").flatten();
    f.finish()?;

    let Some(inbox) = Inbox::by_slug(&state.db, &slug).await? else {
        return Err(AppError::not_found("Inbox not found"));
    };
    if let Some((_, url)) = sets.iter().find(|(k, _)| *k == "forward_url") {
        check_forward_url(url.as_deref()).await?;
    }

    if !sets.is_empty() || is_active.is_some() {
        let mut sql = String::from("UPDATE webhook_inboxes SET updated_at = ?");
        for (k, _) in &sets {
            sql.push_str(&format!(", {k} = ?"));
        }
        if is_active.is_some() {
            sql.push_str(", is_active = ?");
        }
        sql.push_str(" WHERE id = ?");
        let mut q = sqlx::query(&sql).bind(db::now());
        for (_, v) in &sets {
            q = q.bind(v.clone());
        }
        if let Some(a) = is_active {
            q = q.bind(a);
        }
        q.bind(&inbox.id).execute(&state.db).await?;
    }

    let inbox = Inbox::by_id(&state.db, &inbox.id)
        .await?
        .ok_or_else(|| AppError::not_found("Inbox not found"))?;
    Ok(Json(single(&inbox.response())))
}

/// Deletes the inbox; its events, channels and email provider go with it
/// (foreign keys with `ON DELETE CASCADE`).
async fn delete_inbox(
    State(state): State<AppState>,
    _key: RequireWebhooks,
    Path(slug): Path<String>,
) -> AppResult<axum::response::Response> {
    let result = sqlx::query("DELETE FROM webhook_inboxes WHERE slug = ?")
        .bind(&slug)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("Inbox not found"));
    }
    Ok(super::envelope::no_content())
}
