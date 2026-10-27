//! The `email` channel type: recipients from every email channel of the inbox,
//! the per-inbox rate limit (10 per 10 minutes, key `channel_email_rate:{id}`),
//! and the email itself, rendered by `crate::email::render` (HTML plus a
//! plain-text twin). Delivery goes through the inbox's resolved email
//! provider (`crate::email`); see `resolve_mailer`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use super::ChannelContext;
use super::pyfmt::py_str;
use crate::email::providers::ReplyTo;
use crate::state::AppState;

pub const EMAIL_RATE_LIMIT: u64 = 10;
pub const EMAIL_RATE_WINDOW: Duration = Duration::from_secs(600);

pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// What the channel dispatcher needs from an email provider: send one email,
/// HTML plus its plain-text twin, to one recipient, with the submitter as
/// Reply-To when the submission names a valid address.
pub trait ChannelMailer: Send + Sync {
    fn provider_type(&self) -> &str;
    fn send_email<'a>(
        &'a self,
        to: &'a str,
        subject: &'a str,
        html: &'a str,
        text: &'a str,
        sender_name: &'a str,
        reply_to: Option<&'a ReplyTo>,
    ) -> SendFuture<'a>;
}

/// Python `resolve_email_provider(db, inbox_id)`: inbox provider, then the
/// global default, then the env Gmail (`crate::email::resolve_provider`).
/// No provider, or one that cannot be built, means email channels send
/// nothing, as in Python (where the dispatch logs the error).
pub async fn resolve_mailer(state: &AppState, inbox_id: &str) -> Option<Arc<dyn ChannelMailer>> {
    match crate::email::resolve_provider(state, inbox_id).await {
        Ok(Some(provider)) => Some(Arc::new(ProviderMailer {
            http: state.http.clone(),
            provider,
        })),
        Ok(None) => None,
        Err(e) => {
            tracing::error!(inbox_id, error = %e, "Email provider could not be loaded");
            None
        }
    }
}

/// A resolved `crate::email::Provider` with the HTTP client it sends through.
struct ProviderMailer {
    http: reqwest::Client,
    provider: crate::email::Provider,
}

impl ChannelMailer for ProviderMailer {
    fn provider_type(&self) -> &str {
        self.provider.provider_type()
    }

    fn send_email<'a>(
        &'a self,
        to: &'a str,
        subject: &'a str,
        html: &'a str,
        text: &'a str,
        sender_name: &'a str,
        reply_to: Option<&'a ReplyTo>,
    ) -> SendFuture<'a> {
        Box::pin(self.provider.send(
            &self.http,
            to,
            subject,
            html,
            text,
            Some(sender_name),
            reply_to,
        ))
    }
}

/// Recipients of one email channel: a list, or a comma-separated string
/// (PATCH does not validate config, so a string can be stored).
pub fn recipients_of(config: &Map<String, Value>) -> Vec<String> {
    match config.get("recipients") {
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(String::from)
            .collect(),
        Some(Value::Array(items)) => items.iter().map(py_str).collect(),
        _ => Vec::new(),
    }
}

/// The prefix, then `from {name}`, or `New Submission` when there is no name.
pub fn subject(ctx: &ChannelContext) -> String {
    crate::email::render::subject_line(&ctx.subject_prefix, &ctx.body)
}

/// The designed email (HTML plus plain text) for this submission.
pub fn render(ctx: &ChannelContext) -> crate::email::render::Rendered {
    crate::email::render::render(&ctx.slug, &subject(ctx), &ctx.body)
}

/// Rate check plus delivery to every recipient. Returns how many sends succeeded.
pub async fn send_emails(
    state: &AppState,
    inbox_id: &str,
    mailer: &dyn ChannelMailer,
    recipients: &[String],
    ctx: &ChannelContext,
) -> usize {
    if recipients.is_empty() {
        return 0;
    }
    let count = state
        .counters
        .incr(&format!("channel_email_rate:{inbox_id}"), EMAIL_RATE_WINDOW);
    if count > EMAIL_RATE_LIMIT {
        tracing::warn!(inbox = %ctx.slug, "email rate limit hit (channel dispatcher)");
        return 0;
    }
    let email = render(ctx);
    // Replying to the notification answers the submitter (their own `email` field).
    let reply_to = crate::email::render::submitter_reply_to(&ctx.body);
    let sends = recipients.iter().map(|to| {
        mailer.send_email(
            to,
            &email.subject,
            &email.html,
            &email.text,
            &ctx.sender_name,
            reply_to.as_ref(),
        )
    });
    let mut ok = 0;
    // Python gathers the sends concurrently; these futures are polled together.
    let results = join_all(sends).await;
    for (to, result) in recipients.iter().zip(results) {
        match result {
            Ok(()) => {
                ok += 1;
                tracing::info!(provider = mailer.provider_type(), "email sent to {to}");
            }
            Err(e) => tracing::error!(
                provider = mailer.provider_type(),
                "email send failed to {to}: {e}"
            ),
        }
    }
    ok
}

/// Minimal `futures::join_all` so no extra crate is needed.
async fn join_all<'a>(futs: impl Iterator<Item = SendFuture<'a>>) -> Vec<Result<(), String>> {
    let mut pending: Vec<Option<SendFuture<'a>>> = futs.map(Some).collect();
    let mut results: Vec<Option<Result<(), String>>> = (0..pending.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut all_done = true;
        for (i, slot) in pending.iter_mut().enumerate() {
            if let Some(f) = slot {
                match f.as_mut().poll(cx) {
                    std::task::Poll::Ready(r) => {
                        results[i] = Some(r);
                        *slot = None;
                    }
                    std::task::Poll::Pending => all_done = false,
                }
            }
        }
        if all_done {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await;
    results
        .into_iter()
        .map(|r| r.expect("future finished"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(body: Value) -> ChannelContext {
        ChannelContext {
            slug: "contact".into(),
            subject_prefix: "[contact]".into(),
            sender_name: "Acme".into(),
            body: body.as_object().unwrap().clone(),
            timestamp: String::new(),
        }
    }

    #[test]
    fn recipients_from_list_or_string() {
        let c = json!({"recipients": " a@b.c, ,d@e.f "});
        assert_eq!(recipients_of(c.as_object().unwrap()), ["a@b.c", "d@e.f"]);
        let c = json!({"recipients": ["a@b.c"]});
        assert_eq!(recipients_of(c.as_object().unwrap()), ["a@b.c"]);
        let c = json!({});
        assert!(recipients_of(c.as_object().unwrap()).is_empty());
    }

    #[test]
    fn subject_and_email() {
        assert_eq!(subject(&ctx(json!({"name": "Ada"}))), "[contact] from Ada");
        assert_eq!(subject(&ctx(json!({"x": 1}))), "[contact] New Submission");
        let email = render(&ctx(
            json!({"name": "<Ada>", "email": "a@b.c", "raw": "r", "empty": "", "first_name": "A&B"}),
        ));
        assert_eq!(email.subject, "[contact] from <Ada>");
        assert!(email.html.contains(">New submission from &lt;Ada&gt;</h1>"));
        assert!(email.html.contains(">A&amp;B</td>"));
        assert!(!email.html.contains(">Raw</td>"));
        assert!(!email.html.contains(">Empty</td>"));
        assert!(email.html.contains(r#"<a href="mailto:a@b.c""#));
        assert!(!email.html.contains("Delivered by"));
        assert!(email.text.contains("First Name: A&B\n"));
        assert!(email.text.contains("Reply to <Ada>: mailto:a@b.c"));
    }
}
