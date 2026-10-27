//! Config validation for notification channels (Python `channels/validate.py`).
//! Messages are copied word for word; they reach API clients as 400 details.

use serde_json::{Map, Value};

use super::pyfmt::{py_str, truthy};

pub const VALID_CHANNEL_TYPES: &[&str] = &[
    "email", "discord", "slack", "teams", "telegram", "ntfy", "webhook",
];

const CHANNEL_SUGGESTIONS: &[(&str, &str)] = &[
    ("discrod", "discord"),
    ("dicord", "discord"),
    ("disocrd", "discord"),
    ("slak", "slack"),
    ("sclack", "slack"),
    ("team", "teams"),
    ("ms-teams", "teams"),
    ("msteams", "teams"),
    ("microsoft-teams", "teams"),
    ("telegarm", "telegram"),
    ("telgram", "telegram"),
    ("tg", "telegram"),
    ("emal", "email"),
    ("mail", "email"),
    ("e-mail", "email"),
    ("webhok", "webhook"),
    ("hook", "webhook"),
    ("nfty", "ntfy"),
    ("notify", "ntfy"),
];

pub fn is_valid_channel_type(t: &str) -> bool {
    VALID_CHANNEL_TYPES.contains(&t)
}

/// A suggestion when the input looks like a typo of a valid type.
pub fn suggest_channel_type(input: &str) -> Option<&'static str> {
    if is_valid_channel_type(input) {
        return None;
    }
    let lower = input.to_lowercase();
    CHANNEL_SUGGESTIONS
        .iter()
        .find(|(typo, _)| *typo == lower)
        .map(|(_, s)| *s)
}

/// `None` when valid, else the error message.
pub fn validate_channel_config(channel_type: &str, config: &Map<String, Value>) -> Option<String> {
    match channel_type {
        "email" => validate_email(config),
        "discord" => validate_discord(config),
        "slack" => validate_slack(config),
        "teams" => validate_url(first_truthy(config, "webhook_url", "url"), "webhook_url"),
        "telegram" => validate_telegram(config),
        "ntfy" => validate_ntfy(config),
        "webhook" => validate_webhook(config),
        other => Some(format!("Unknown channel type: {other}")),
    }
}

/// Python `config.get(a) or config.get(b)`.
fn first_truthy<'a>(config: &'a Map<String, Value>, a: &str, b: &str) -> Option<&'a Value> {
    match config.get(a) {
        Some(v) if truthy(v) => Some(v),
        _ => config.get(b),
    }
}

/// The parts of Python's `urllib.parse.urlsplit` the validator looks at:
/// lowercased scheme and netloc. `Err` where urlsplit raises `ValueError`.
fn split_scheme_netloc(url: &str) -> Result<(String, String), ()> {
    let url: String = url
        .trim_start_matches(|c: char| c <= ' ')
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme = String::new();
    let mut rest = url.as_str();
    if let Some(i) = url.find(':')
        && i > 0
    {
        let candidate = &url[..i];
        let first = candidate.chars().next().unwrap_or(' ');
        if first.is_ascii_alphabetic()
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = candidate.to_ascii_lowercase();
            rest = &url[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        let (open, close) = (netloc.contains('['), netloc.contains(']'));
        if open != close {
            return Err(());
        }
        if open {
            let inner = netloc
                .split_once('[')
                .and_then(|(_, r)| r.split_once(']'))
                .map(|(h, _)| h)
                .unwrap_or("");
            let host = inner.split('%').next().unwrap_or("");
            if !inner.starts_with('v') && host.parse::<std::net::Ipv6Addr>().is_err() {
                return Err(());
            }
        }
    }
    Ok((scheme, netloc))
}

fn validate_url(value: Option<&Value>, field: &str) -> Option<String> {
    let url = match value {
        Some(Value::String(s)) if !s.is_empty() => s,
        _ => return Some(format!("Missing required field: {field}")),
    };
    match split_scheme_netloc(url) {
        Err(()) => Some(format!("{field} is not a valid URL")),
        Ok((scheme, _)) if scheme != "http" && scheme != "https" => {
            Some(format!("{field} must use http or https protocol"))
        }
        Ok((_, netloc)) if netloc.is_empty() => Some(format!("{field} is not a valid URL")),
        Ok(_) => None,
    }
}

fn validate_email(config: &Map<String, Value>) -> Option<String> {
    let recipients = match config.get("recipients") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        _ => return Some("Email channel requires a non-empty recipients array".into()),
    };
    for r in recipients {
        match r {
            Value::String(s) if s.contains('@') => {}
            other => return Some(format!("Invalid email address: {}", py_str(other))),
        }
    }
    None
}

fn validate_discord(config: &Map<String, Value>) -> Option<String> {
    let url = first_truthy(config, "webhook_url", "url");
    if let Some(err) = validate_url(url, "webhook_url") {
        return Some(err);
    }
    match url {
        Some(Value::String(s)) if !s.contains("discord.com/api/webhooks") => {
            Some("Discord webhook_url must be a discord.com webhook URL".into())
        }
        _ => None,
    }
}

fn validate_slack(config: &Map<String, Value>) -> Option<String> {
    let url = first_truthy(config, "webhook_url", "url");
    if let Some(err) = validate_url(url, "webhook_url") {
        return Some(err);
    }
    match url {
        Some(Value::String(s)) if !s.contains("hooks.slack.com/") => {
            Some("Slack webhook_url must be a hooks.slack.com URL".into())
        }
        _ => None,
    }
}

fn validate_telegram(config: &Map<String, Value>) -> Option<String> {
    if let Some(err) = validate_url(config.get("bot_url"), "bot_url") {
        return Some(err);
    }
    if !config.get("chat_id").is_some_and(truthy) {
        return Some("Telegram channel requires chat_id".into());
    }
    None
}

/// Python `int(value)` for a JSON value; `None` where Python raises
/// `ValueError` or `TypeError`.
fn py_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| n.as_u64().map(|_| i64::MAX))
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite())
                    .map(|f| f.trunc() as i64)
            }),
        Value::String(s) => {
            let t = s.trim();
            let (sign, digits) = match t.strip_prefix(['+', '-']) {
                Some(d) => (if t.starts_with('-') { -1 } else { 1 }, d),
                None => (1, t),
            };
            // Underscores only between digits, as in Python int literals.
            if digits.is_empty()
                || digits.starts_with('_')
                || digits.ends_with('_')
                || digits.contains("__")
                || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
            {
                return None;
            }
            let clean: String = digits.chars().filter(|c| *c != '_').collect();
            Some(
                clean
                    .parse::<i64>()
                    .map(|n| n * sign)
                    .unwrap_or(i64::MAX * sign),
            )
        }
        _ => None,
    }
}

fn validate_ntfy(config: &Map<String, Value>) -> Option<String> {
    if let Some(err) = validate_url(config.get("url"), "url") {
        return Some(err);
    }
    match config.get("priority") {
        None | Some(Value::Null) => None,
        Some(p) => match py_int(p) {
            Some(n) if (1..=5).contains(&n) => None,
            _ => Some("Ntfy priority must be between 1 and 5".into()),
        },
    }
}

fn validate_webhook(config: &Map<String, Value>) -> Option<String> {
    if let Some(err) = validate_url(first_truthy(config, "url", "webhook_url"), "url") {
        return Some(err);
    }
    match config.get("custom_headers") {
        None | Some(Value::Null) | Some(Value::Object(_)) => None,
        Some(_) => Some("custom_headers must be an object".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn check(t: &str, config: Value) -> Option<String> {
        validate_channel_config(t, config.as_object().unwrap())
    }

    #[test]
    fn suggestions() {
        assert_eq!(suggest_channel_type("Discrod"), Some("discord"));
        assert_eq!(suggest_channel_type("tg"), Some("telegram"));
        assert_eq!(suggest_channel_type("discord"), None);
        assert_eq!(suggest_channel_type("pigeon"), None);
    }

    #[test]
    fn email_rules() {
        assert_eq!(
            check("email", json!({})).unwrap(),
            "Email channel requires a non-empty recipients array"
        );
        assert_eq!(
            check("email", json!({"recipients": "a@b.c"})).unwrap(),
            "Email channel requires a non-empty recipients array"
        );
        assert_eq!(
            check("email", json!({"recipients": ["a@b.c", "nope"]})).unwrap(),
            "Invalid email address: nope"
        );
        assert_eq!(
            check("email", json!({"recipients": [5]})).unwrap(),
            "Invalid email address: 5"
        );
        assert_eq!(
            check("email", json!({"recipients": [null]})).unwrap(),
            "Invalid email address: None"
        );
        assert_eq!(check("email", json!({"recipients": ["a@b.c"]})), None);
    }

    #[test]
    fn url_rules() {
        assert_eq!(
            check("discord", json!({})).unwrap(),
            "Missing required field: webhook_url"
        );
        assert_eq!(
            check("discord", json!({"webhook_url": 5})).unwrap(),
            "Missing required field: webhook_url"
        );
        assert_eq!(
            check(
                "discord",
                json!({"webhook_url": "ftp://discord.com/api/webhooks/1"})
            )
            .unwrap(),
            "webhook_url must use http or https protocol"
        );
        assert_eq!(
            check(
                "discord",
                json!({"webhook_url": "discord.com/api/webhooks/1"})
            )
            .unwrap(),
            "webhook_url must use http or https protocol"
        );
        assert_eq!(
            check("discord", json!({"webhook_url": "https:///x"})).unwrap(),
            "webhook_url is not a valid URL"
        );
        assert_eq!(
            check("discord", json!({"webhook_url": "http://[::1/x"})).unwrap(),
            "webhook_url is not a valid URL"
        );
        assert_eq!(
            check("discord", json!({"webhook_url": "https://example.com/x"})).unwrap(),
            "Discord webhook_url must be a discord.com webhook URL"
        );
        assert_eq!(
            check(
                "discord",
                json!({"url": "https://discord.com/api/webhooks/1/x"})
            ),
            None
        );
        assert_eq!(
            check(
                "discord",
                json!({"webhook_url": "", "url": "https://discord.com/api/webhooks/1/x"})
            ),
            None
        );
        assert_eq!(
            check("slack", json!({"webhook_url": "https://example.com/"})).unwrap(),
            "Slack webhook_url must be a hooks.slack.com URL"
        );
        assert_eq!(
            check(
                "slack",
                json!({"webhook_url": "HTTPS://hooks.slack.com/services/x"})
            ),
            None
        );
        assert_eq!(
            check("teams", json!({"url": "https://anything.example/x"})),
            None
        );
        assert_eq!(
            check("teams", json!({})).unwrap(),
            "Missing required field: webhook_url"
        );
    }

    #[test]
    fn telegram_ntfy_webhook() {
        assert_eq!(
            check("telegram", json!({"chat_id": 1})).unwrap(),
            "Missing required field: bot_url"
        );
        assert_eq!(
            check(
                "telegram",
                json!({"bot_url": "https://api.telegram.org/botX/sendMessage", "chat_id": ""})
            )
            .unwrap(),
            "Telegram channel requires chat_id"
        );
        assert_eq!(
            check(
                "telegram",
                json!({"bot_url": "https://api.telegram.org/botX/sendMessage", "chat_id": -100})
            ),
            None
        );
        assert_eq!(check("ntfy", json!({"url": "https://ntfy.sh/t"})), None);
        assert_eq!(
            check("ntfy", json!({"url": "https://ntfy.sh/t", "priority": "3"})),
            None
        );
        assert_eq!(
            check("ntfy", json!({"url": "https://ntfy.sh/t", "priority": 4.9})),
            None
        );
        for p in [json!(0), json!(6), json!("high"), json!("2.5"), json!([1])] {
            assert_eq!(
                check("ntfy", json!({"url": "https://ntfy.sh/t", "priority": p})).unwrap(),
                "Ntfy priority must be between 1 and 5"
            );
        }
        assert_eq!(
            check("webhook", json!({"webhook_url": "https://example.com/h"})),
            None
        );
        assert_eq!(
            check("webhook", json!({})).unwrap(),
            "Missing required field: url"
        );
        assert_eq!(
            check(
                "webhook",
                json!({"url": "https://example.com/h", "custom_headers": ["x"]})
            )
            .unwrap(),
            "custom_headers must be an object"
        );
        assert_eq!(
            check("pigeon", json!({})).unwrap(),
            "Unknown channel type: pigeon"
        );
    }
}
