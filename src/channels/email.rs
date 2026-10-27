//! The `email` channel type (Python `dispatcher._send_emails` and
//! `_build_email_html`): recipients from every email channel of the inbox,
//! the per-inbox rate limit (10 per 10 minutes, key `channel_email_rate:{id}`),
//! and the HTML body. Delivery goes through the inbox's resolved email
//! provider, which step 5 (`crate::email`) owns; see `resolve_mailer`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value};

use super::ChannelContext;
use super::format_value::{DEFAULT_MAX_LEN, format_value};
use super::pyfmt::{html_escape, label, py_str, truthy};
use crate::state::AppState;

pub const EMAIL_RATE_LIMIT: u64 = 10;
pub const EMAIL_RATE_WINDOW: Duration = Duration::from_secs(600);

pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// What the channel dispatcher needs from an email provider (Python
/// `providers.base.EmailProvider.send_email`).
pub trait ChannelMailer: Send + Sync {
    fn provider_type(&self) -> &str;
    fn send_email<'a>(
        &'a self,
        to: &'a str,
        subject: &'a str,
        html: &'a str,
        sender_name: &'a str,
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
        sender_name: &'a str,
    ) -> SendFuture<'a> {
        Box::pin(
            self.provider
                .send(&self.http, to, subject, html, Some(sender_name)),
        )
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

/// Python: `f"{prefix} from {name}"`, or `New Submission` when there is no name.
pub fn subject(ctx: &ChannelContext) -> String {
    let name = ctx
        .body
        .get("name")
        .map(py_str)
        .unwrap_or_else(|| "Unknown".into());
    let detail = if name != "Unknown" {
        format!("from {name}")
    } else {
        "New Submission".into()
    };
    format!("{} {detail}", ctx.subject_prefix)
}

/// Python `_build_email_html(slug, body, sender_name)`.
pub fn build_email_html(slug: &str, body: &Map<String, Value>, sender_name: &str) -> String {
    let slug_escaped = html_escape(slug);
    let mut field_rows = String::new();
    for (key, val) in body {
        if super::SKIP_KEYS.contains(&key.as_str()) || !truthy(val) {
            continue;
        }
        field_rows.push_str(&format!(
            "<tr>\
             <td style=\"padding:10px 14px;font-weight:600;color:#555;\
             white-space:nowrap;vertical-align:top;border-bottom:1px solid #eee;\">{}</td>\
             <td style=\"padding:10px 14px;color:#222;border-bottom:1px solid #eee;\">{}</td>\
             </tr>",
            html_escape(&label(key)),
            html_escape(&format_value(val, DEFAULT_MAX_LEN)),
        ));
    }
    let name = html_escape(
        &body
            .get("name")
            .map(py_str)
            .unwrap_or_else(|| "Unknown".into()),
    );
    let email_raw = body.get("email").map(py_str).unwrap_or_default();
    let email_escaped = html_escape(&email_raw);
    let subject_detail = if name != "Unknown" {
        format!("from {name}")
    } else {
        "New Submission".into()
    };
    let footer_name = html_escape(if sender_name.is_empty() {
        "HookForms"
    } else {
        sender_name
    });
    let reply_button = if email_raw.is_empty() {
        String::new()
    } else {
        format!(
            "<tr><td style=\"padding:0 32px 24px;\"><a href=\"mailto:{email_escaped}\" \
             style=\"display:inline-block;padding:10px 20px;background:#1a1a2e;color:#fff;\
             text-decoration:none;border-radius:5px;font-size:14px;\">Reply to {name}</a></td></tr>"
        )
    };
    format!(
        r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body style="margin:0;padding:0;background:#f4f5f7;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;">
  <table width="100%" cellpadding="0" cellspacing="0" style="background:#f4f5f7;padding:32px 0;">
    <tr><td align="center">
      <table width="600" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:8px;overflow:hidden;box-shadow:0 2px 8px rgba(0,0,0,0.08);">
        <tr>
          <td style="background:#1a1a2e;padding:24px 32px;">
            <h1 style="margin:0;color:#ffffff;font-size:20px;font-weight:600;">{subject_detail}</h1>
          </td>
        </tr>
        <tr>
          <td style="padding:24px 32px;">
            <p style="margin:0 0 16px;color:#666;font-size:14px;">A new form submission was received:</p>
            <table width="100%" cellpadding="0" cellspacing="0" style="border:1px solid #eee;border-radius:6px;overflow:hidden;">
              {field_rows}
            </table>
          </td>
        </tr>
        {reply_button}
        <tr>
          <td style="padding:16px 32px;background:#fafafa;border-top:1px solid #eee;">
            <p style="margin:0;color:#999;font-size:12px;">Delivered by {footer_name} &middot; <code style="background:#eee;padding:2px 6px;border-radius:3px;font-size:11px;">/hooks/{slug_escaped}</code></p>
          </td>
        </tr>
      </table>
    </td></tr>
  </table>
</body>
</html>"#
    )
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
    let html = build_email_html(&ctx.slug, &ctx.body, &ctx.sender_name);
    let subject = subject(ctx);
    let sends = recipients
        .iter()
        .map(|to| mailer.send_email(to, &subject, &html, &ctx.sender_name));
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
    fn subject_and_html() {
        assert_eq!(subject(&ctx(json!({"name": "Ada"}))), "[contact] from Ada");
        assert_eq!(subject(&ctx(json!({"x": 1}))), "[contact] New Submission");
        let html = build_email_html(
            "con<tact>",
            &ctx(json!({"name": "<Ada>", "email": "a@b.c", "raw": "r", "empty": "", "first_name": "A&B"})).body,
            "Acme",
        );
        assert!(html.contains(r#"font-weight:600;">from &lt;Ada&gt;</h1>"#));
        assert!(html.contains(r#"border-bottom:1px solid #eee;">First Name</td><td style="padding:10px 14px;color:#222;border-bottom:1px solid #eee;">A&amp;B</td></tr>"#));
        assert!(!html.contains("Raw</td>"));
        assert!(!html.contains("Empty</td>"));
        assert!(html.contains(r#"<a href="mailto:a@b.c" style="display:inline-block;padding:10px 20px;background:#1a1a2e;color:#fff;text-decoration:none;border-radius:5px;font-size:14px;">Reply to &lt;Ada&gt;</a></td></tr>"#));
        assert!(html.contains("Delivered by Acme &middot;"));
        assert!(html.contains("/hooks/con&lt;tact&gt;</code>"));
        let no_reply = build_email_html("s", json!({"a": 1}).as_object().unwrap(), "");
        assert!(!no_reply.contains("mailto:"));
        assert!(no_reply.contains("New Submission</h1>"));
        assert!(no_reply.contains("Delivered by HookForms"));
    }
}
