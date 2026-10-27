//! Email notifications (Python `app/providers/`, `app/mail.py`, the email part
//! of `channels/dispatcher.py` and the legacy `notify_email` block in
//! `routers/webhooks.py`).
//!
//! - `providers`: Gmail, Resend, SendGrid, SMTP senders.
//! - `render`: the notification email (subject, designed HTML, plain text),
//!   shared with the email channel.
//! - `resolve_provider`: inbox provider, then the global default, then the
//!   env Gmail token file.
//! - `notify`: the intake hook (step 3) for inboxes without channels.
//!
//! The email channel type lives in `channels::email` and uses
//! `resolve_provider` from here.
//!
//! The legacy rate limit uses `AppState::counters` with the Python Redis key
//! name `webhook_email_rate:{inbox_id}`, 10 emails per inbox per 10 minutes.

pub mod providers;
pub mod render;

use std::time::Duration;

use serde_json::{Map, Value};

use crate::state::AppState;
pub use providers::Provider;
use render::truthy;

pub const PROVIDER_TYPES: &[&str] = &["gmail", "resend", "sendgrid", "smtp"];
const EMAIL_RATE_LIMIT: u64 = 10;
const EMAIL_RATE_WINDOW: Duration = Duration::from_secs(600);

/// What the email code needs from an inbox row. The `Inbox` model (step 2)
/// implements this, so `notify(&state, &inbox, &event)` works unchanged.
pub trait NotifyInbox {
    fn id(&self) -> &str;
    fn slug(&self) -> &str;
    fn notify_email(&self) -> Option<&str>;
    fn email_subject_prefix(&self) -> Option<&str>;
    fn sender_name(&self) -> Option<&str>;
}

/// What the email code needs from an event row (step 3's `Event`): its
/// decoded body, `None` when the request had none.
pub trait NotifyEvent {
    fn body(&self) -> Option<&Value>;
}

/// Owned inbox fields, used when the work moves into a spawned task.
#[derive(Debug, Clone, Default)]
pub struct InboxInfo {
    pub id: String,
    pub slug: String,
    pub notify_email: Option<String>,
    pub email_subject_prefix: Option<String>,
    pub sender_name: Option<String>,
}

impl InboxInfo {
    pub fn from_inbox<I: NotifyInbox + ?Sized>(inbox: &I) -> Self {
        InboxInfo {
            id: inbox.id().to_string(),
            slug: inbox.slug().to_string(),
            notify_email: inbox.notify_email().map(String::from),
            email_subject_prefix: inbox.email_subject_prefix().map(String::from),
            sender_name: inbox.sender_name().map(String::from),
        }
    }
}

impl NotifyInbox for InboxInfo {
    fn id(&self) -> &str {
        &self.id
    }
    fn slug(&self) -> &str {
        &self.slug
    }
    fn notify_email(&self) -> Option<&str> {
        self.notify_email.as_deref()
    }
    fn email_subject_prefix(&self) -> Option<&str> {
        self.email_subject_prefix.as_deref()
    }
    fn sender_name(&self) -> Option<&str> {
        self.sender_name.as_deref()
    }
}

impl NotifyEvent for Value {
    fn body(&self) -> Option<&Value> {
        Some(self)
    }
}

impl NotifyEvent for Option<Value> {
    fn body(&self) -> Option<&Value> {
        self.as_ref()
    }
}

/// Load the provider for an inbox (Python `resolve_email_provider`).
///
/// Order: the inbox's own active row, the active global row (`inbox_id IS
/// NULL`), the env Gmail token file, else `None`. A stored row that cannot be
/// built (bad Gmail path, missing key) is an error, as in Python.
pub async fn resolve_provider(
    state: &AppState,
    inbox_id: &str,
) -> Result<Option<Provider>, String> {
    let specific: Option<(String, String)> = sqlx::query_as(
        "SELECT type, config FROM email_providers WHERE inbox_id = ? AND is_active = 1 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(inbox_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| format!("database: {e}"))?;
    let row = match specific {
        Some(r) => Some(r),
        None => sqlx::query_as(
            "SELECT type, config FROM email_providers WHERE inbox_id IS NULL AND is_active = 1 \
             ORDER BY created_at DESC LIMIT 1",
        )
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("database: {e}"))?,
    };
    if let Some((kind, config)) = row {
        let config: Map<String, Value> = serde_json::from_str(&config).unwrap_or_default();
        return Provider::from_record(&kind, &config).map(Some);
    }
    Ok(providers::GmailProvider::from_settings(
        &state.config.gmail_token_path,
        &state.config.gmail_sender_email,
    )
    .map(Provider::Gmail))
}

/// True when the env Gmail fallback is usable (token file plus sender), the
/// check behind `meta.fallback = "env_gmail"`.
pub fn has_env_gmail(state: &AppState) -> bool {
    std::path::Path::new(&state.config.gmail_token_path).exists()
        && !state.config.gmail_sender_email.is_empty()
}

/// Python `validate_provider_config`: `None` when valid, else the message.
pub fn validate_provider_config(kind: &str, config: &Map<String, Value>) -> Option<String> {
    let fields: &[&str] = match kind {
        "gmail" => &["credentials_path", "token_path", "sender_email"],
        "resend" | "sendgrid" => &["api_key", "from_email"],
        "smtp" => &["host", "port", "from_email"],
        other => return Some(format!("Unknown provider type: {other}")),
    };
    fields
        .iter()
        .find(|f| !config.get(**f).is_some_and(truthy))
        .map(|f| format!("Missing required field: {f}"))
}

/// Python `_redact_config`, shared with the channel routes (one function in Python too).
pub use crate::routes::channels::redact_config;

/// `INCR` the per-inbox email counter; true when this email is over the limit.
fn over_rate_limit(state: &AppState, key: &str) -> bool {
    state.counters.incr(key, EMAIL_RATE_WINDOW) > EMAIL_RATE_LIMIT
}

/// Intake hook for the legacy `notify_email` field. Fire and forget: the work
/// runs in a spawned task and never fails the intake.
pub async fn notify<I, E>(state: &AppState, inbox: &I, event: &E)
where
    I: NotifyInbox + ?Sized,
    E: NotifyEvent + ?Sized,
{
    let Some(body) = event.body().filter(|b| truthy(b)).cloned() else {
        return;
    };
    if inbox.notify_email().is_none_or(str::is_empty) {
        return;
    }
    let state = state.clone();
    let info = InboxInfo::from_inbox(inbox);
    tokio::spawn(async move {
        deliver_legacy(&state, &info, &body).await;
    });
}

/// The awaited body of `notify`. Returns how many emails went out.
///
/// Like Python, it does nothing when the inbox has active notification
/// channels (the dispatcher handles email then), counts the rate limit before
/// rendering, and sends to each address in turn, stopping at the first failure.
pub async fn deliver_legacy(state: &AppState, inbox: &InboxInfo, body: &Value) -> usize {
    if !truthy(body) {
        return 0;
    }
    let Some(notify_email) = inbox.notify_email.as_deref().filter(|s| !s.is_empty()) else {
        return 0;
    };
    let channels: Result<(i64,), _> = sqlx::query_as(
        "SELECT COUNT(*) FROM notification_channels WHERE inbox_id = ? AND is_active = 1",
    )
    .bind(&inbox.id)
    .fetch_one(&state.db)
    .await;
    match channels {
        Ok((n,)) if n > 0 => return 0,
        Ok(_) => {}
        Err(e) => {
            tracing::error!(slug = %inbox.slug, error = %e, "Email notification failed");
            return 0;
        }
    }

    if over_rate_limit(state, &format!("webhook_email_rate:{}", inbox.id)) {
        tracing::warn!(slug = %inbox.slug, "Email rate limit hit for inbox");
        return 0;
    }

    let Some(fields) = body.as_object() else {
        tracing::error!(slug = %inbox.slug, "Email notification failed: body is not an object");
        return 0;
    };
    let rendered = render::render_legacy(
        &inbox.slug,
        inbox.email_subject_prefix.as_deref(),
        inbox.sender_name.as_deref(),
        fields,
    );
    let provider = match resolve_provider(state, &inbox.id).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            tracing::error!(slug = %inbox.slug, "Email notification failed: no email provider configured");
            return 0;
        }
        Err(e) => {
            tracing::error!(slug = %inbox.slug, error = %e, "Email notification failed");
            return 0;
        }
    };

    let mut sent = 0;
    for to in notify_email
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        match provider
            .send(
                &state.http,
                to,
                &rendered.subject,
                &rendered.html,
                &rendered.text,
                inbox.sender_name.as_deref(),
            )
            .await
        {
            Ok(()) => sent += 1,
            Err(e) => {
                tracing::error!(slug = %inbox.slug, error = %e, "Email notification failed");
                break;
            }
        }
    }
    sent
}

impl NotifyInbox for crate::models::Inbox {
    fn id(&self) -> &str {
        &self.id
    }
    fn slug(&self) -> &str {
        &self.slug
    }
    fn notify_email(&self) -> Option<&str> {
        self.notify_email.as_deref()
    }
    fn email_subject_prefix(&self) -> Option<&str> {
        self.email_subject_prefix.as_deref()
    }
    fn sender_name(&self) -> Option<&str> {
        self.sender_name.as_deref()
    }
}

impl NotifyEvent for crate::models::Event {
    fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn map(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn provider_validation_matches_python() {
        assert_eq!(
            validate_provider_config(
                "smtp",
                &map(json!({"host": "h", "port": 0, "from_email": "a"}))
            ),
            Some("Missing required field: port".into())
        );
        assert_eq!(
            validate_provider_config("resend", &map(json!({"api_key": "k"}))),
            Some("Missing required field: from_email".into())
        );
        assert_eq!(
            validate_provider_config(
                "gmail",
                &map(json!({"credentials_path": "c", "token_path": "t", "sender_email": "s"}))
            ),
            None
        );
        assert_eq!(
            validate_provider_config("x", &Map::new()),
            Some("Unknown provider type: x".into())
        );
    }

    #[test]
    fn redaction_matches_python() {
        let r = redact_config(&map(json!({
            "api_key": "re_1234567890",
            "password": "short",
            "from_email": "someone@example.com",
            "port": 587
        })));
        assert_eq!(
            Value::Object(r),
            json!({"api_key": "re_1***7890", "password": "short", "from_email": "someone@example.com", "port": 587})
        );
    }
}
