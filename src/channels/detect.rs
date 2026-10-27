//! Channel type detection from a URL (Python `channels/detect.py`).

/// One of `discord`, `slack`, `teams`, `telegram`, `ntfy` or `webhook`.
pub fn detect_channel_type(url: &str) -> &'static str {
    let url = url.to_lowercase();
    if url.contains("discord.com/api/webhooks") {
        "discord"
    } else if url.contains("hooks.slack.com/") {
        "slack"
    } else if url.contains("webhook.office.com") || url.contains("logic.azure.com") {
        "teams"
    } else if url.contains("api.telegram.org/bot") {
        "telegram"
    } else if url.contains("ntfy.sh/") {
        "ntfy"
    } else {
        "webhook"
    }
}

#[cfg(test)]
mod tests {
    use super::detect_channel_type as d;

    #[test]
    fn detects_every_type() {
        assert_eq!(d("https://Discord.com/api/webhooks/1/abc"), "discord");
        assert_eq!(d("https://hooks.slack.com/services/T/B/x"), "slack");
        assert_eq!(d("https://acme.webhook.office.com/webhookb2/x"), "teams");
        assert_eq!(
            d("https://prod-1.westus.logic.azure.com/workflows/x"),
            "teams"
        );
        assert_eq!(
            d("https://api.telegram.org/bot123:abc/sendMessage"),
            "telegram"
        );
        assert_eq!(d("https://ntfy.sh/topic"), "ntfy");
        assert_eq!(d("https://ntfy.example.com/topic"), "webhook");
        assert_eq!(d("https://example.com/hook"), "webhook");
    }
}
