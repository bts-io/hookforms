//! Notification channels (Python `app/channels/`).
//!
//! - `detect`: channel type from a URL
//! - `validate`: config checks with Python's messages
//! - `format_value`, `pyfmt`: Python-identical value and JSON text
//! - `senders`: Discord, Slack, Teams, Telegram, ntfy and generic webhook payloads
//! - `email`: the email channel type (HTML, rate limit, provider hook)
//!
//! Entry point for intake (step 3): [`dispatch`], fire-and-forget. All
//! outgoing requests use `AppState::http` (the SSRF-safe client) plus
//! `security::check_target` for IP literals.

pub mod detect;
pub mod email;
pub mod format_value;
pub mod pyfmt;
pub mod senders;
pub mod validate;

use std::time::Duration;

use axum::http::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Map, Value};

use crate::security;
use crate::state::AppState;

/// Body keys no channel shows (Turnstile token and internal fields).
pub const SKIP_KEYS: &[&str] = &["cf-turnstile-response", "raw", "source"];

/// Python sends channel notifications with a 10 s timeout.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Python `ChannelContext`.
#[derive(Debug, Clone)]
pub struct ChannelContext {
    pub slug: String,
    pub subject_prefix: String,
    pub sender_name: String,
    pub body: Map<String, Value>,
    /// Discord embed timestamp, Python `datetime.now(timezone.utc).isoformat()`.
    pub timestamp: String,
}

impl ChannelContext {
    pub fn new(inbox: &InboxInfo, body: Map<String, Value>) -> Self {
        let non_empty =
            |v: &Option<String>| v.as_deref().filter(|s| !s.is_empty()).map(String::from);
        ChannelContext {
            slug: inbox.slug.clone(),
            subject_prefix: non_empty(&inbox.email_subject_prefix)
                .unwrap_or_else(|| format!("[{}]", inbox.slug)),
            sender_name: non_empty(&inbox.sender_name).unwrap_or_else(|| "HookForms".into()),
            body,
            timestamp: crate::routes::root::python_isoformat(chrono::Utc::now()),
        }
    }

    pub fn forwarded_from(&self) -> String {
        format!("hookforms/hooks/{}", self.slug)
    }
}

/// Python `ChannelPayload`; every channel request is a POST.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelPayload {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The inbox fields the dispatcher reads.
#[derive(Debug, Clone)]
pub struct InboxInfo {
    pub id: String,
    pub slug: String,
    pub email_subject_prefix: Option<String>,
    pub sender_name: Option<String>,
}

/// Implemented for step 2's `Inbox` row so intake can pass it straight in.
pub trait InboxView {
    fn info(&self) -> InboxInfo;
}

/// Implemented for step 3's `Event` row: the stored (parsed) body.
pub trait EventView {
    fn body(&self) -> Option<&Value>;
}

impl InboxView for InboxInfo {
    fn info(&self) -> InboxInfo {
        self.clone()
    }
}

impl EventView for Value {
    fn body(&self) -> Option<&Value> {
        Some(self)
    }
}

/// A `notification_channels` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChannelRow {
    pub id: String,
    pub inbox_id: String,
    #[sqlx(rename = "type")]
    pub channel_type: String,
    pub label: Option<String>,
    pub config: String,
    pub is_active: Option<bool>,
    pub created_at: String,
}

impl ChannelRow {
    pub fn config_map(&self) -> Map<String, Value> {
        match serde_json::from_str(&self.config) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        }
    }
}

/// Intake asks this to choose between channels and the legacy
/// `forward_url` / `notify_email` path (Python counts active channel rows).
pub async fn has_active_channels(state: &AppState, inbox_id: &str) -> Result<bool, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM notification_channels WHERE inbox_id = ? AND is_active = 1",
    )
    .bind(inbox_id)
    .fetch_one(&state.db)
    .await?;
    Ok(n > 0)
}

/// Send the event to every active channel of the inbox. Returns at once: the
/// work runs in a spawned task and errors are only logged, so intake never
/// fails because of a channel. Does nothing for an empty body, like Python.
pub async fn dispatch<I, E>(state: &AppState, inbox: &I, event: &E)
where
    I: InboxView + ?Sized,
    E: EventView + ?Sized,
{
    let Some(body) = event.body().filter(|b| pyfmt::truthy(b)).cloned() else {
        return;
    };
    let state = state.clone();
    let inbox = inbox.info();
    tokio::spawn(async move {
        let report = dispatch_now(&state, &inbox, &body).await;
        tracing::debug!(inbox = %inbox.slug, ?report, "notifications dispatched");
    });
}

/// What one dispatch did (for logs and tests).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct DispatchReport {
    /// HTTP channels that answered below 400.
    pub sent: usize,
    /// HTTP channels that failed or answered 400 or more.
    pub failed: usize,
    /// Emails delivered.
    pub emails: usize,
}

/// The awaited form of [`dispatch`]: load the active channels and send.
pub async fn dispatch_now(state: &AppState, inbox: &InboxInfo, body: &Value) -> DispatchReport {
    let rows: Vec<ChannelRow> = match sqlx::query_as(
        "SELECT id, inbox_id, type, label, config, is_active, created_at \
         FROM notification_channels WHERE inbox_id = ? AND is_active = 1",
    )
    .bind(&inbox.id)
    .fetch_all(&state.db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(inbox = %inbox.slug, "loading channels failed: {e}");
            return DispatchReport::default();
        }
    };
    let mailer = email::resolve_mailer(state, &inbox.id).await;
    dispatch_channels(state, inbox, &rows, body, mailer.as_deref()).await
}

/// Python `dispatch_notifications(inbox, channels, body, email_provider)`.
pub async fn dispatch_channels(
    state: &AppState,
    inbox: &InboxInfo,
    channels: &[ChannelRow],
    body: &Value,
    mailer: Option<&dyn email::ChannelMailer>,
) -> DispatchReport {
    let mut report = DispatchReport::default();
    // Python's formatters call `body.items()`, which fails for anything but
    // an object, so a list or scalar body notifies nobody.
    let Value::Object(body) = body else {
        tracing::warn!(inbox = %inbox.slug, "body is not an object, no notifications sent");
        return report;
    };
    let ctx = ChannelContext::new(inbox, body.clone());
    let mut tasks = tokio::task::JoinSet::new();
    let mut recipients: Vec<String> = Vec::new();

    for channel in channels {
        if channel.is_active == Some(false) {
            continue;
        }
        let config = channel.config_map();
        let mut channel_type = channel.channel_type.as_str();
        if channel_type == "webhook"
            && let Some(Value::String(url)) = config.get("url")
            && !url.is_empty()
        {
            channel_type = detect::detect_channel_type(url);
        }
        let payload = match channel_type {
            "discord" => senders::format_discord(&config, &ctx),
            "slack" => senders::format_slack(&config, &ctx),
            "teams" => senders::format_teams(&config, &ctx),
            "telegram" => senders::format_telegram(&config, &ctx),
            "ntfy" => senders::format_ntfy(&config, &ctx),
            "webhook" => senders::format_webhook(&config, &ctx),
            "email" => {
                recipients.extend(email::recipients_of(&config));
                continue;
            }
            other => {
                tracing::warn!("unknown channel type: {other}");
                continue;
            }
        };
        let http = state.http.clone();
        let id = channel.id.clone();
        tasks.spawn(async move { send_payload(&http, &payload, &id).await });
    }

    while let Some(res) = tasks.join_next().await {
        match res {
            Ok(Ok(status)) if status < 400 => report.sent += 1,
            _ => report.failed += 1,
        }
    }

    if !recipients.is_empty()
        && let Some(mailer) = mailer
    {
        report.emails = email::send_emails(state, &inbox.id, mailer, &recipients, &ctx).await;
    }
    report
}

/// Python `_send_notification`: one POST, logged, never raised. Returns the
/// status code, or the reason the request did not complete.
pub async fn send_payload(
    http: &reqwest::Client,
    payload: &ChannelPayload,
    channel_id: &str,
) -> Result<u16, String> {
    let result = try_send(http, payload).await;
    match &result {
        Ok((status, text)) if *status >= 400 => {
            let snippet: String = text.chars().take(200).collect();
            tracing::warn!("channel {channel_id} returned status {status}: {snippet}");
        }
        Ok(_) => tracing::debug!("sent notification to channel {channel_id}"),
        Err(e) => tracing::error!("failed to send notification to channel {channel_id}: {e}"),
    }
    result.map(|(status, _)| status)
}

async fn try_send(
    http: &reqwest::Client,
    payload: &ChannelPayload,
) -> Result<(u16, String), String> {
    let url = reqwest::Url::parse(&payload.url).map_err(|e| format!("invalid URL: {e}"))?;
    security::check_target(&url)?;
    let mut headers = HeaderMap::new();
    for (k, v) in &payload.headers {
        let name = HeaderName::from_bytes(k.as_bytes()).map_err(|e| format!("header {k}: {e}"))?;
        let value =
            HeaderValue::from_bytes(v.as_bytes()).map_err(|e| format!("header {k}: {e}"))?;
        headers.insert(name, value);
    }
    let req = http
        .post(url)
        .timeout(SEND_TIMEOUT)
        .headers(headers)
        .body(payload.body.clone())
        .build()
        .map_err(|e| e.to_string())?;
    let resp = security::send_following(http, req).await?;
    let status = resp.status().as_u16();
    let text = if status >= 400 {
        resp.text().await.unwrap_or_default()
    } else {
        String::new()
    };
    Ok((status, text))
}

impl InboxView for crate::models::Inbox {
    fn info(&self) -> InboxInfo {
        InboxInfo {
            id: self.id.clone(),
            slug: self.slug.clone(),
            email_subject_prefix: self.email_subject_prefix.clone(),
            sender_name: self.sender_name.clone(),
        }
    }
}

impl EventView for crate::models::Event {
    fn body(&self) -> Option<&Value> {
        self.body.as_ref()
    }
}
