//! Legacy `forward_url` forwarding for inboxes without channel rows (Python:
//! the `else` branch of `receive_webhook`). Discord and Slack webhook URLs get
//! a formatted message, anything else gets the body re-sent as JSON with the
//! original method.

use std::time::Duration;

use reqwest::Method;
use serde_json::{Map, Value, json};

use crate::models::Inbox;

const FORWARD_TIMEOUT: Duration = Duration::from_secs(10);
/// Discord embed colour 0xD4A843.
const DISCORD_COLOR: u32 = 0xD4A843;

/// Send the event body to `forward_url`. Errors are logged, never returned.
pub async fn forward(http: reqwest::Client, inbox: Inbox, method: Method, body: Value) {
    let Some(url) = inbox.forward_url.as_deref() else {
        return;
    };
    let slug = &inbox.slug;
    let forwarded_from = format!("hookforms/hooks/{slug}");
    let request = match &body {
        Value::Object(map) if url.contains("discord.com/api/webhooks") => http
            .post(url)
            .json(&discord_payload(&inbox, map, &python_utc_now())),
        Value::Object(map) if url.contains("hooks.slack.com/") => {
            http.post(url).json(&slack_payload(&inbox, map))
        }
        _ => http.request(method, url).json(&body),
    };
    let result = match request
        .header("X-Forwarded-From", forwarded_from)
        .timeout(FORWARD_TIMEOUT)
        .build()
    {
        Ok(req) => crate::security::send_following(&http, req).await,
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = result {
        tracing::error!(error = %e, "Forwarding failed for /hooks/{slug}");
    }
}

fn subject_prefix(inbox: &Inbox) -> String {
    match inbox.email_subject_prefix.as_deref() {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => format!("[{}]", inbox.slug),
    }
}

/// `datetime.now(timezone.utc).isoformat()`.
fn python_utc_now() -> String {
    let now = chrono::Utc::now();
    if now.timestamp_subsec_micros() == 0 {
        now.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        now.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

pub fn discord_payload(inbox: &Inbox, body: &Map<String, Value>, timestamp: &str) -> Value {
    let fields: Vec<Value> = body
        .iter()
        .filter(|(k, v)| super::body::truthy(v) && k.as_str() != "cf-turnstile-response")
        .map(|(k, v)| {
            let formatted = format_value(v, 1024);
            json!({
                "name": py_title(&k.replace('_', " ")),
                "value": take_chars(&formatted, 1024),
                "inline": formatted.chars().count() < 50,
            })
        })
        .collect();
    json!({
        "embeds": [{
            "title": format!("{} New Submission", subject_prefix(inbox)),
            "color": DISCORD_COLOR,
            "fields": fields,
            "footer": { "text": format!("hookforms/hooks/{}", inbox.slug) },
            "timestamp": timestamp,
        }]
    })
}

pub fn slack_payload(inbox: &Inbox, body: &Map<String, Value>) -> Value {
    let lines: Vec<String> = body
        .iter()
        .filter(|(k, v)| super::body::truthy(v) && k.as_str() != "cf-turnstile-response")
        .map(|(k, v)| format!("*{}:* {}", k.replace('_', " "), format_value(v, 300)))
        .collect();
    json!({
        "text": format!("{} New Submission", subject_prefix(inbox)),
        "blocks": [{
            "type": "section",
            "text": { "type": "mrkdwn", "text": lines.join("\n") },
        }]
    })
}

/// One copy of each Python formatting helper, shared with the channels.
pub use crate::channels::format_value::format_value;
use crate::channels::pyfmt::take_chars;
pub use crate::channels::pyfmt::title as py_title;

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox(prefix: Option<&str>) -> Inbox {
        Inbox {
            id: "i".into(),
            slug: "contact".into(),
            description: None,
            forward_url: None,
            notify_email: None,
            email_subject_prefix: prefix.map(Into::into),
            sender_name: None,
            turnstile_secret: None,
            is_active: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn title_like_python() {
        assert_eq!(py_title("first name"), "First Name");
        assert_eq!(py_title("e-mail ADDRESS"), "E-Mail Address");
        assert_eq!(py_title("2nd line's"), "2Nd Line'S");
    }

    #[test]
    fn format_value_cases() {
        let f = |v: Value| format_value(&v, 300);
        assert_eq!(f(json!(null)), "");
        assert_eq!(f(json!(true)), "True");
        assert_eq!(f(json!("x")), "x");
        assert_eq!(f(json!([])), "(empty)");
        assert_eq!(f(json!([1, "a", null, false])), "1, a, None, False");
        assert_eq!(f(json!({"login": "octo", "id": 1})), "octo");
        assert_eq!(f(json!({"name": null, "id": 5})), "5");
        assert_eq!(
            f(json!([{"name": "a"}, {"x": "é"}])),
            "a, {\"x\": \"\\u00e9\"}"
        );
        assert_eq!(
            f(json!({"a": [1, 2], "b": "c"})),
            r#"{"a": [1, 2], "b": "c"}"#
        );
        let long = json!({ "k": "x".repeat(400) });
        let out = format_value(&long, 300);
        assert_eq!(out.chars().count(), 303);
        assert!(out.ends_with("..."));
        assert_eq!(f(json!([[1], "it's"])), "1, it's");
        // Python's json.dumps escapes DEL; floats follow str(float).
        assert_eq!(f(json!({"x": "a\u{7f}b"})), r#"{"x": "a\u007fb"}"#);
        assert_eq!(f(json!({"x": 1e-05})), r#"{"x": 1e-05}"#);
    }

    #[test]
    fn discord_payload_shape() {
        let body = json!({"first_name": "Ann", "empty": "", "message": "y".repeat(60),
                          "cf-turnstile-response": "t", "n": 0});
        let p = discord_payload(&inbox(None), body.as_object().unwrap(), "TS");
        assert_eq!(
            p,
            json!({"embeds": [{
                "title": "[contact] New Submission",
                "color": 13936707,
                "fields": [
                    {"name": "First Name", "value": "Ann", "inline": true},
                    {"name": "Message", "value": "y".repeat(60), "inline": false},
                ],
                "footer": {"text": "hookforms/hooks/contact"},
                "timestamp": "TS",
            }]})
        );
    }

    #[test]
    fn slack_payload_shape() {
        let body = json!({"first_name": "Ann", "tags": ["a", "b"]});
        let p = slack_payload(&inbox(Some("[Site]")), body.as_object().unwrap());
        assert_eq!(
            p,
            json!({
                "text": "[Site] New Submission",
                "blocks": [{"type": "section", "text": {"type": "mrkdwn",
                    "text": "*first name:* Ann\n*tags:* a, b"}}],
            })
        );
    }
}
