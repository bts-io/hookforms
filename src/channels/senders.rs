//! Payload builders for each HTTP channel type (Python `channels/discord.py`,
//! `slack.py`, `teams.py`, `telegram.py`, `ntfy.py`, `webhook.py`).
//! Same layouts, field formatting and truncation limits as Python.

use serde_json::{Map, Value, json};

use super::format_value::{DEFAULT_MAX_LEN, format_value};
use super::pyfmt::{label, py_dumps, take_chars, truthy};
use super::{ChannelContext, ChannelPayload, SKIP_KEYS};

/// Discord embed gold.
pub const DISCORD_COLOR: u32 = 0xD4A843;

fn config_str(config: &Map<String, Value>, key: &str) -> String {
    match config.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Python reads only `webhook_url` here, though validation also accepts `url`,
/// so a channel saved with `url` alone sent to an empty URL. Fall back to `url`.
fn webhook_url(config: &Map<String, Value>) -> String {
    let primary = config_str(config, "webhook_url");
    if primary.is_empty() {
        config_str(config, "url")
    } else {
        primary
    }
}

/// Body fields that are shown: truthy and not internal.
fn shown_fields(body: &Map<String, Value>) -> impl Iterator<Item = (&String, &Value)> {
    body.iter()
        .filter(|(k, v)| truthy(v) && !SKIP_KEYS.contains(&k.as_str()))
}

fn json_headers(ctx: &ChannelContext) -> Vec<(String, String)> {
    vec![
        ("Content-Type".into(), "application/json".into()),
        ("X-Forwarded-From".into(), ctx.forwarded_from()),
    ]
}

pub fn format_discord(config: &Map<String, Value>, ctx: &ChannelContext) -> ChannelPayload {
    let fields: Vec<Value> = shown_fields(&ctx.body)
        .map(|(k, v)| {
            let formatted = format_value(v, 1024);
            json!({
                "name": label(k),
                "value": take_chars(&formatted, 1024),
                "inline": formatted.chars().count() < 50,
            })
        })
        .collect();
    let embed = json!({
        "embeds": [{
            "title": format!("{} New Submission", ctx.subject_prefix),
            "color": DISCORD_COLOR,
            "fields": fields,
            "footer": {"text": ctx.forwarded_from()},
            "timestamp": ctx.timestamp,
        }]
    });
    ChannelPayload {
        url: webhook_url(config),
        headers: json_headers(ctx),
        body: py_dumps(&embed),
    }
}

pub fn format_slack(config: &Map<String, Value>, ctx: &ChannelContext) -> ChannelPayload {
    let lines: Vec<String> = shown_fields(&ctx.body)
        .map(|(k, v)| {
            format!(
                "*{}:* {}",
                k.replace('_', " "),
                format_value(v, DEFAULT_MAX_LEN)
            )
        })
        .collect();
    let body = json!({
        "text": format!("{} New Submission", ctx.subject_prefix),
        "blocks": [{
            "type": "section",
            "text": {"type": "mrkdwn", "text": lines.join("\n")},
        }],
    });
    ChannelPayload {
        url: webhook_url(config),
        headers: json_headers(ctx),
        body: py_dumps(&body),
    }
}

pub fn format_teams(config: &Map<String, Value>, ctx: &ChannelContext) -> ChannelPayload {
    let facts: Vec<Value> = shown_fields(&ctx.body)
        .map(|(k, v)| json!({"title": label(k), "value": take_chars(&format_value(v, 1024), 1024)}))
        .collect();
    let card = json!({
        "type": "AdaptiveCard",
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "version": "1.4",
        "body": [
            {
                "type": "TextBlock",
                "text": format!("{} New Submission", ctx.subject_prefix),
                "weight": "Bolder",
                "size": "Large",
                "wrap": true,
            },
            {"type": "FactSet", "facts": facts},
            {
                "type": "TextBlock",
                "text": ctx.forwarded_from(),
                "size": "Small",
                "color": "Accent",
                "wrap": true,
            },
        ],
    });
    let body = json!({
        "type": "message",
        "attachments": [{
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": card,
        }],
    });
    ChannelPayload {
        url: webhook_url(config),
        headers: json_headers(ctx),
        body: py_dumps(&body),
    }
}

pub fn format_telegram(config: &Map<String, Value>, ctx: &ChannelContext) -> ChannelPayload {
    let mut lines = vec![format!("<b>{} New Submission</b>\n", ctx.subject_prefix)];
    for (k, v) in shown_fields(&ctx.body) {
        // Python escapes only the value, not the label or the prefix.
        let value = format_value(v, DEFAULT_MAX_LEN)
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        lines.push(format!("<b>{}:</b> {value}", label(k)));
    }
    lines.push(format!("\n<i>{}</i>", ctx.forwarded_from()));
    let chat_id = config.get("chat_id").cloned().unwrap_or(json!(""));
    let body = json!({
        "chat_id": chat_id,
        "text": lines.join("\n"),
        "parse_mode": "HTML",
    });
    ChannelPayload {
        url: config_str(config, "bot_url"),
        headers: json_headers(ctx),
        body: py_dumps(&body),
    }
}

pub fn format_ntfy(config: &Map<String, Value>, ctx: &ChannelContext) -> ChannelPayload {
    let lines: Vec<String> = shown_fields(&ctx.body)
        .map(|(k, v)| format!("{}: {}", label(k), format_value(v, DEFAULT_MAX_LEN)))
        .collect();
    ChannelPayload {
        url: config_str(config, "url"),
        headers: vec![
            (
                "Title".into(),
                format!("{} New Submission", ctx.subject_prefix),
            ),
            ("Tags".into(), "incoming_envelope".into()),
            ("Priority".into(), "default".into()),
            ("X-Forwarded-From".into(), ctx.forwarded_from()),
        ],
        body: lines.join("\n"),
    }
}

pub fn format_webhook(config: &Map<String, Value>, ctx: &ChannelContext) -> ChannelPayload {
    // Every key except the internal ones, falsy values included.
    let clean: Map<String, Value> = ctx
        .body
        .iter()
        .filter(|(k, _)| !SKIP_KEYS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut headers = json_headers(ctx);
    if let Some(Value::Object(custom)) = config.get("custom_headers") {
        for (k, v) in custom {
            let value = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            // dict.update semantics: an existing key keeps its place.
            match headers.iter_mut().find(|(hk, _)| hk == k) {
                Some(slot) => slot.1 = value,
                None => headers.push((k.clone(), value)),
            }
        }
    }
    ChannelPayload {
        url: config_str(config, "url"),
        headers,
        body: py_dumps(&Value::Object(clean)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(body: Value) -> ChannelContext {
        ChannelContext {
            slug: "contact".into(),
            subject_prefix: "[contact]".into(),
            sender_name: "HookForms".into(),
            body: body.as_object().unwrap().clone(),
            timestamp: "2026-10-27T10:00:00.000001+00:00".into(),
        }
    }

    fn cfg(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn body_json(p: &ChannelPayload) -> Value {
        serde_json::from_str(&p.body).unwrap()
    }

    #[test]
    fn discord_embed() {
        let long = "x".repeat(1100);
        let c = ctx(json!({
            "first_name": "Ada", "empty": "", "cf-turnstile-response": "t", "raw": "r",
            "source": "s", "message": long, "team": {"login": "octo"}
        }));
        let p = format_discord(
            &cfg(json!({"webhook_url": "https://discord.com/api/webhooks/1/a"})),
            &c,
        );
        assert_eq!(p.url, "https://discord.com/api/webhooks/1/a");
        assert_eq!(
            p.headers,
            vec![
                ("Content-Type".to_string(), "application/json".to_string()),
                (
                    "X-Forwarded-From".to_string(),
                    "hookforms/hooks/contact".to_string()
                ),
            ]
        );
        let b = body_json(&p);
        let embed = &b["embeds"][0];
        assert_eq!(embed["title"], "[contact] New Submission");
        assert_eq!(embed["color"], 13936707);
        assert_eq!(embed["footer"]["text"], "hookforms/hooks/contact");
        assert_eq!(embed["timestamp"], "2026-10-27T10:00:00.000001+00:00");
        let fields = embed["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(
            fields[0],
            json!({"name": "First Name", "value": "Ada", "inline": true})
        );
        assert_eq!(fields[1]["name"], "Message");
        assert_eq!(fields[1]["value"].as_str().unwrap().len(), 1024);
        assert_eq!(fields[1]["inline"], false);
        assert_eq!(
            fields[2],
            json!({"name": "Team", "value": "octo", "inline": true})
        );
        // Python json.dumps layout.
        assert!(p.body.starts_with(r#"{"embeds": [{"title": "[contact] New Submission", "color": 13936707, "fields": [{"name": "First Name""#));
    }

    #[test]
    fn discord_falls_back_to_url_key() {
        let p = format_discord(
            &cfg(json!({"url": "https://discord.com/api/webhooks/9/z"})),
            &ctx(json!({"a": 1})),
        );
        assert_eq!(p.url, "https://discord.com/api/webhooks/9/z");
    }

    #[test]
    fn slack_blocks() {
        let p = format_slack(
            &cfg(json!({"webhook_url": "https://hooks.slack.com/services/x"})),
            &ctx(json!({"first_name": "Ada", "n": 0, "tags": ["a", "b"]})),
        );
        assert_eq!(
            body_json(&p),
            json!({
                "text": "[contact] New Submission",
                "blocks": [{"type": "section", "text": {"type": "mrkdwn", "text": "*first name:* Ada\n*tags:* a, b"}}]
            })
        );
    }

    #[test]
    fn teams_card() {
        let p = format_teams(
            &cfg(json!({"webhook_url": "https://x.webhook.office.com/a"})),
            &ctx(json!({"email": "a@b.c"})),
        );
        let b = body_json(&p);
        assert_eq!(b["type"], "message");
        let att = &b["attachments"][0];
        assert_eq!(
            att["contentType"],
            "application/vnd.microsoft.card.adaptive"
        );
        let card = &att["content"];
        assert_eq!(
            card["$schema"],
            "http://adaptivecards.io/schemas/adaptive-card.json"
        );
        assert_eq!(card["version"], "1.4");
        assert_eq!(
            card["body"][0],
            json!({"type": "TextBlock", "text": "[contact] New Submission", "weight": "Bolder", "size": "Large", "wrap": true})
        );
        assert_eq!(
            card["body"][1],
            json!({"type": "FactSet", "facts": [{"title": "Email", "value": "a@b.c"}]})
        );
        assert_eq!(
            card["body"][2],
            json!({"type": "TextBlock", "text": "hookforms/hooks/contact", "size": "Small", "color": "Accent", "wrap": true})
        );
    }

    #[test]
    fn telegram_html() {
        let p = format_telegram(
            &cfg(json!({"bot_url": "https://api.telegram.org/botT/sendMessage", "chat_id": -100})),
            &ctx(json!({"note": "<b>&</b>", "first_name": "Ada"})),
        );
        assert_eq!(p.url, "https://api.telegram.org/botT/sendMessage");
        assert_eq!(
            body_json(&p),
            json!({
                "chat_id": -100,
                "text": "<b>[contact] New Submission</b>\n\n<b>Note:</b> &lt;b&gt;&amp;&lt;/b&gt;\n<b>First Name:</b> Ada\n\n<i>hookforms/hooks/contact</i>",
                "parse_mode": "HTML",
            })
        );
    }

    #[test]
    fn ntfy_plain_text() {
        let p = format_ntfy(
            &cfg(json!({"url": "https://ntfy.sh/t", "priority": 5})),
            &ctx(json!({"first_name": "Ada", "msg": "hi"})),
        );
        assert_eq!(p.body, "First Name: Ada\nMsg: hi");
        assert_eq!(
            p.headers,
            vec![
                ("Title".to_string(), "[contact] New Submission".to_string()),
                ("Tags".to_string(), "incoming_envelope".to_string()),
                ("Priority".to_string(), "default".to_string()),
                (
                    "X-Forwarded-From".to_string(),
                    "hookforms/hooks/contact".to_string()
                ),
            ]
        );
    }

    #[test]
    fn webhook_clean_body_and_headers() {
        let p = format_webhook(
            &cfg(
                json!({"url": "https://example.com/h", "custom_headers": {"Authorization": "Bearer x", "Content-Type": "text/plain"}}),
            ),
            &ctx(json!({"a": "", "raw": "r", "b": 2})),
        );
        assert_eq!(p.body, r#"{"a": "", "b": 2}"#);
        assert_eq!(
            p.headers,
            vec![
                ("Content-Type".to_string(), "text/plain".to_string()),
                (
                    "X-Forwarded-From".to_string(),
                    "hookforms/hooks/contact".to_string()
                ),
                ("Authorization".to_string(), "Bearer x".to_string()),
            ]
        );
    }
}
