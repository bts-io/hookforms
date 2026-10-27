//! Email providers: Gmail (OAuth refresh token in a token file), Resend,
//! SendGrid and SMTP. Each sends one email to one recipient, from
//! `"{sender_name or 'HookForms'} <{from}>"`, always as an HTML part plus its
//! plain-text twin (`multipart/alternative` on the wire).

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use lettre::message::header::ContentType;
use lettre::message::{Body, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde_json::{Map, Value};

use super::render::truthy;

pub const RESEND_API_BASE: &str = "https://api.resend.com";
pub const SENDGRID_API_BASE: &str = "https://api.sendgrid.com";
pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com";
pub const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
/// Stored Gmail configs must keep their files here (Python `from_config`).
pub const GMAIL_ALLOWED_DIR: &str = "/app/config/gmail";

const DEFAULT_SENDER_NAME: &str = "HookForms";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// google-auth refreshes a token this long before it expires.
const REFRESH_THRESHOLD_SECS: i64 = 225;

/// Where replies to an email go (the Reply-To header): an address and, when
/// known, a display name. Form notifications use the submitter's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTo {
    pub address: String,
    pub name: Option<String>,
}

impl ReplyTo {
    /// The same Reply-To with control characters removed from both parts, or
    /// `None` when no address is left.
    fn cleaned(&self) -> Option<ReplyTo> {
        let address = clean_address(&self.address);
        let name = self
            .name
            .as_deref()
            .map(clean_header)
            .filter(|n| !n.is_empty());
        (!address.is_empty()).then_some(ReplyTo { address, name })
    }

    /// `Name <address>` for an API that parses the mailbox itself: the name
    /// loses the characters that would end it early and is quoted when it
    /// holds other specials (a comma would split it into two addresses).
    fn mailbox(&self) -> String {
        let Some(name) = self
            .name
            .as_deref()
            .map(|n| n.replace(['<', '>', '"'], ""))
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
        else {
            return self.address.clone();
        };
        if name.contains(['(', ')', '[', ']', ':', ';', '@', '\\', ',', '.']) {
            format!("\"{}\" <{}>", name.replace('\\', "\\\\"), self.address)
        } else {
            format!("{name} <{}>", self.address)
        }
    }
}

#[derive(Debug, Clone)]
pub enum Provider {
    Gmail(GmailProvider),
    Resend(ApiProvider),
    SendGrid(ApiProvider),
    Smtp(SmtpProvider),
}

impl Provider {
    pub fn provider_type(&self) -> &'static str {
        match self {
            Provider::Gmail(_) => "gmail",
            Provider::Resend(_) => "resend",
            Provider::SendGrid(_) => "sendgrid",
            Provider::Smtp(_) => "smtp",
        }
    }

    /// Build from a stored `email_providers` row (Python `_build_provider`).
    pub fn from_record(kind: &str, config: &Map<String, Value>) -> Result<Self, String> {
        match kind {
            "gmail" => GmailProvider::from_config(config).map(Provider::Gmail),
            "resend" => {
                ApiProvider::from_config(config, &api_base(RESEND_API_BASE)).map(Provider::Resend)
            }
            "sendgrid" => ApiProvider::from_config(config, &api_base(SENDGRID_API_BASE))
                .map(Provider::SendGrid),
            "smtp" => SmtpProvider::from_config(config).map(Provider::Smtp),
            other => Err(format!("Unknown email provider type: {other}")),
        }
    }

    /// Send one email. `reply_to`, when set, becomes the Reply-To header
    /// (form notifications: the submitter); the sender stays the provider's.
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        &self,
        http: &reqwest::Client,
        to: &str,
        subject: &str,
        html: &str,
        text: &str,
        sender_name: Option<&str>,
        reply_to: Option<&ReplyTo>,
    ) -> Result<(), String> {
        let display = match sender_name {
            Some(n) if !n.is_empty() => clean_header(n),
            _ => DEFAULT_SENDER_NAME.to_string(),
        };
        // Subjects and names come from submitted form fields: no provider may see CR/LF or
        // other control characters in a header value.
        let subject = clean_header(subject);
        let to = clean_address(to);
        let reply_to = reply_to.and_then(ReplyTo::cleaned);
        let msg = Outgoing {
            to: &to,
            subject: &subject,
            html,
            text,
            display: &display,
            reply_to: reply_to.as_ref(),
        };
        match self {
            Provider::Gmail(p) => p.send(http, &msg).await,
            Provider::Resend(p) => p.send_resend(http, &msg).await,
            Provider::SendGrid(p) => p.send_sendgrid(http, &msg).await,
            Provider::Smtp(p) => p.send(&msg).await,
        }
    }
}

/// One cleaned email on its way to a provider.
struct Outgoing<'a> {
    to: &'a str,
    subject: &'a str,
    html: &'a str,
    text: &'a str,
    display: &'a str,
    reply_to: Option<&'a ReplyTo>,
}

/// The real API base, unless the test-only upstream override is set.
fn api_base(default: &str) -> String {
    crate::security::test_upstream_base().unwrap_or_else(|| default.to_string())
}

/// A header value with every control character (CR, LF, tab, NUL, C1, and the Unicode line
/// and paragraph separators) replaced by a space, runs of spaces collapsed, and trimmed.
pub fn clean_header(value: &str) -> String {
    let replaced: String = value
        .chars()
        .map(|c| {
            if c.is_control() || c == '\u{2028}' || c == '\u{2029}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    replaced
        .split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// An address has no whitespace at all: anything after the first space is dropped.
pub fn clean_address(value: &str) -> String {
    clean_header(value)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `config[key]` as a string; Python raises `KeyError` when it is missing.
fn req_str(config: &Map<String, Value>, key: &str) -> Result<String, String> {
    match config.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Null) | None => Err(format!("missing config field: {key}")),
        Some(other) => Ok(other.to_string()),
    }
}

fn opt_str(config: &Map<String, Value>, key: &str) -> String {
    match config.get(key) {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// One `multipart/alternative` message: the plain-text part first, then the
/// HTML part, so clients that show HTML pick the last one they understand.
fn build_message(from: &str, msg: &Outgoing<'_>) -> Result<Message, String> {
    let from_addr = from
        .parse()
        .map_err(|e| format!("invalid sender address {from:?}: {e}"))?;
    let to_box: Mailbox = msg
        .to
        .parse()
        .map_err(|e| format!("invalid recipient address {:?}: {e}", msg.to))?;
    let mut builder = Message::builder()
        .from(Mailbox::new(Some(msg.display.to_string()), from_addr))
        .to(to_box);
    if let Some(reply) = msg.reply_to {
        // An address lettre cannot parse leaves the Reply-To out; the email still goes.
        match reply.address.parse() {
            Ok(addr) => builder = builder.reply_to(Mailbox::new(reply.name.clone(), addr)),
            Err(e) => tracing::warn!(error = %e, "Reply-To address left out"),
        }
    }
    builder
        .subject(msg.subject)
        .multipart(
            MultiPart::alternative()
                .singlepart(part(ContentType::TEXT_PLAIN, msg.text))
                .singlepart(part(ContentType::TEXT_HTML, msg.html)),
        )
        .map_err(|e| format!("cannot build message: {e}"))
}

/// One body part. lettre picks 7bit or quoted-printable for ASCII text;
/// anything else goes as base64 of the exact utf-8 text.
fn part(content_type: ContentType, content: &str) -> SinglePart {
    use lettre::message::header::ContentTransferEncoding;
    let body = if content.is_ascii() {
        Body::new(content.to_string())
    } else {
        Body::new_with_encoding(content.as_bytes().to_vec(), ContentTransferEncoding::Base64)
            .unwrap_or_else(Body::new)
    };
    SinglePart::builder().header(content_type).body(body)
}

// ---------------------------------------------------------------------------
// Resend and SendGrid
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ApiProvider {
    pub api_key: String,
    pub from_email: String,
    /// Scheme and host of the API; tests point it at a local mock server.
    pub api_base: String,
}

impl ApiProvider {
    pub fn from_config(config: &Map<String, Value>, api_base: &str) -> Result<Self, String> {
        Ok(ApiProvider {
            api_key: req_str(config, "api_key")?,
            from_email: req_str(config, "from_email")?,
            api_base: api_base.to_string(),
        })
    }

    async fn post(
        &self,
        http: &reqwest::Client,
        path: &str,
        payload: Value,
        name: &str,
    ) -> Result<(), String> {
        let resp = http
            .post(format!("{}{path}", self.api_base))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .timeout(HTTP_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("{name} request failed: {e}"))?;
        let status = resp.status();
        if status.as_u16() >= 400 {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("{name} send failed: {} {text}", status.as_u16()));
        }
        Ok(())
    }

    async fn send_resend(&self, http: &reqwest::Client, msg: &Outgoing<'_>) -> Result<(), String> {
        let (to, subject) = (msg.to, msg.subject);
        let mut payload = serde_json::json!({
            "from": format!("{} <{}>", msg.display, self.from_email),
            "to": [to],
            "subject": subject,
            "html": msg.html,
            "text": msg.text,
        });
        if let Some(reply) = msg.reply_to {
            payload["reply_to"] = Value::String(reply.mailbox());
        }
        self.post(http, "/emails", payload, "Resend").await?;
        tracing::info!(to, subject, "Email sent via Resend");
        Ok(())
    }

    async fn send_sendgrid(
        &self,
        http: &reqwest::Client,
        msg: &Outgoing<'_>,
    ) -> Result<(), String> {
        let (to, subject) = (msg.to, msg.subject);
        // SendGrid wants text/plain first when both are given.
        let mut payload = serde_json::json!({
            "personalizations": [{"to": [{"email": to}]}],
            "from": {"email": self.from_email, "name": msg.display},
            "subject": subject,
            "content": [
                {"type": "text/plain", "value": msg.text},
                {"type": "text/html", "value": msg.html},
            ],
        });
        if let Some(reply) = msg.reply_to {
            let mut field = serde_json::json!({"email": reply.address});
            if let Some(name) = &reply.name {
                field["name"] = Value::String(name.clone());
            }
            payload["reply_to"] = field;
        }
        self.post(http, "/v3/mail/send", payload, "SendGrid")
            .await?;
        tracing::info!(to, subject, "Email sent via SendGrid");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SMTP
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SmtpProvider {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub use_tls: bool,
    pub from_email: String,
}

impl SmtpProvider {
    pub fn from_config(config: &Map<String, Value>) -> Result<Self, String> {
        let port = match config.get("port") {
            Some(Value::Number(n)) => n.as_u64().and_then(|p| u16::try_from(p).ok()),
            Some(Value::String(s)) => s.trim().parse::<u16>().ok(),
            _ => None,
        }
        .ok_or_else(|| "invalid config field: port".to_string())?;
        Ok(SmtpProvider {
            host: req_str(config, "host")?,
            port,
            username: opt_str(config, "username"),
            password: opt_str(config, "password"),
            use_tls: config.get("use_tls").is_none_or(truthy),
            from_email: req_str(config, "from_email")?,
        })
    }

    async fn send(&self, msg: &Outgoing<'_>) -> Result<(), String> {
        let (to, subject) = (msg.to, msg.subject);
        let message = build_message(&self.from_email, msg)?;
        let mut builder = if self.use_tls {
            // Python: plain connect, then STARTTLS, then login.
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.host)
                .map_err(|e| format!("SMTP setup failed: {e}"))?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&self.host)
        }
        .port(self.port)
        .timeout(Some(Duration::from_secs(30)));
        if !self.username.is_empty() {
            builder = builder.credentials(Credentials::new(
                self.username.clone(),
                self.password.clone(),
            ));
        }
        builder
            .build()
            .send(message)
            .await
            .map_err(|e| format!("SMTP send failed: {e}"))?;
        tracing::info!(to, subject, "Email sent via SMTP");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Gmail
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GmailProvider {
    /// `token.json` written by `scripts/gmail_auth.py` (token, refresh_token,
    /// token_uri, client_id, client_secret, scopes, optional expiry).
    pub token_path: PathBuf,
    pub sender_email: String,
    /// Scheme and host of the Gmail API; tests point it at a local mock server.
    pub api_base: String,
}

/// Lexical `os.path.realpath` stand-in for paths that may not exist yet.
fn normalize(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

impl GmailProvider {
    pub fn new(token_path: impl Into<PathBuf>, sender_email: impl Into<String>) -> Self {
        GmailProvider {
            token_path: token_path.into(),
            sender_email: sender_email.into(),
            api_base: GMAIL_API_BASE.to_string(),
        }
    }

    /// Python `GmailProvider.from_settings`: `None` unless the token file
    /// exists and `GMAIL_SENDER_EMAIL` is set.
    pub fn from_settings(token_path: &str, sender_email: &str) -> Option<Self> {
        if !Path::new(token_path).exists() {
            tracing::warn!(path = token_path, "Gmail token not found");
            return None;
        }
        if sender_email.is_empty() {
            tracing::warn!("GMAIL_SENDER_EMAIL not set");
            return None;
        }
        Some(Self::new(token_path, sender_email))
    }

    /// Python `GmailProvider.from_config`: both paths must sit under
    /// `/app/config/gmail`.
    pub fn from_config(config: &Map<String, Value>) -> Result<Self, String> {
        for key in ["credentials_path", "token_path"] {
            let raw = req_str(config, key)?;
            let path = normalize(Path::new(&raw));
            if !path.starts_with(GMAIL_ALLOWED_DIR) {
                return Err(format!(
                    "Gmail {key} must be under {GMAIL_ALLOWED_DIR}, got: {}",
                    path.display()
                ));
            }
        }
        Ok(Self::new(
            req_str(config, "token_path")?,
            req_str(config, "sender_email")?,
        ))
    }

    fn read_token_file(&self) -> Result<Map<String, Value>, String> {
        let text = std::fs::read_to_string(&self.token_path).map_err(|_| {
            format!(
                "Gmail token not found at {}. Run 'python scripts/gmail_auth.py' to authorize.",
                self.token_path.display()
            )
        })?;
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err("Gmail credentials are invalid. Re-run gmail_auth.py.".into()),
        }
    }

    /// google-auth `Credentials.expired`: true once the stored expiry is less
    /// than the refresh threshold away. No expiry means never expired.
    fn expired(token: &Map<String, Value>) -> bool {
        let Some(raw) = token.get("expiry").and_then(Value::as_str) else {
            return false;
        };
        let trimmed = raw.trim_end_matches('Z');
        let base = trimmed.split('.').next().unwrap_or(trimmed);
        match chrono::NaiveDateTime::parse_from_str(base, "%Y-%m-%dT%H:%M:%S") {
            Ok(t) => {
                let expiry = t.and_utc();
                expiry - chrono::Duration::seconds(REFRESH_THRESHOLD_SECS) <= chrono::Utc::now()
            }
            Err(_) => true,
        }
    }

    /// Exchange the refresh token for a new access token and write it back to
    /// the token file, like `creds.refresh()` plus `token_path.write_text()`.
    async fn refresh(
        &self,
        http: &reqwest::Client,
        token: &mut Map<String, Value>,
    ) -> Result<(), String> {
        tracing::info!("Refreshing expired Gmail token");
        let token_uri = token
            .get("token_uri")
            .and_then(Value::as_str)
            .unwrap_or(GOOGLE_TOKEN_URI)
            .to_string();
        let form = [
            ("grant_type", "refresh_token".to_string()),
            ("client_id", opt_str(token, "client_id")),
            ("client_secret", opt_str(token, "client_secret")),
            ("refresh_token", opt_str(token, "refresh_token")),
        ];
        let resp = http
            .post(&token_uri)
            .form(&form)
            .timeout(HTTP_TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("Gmail token refresh failed: {e}"))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let reason = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(format!(
                "Gmail token refresh failed: {} {reason}",
                status.as_u16()
            ));
        }
        let access = body
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or("Gmail token refresh returned no access_token")?;
        token.insert("token".into(), Value::String(access.to_string()));
        if let Some(rt) = body.get("refresh_token").and_then(Value::as_str) {
            token.insert("refresh_token".into(), Value::String(rt.to_string()));
        }
        if let Some(secs) = body.get("expires_in").and_then(Value::as_i64) {
            let expiry = chrono::Utc::now() + chrono::Duration::seconds(secs);
            token.insert(
                "expiry".into(),
                Value::String(expiry.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),
            );
        }
        let text = serde_json::to_string(&Value::Object(token.clone()))
            .map_err(|e| format!("cannot encode Gmail token: {e}"))?;
        std::fs::write(&self.token_path, text)
            .map_err(|e| format!("cannot write Gmail token file: {e}"))?;
        Ok(())
    }

    async fn send(&self, http: &reqwest::Client, msg: &Outgoing<'_>) -> Result<(), String> {
        let (to, subject) = (msg.to, msg.subject);
        let mut token = self.read_token_file()?;
        let has_refresh = token
            .get("refresh_token")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        let has_token = token
            .get("token")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
        if (!has_token || Self::expired(&token)) && has_refresh {
            self.refresh(http, &mut token).await?;
        }
        if token
            .get("token")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || (Self::expired(&token) && !has_refresh)
        {
            return Err("Gmail credentials are invalid. Re-run gmail_auth.py.".into());
        }

        let message = build_message(&self.sender_email, msg)?;
        let raw = base64::engine::general_purpose::URL_SAFE.encode(message.formatted());
        let url = format!("{}/gmail/v1/users/me/messages/send", self.api_base);
        let payload = serde_json::json!({ "raw": raw });

        let mut retried = false;
        loop {
            let access = opt_str(&token, "token");
            let resp = http
                .post(&url)
                .bearer_auth(&access)
                .json(&payload)
                .timeout(HTTP_TIMEOUT)
                .send()
                .await
                .map_err(|e| format!("Gmail send failed: {e}"))?;
            let status = resp.status().as_u16();
            // The Google client refreshes once on a 401 and retries.
            if status == 401 && has_refresh && !retried {
                retried = true;
                self.refresh(http, &mut token).await?;
                continue;
            }
            if status >= 400 {
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("Gmail send failed: {status} {text}"));
            }
            let body: Value = resp.json().await.unwrap_or(Value::Null);
            let id = body.get("id").and_then(Value::as_str).unwrap_or("");
            tracing::info!(id, to, subject, "Email sent");
            return Ok(());
        }
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
    fn builds_from_records() {
        let p = Provider::from_record(
            "resend",
            &map(json!({"api_key": "k", "from_email": "a@b.c"})),
        )
        .unwrap();
        assert_eq!(p.provider_type(), "resend");
        let p = Provider::from_record(
            "smtp",
            &map(json!({"host": "h", "port": "2525", "from_email": "a@b.c", "use_tls": false})),
        )
        .unwrap();
        match p {
            Provider::Smtp(s) => {
                assert_eq!(s.port, 2525);
                assert!(!s.use_tls);
                assert_eq!(s.username, "");
            }
            _ => panic!("smtp"),
        }
        let Provider::Smtp(s) = Provider::from_record(
            "smtp",
            &map(json!({"host": "h", "port": 587, "from_email": "a@b.c"})),
        )
        .unwrap() else {
            panic!("smtp")
        };
        assert!(s.use_tls, "use_tls defaults to true");
        assert!(
            Provider::from_record("pigeon", &Map::new())
                .unwrap_err()
                .contains("Unknown email provider type")
        );
    }

    #[test]
    fn gmail_paths_must_stay_in_config_dir() {
        let ok = map(json!({
            "credentials_path": "/app/config/gmail/credentials.json",
            "token_path": "/app/config/gmail/token.json",
            "sender_email": "me@example.com"
        }));
        assert!(GmailProvider::from_config(&ok).is_ok());
        let bad = map(json!({
            "credentials_path": "/app/config/gmail/../../../etc/passwd",
            "token_path": "/app/config/gmail/token.json",
            "sender_email": "me@example.com"
        }));
        let err = GmailProvider::from_config(&bad).unwrap_err();
        assert_eq!(
            err,
            "Gmail credentials_path must be under /app/config/gmail, got: /etc/passwd"
        );
    }

    #[test]
    fn gmail_expiry() {
        assert!(!GmailProvider::expired(&map(json!({"token": "t"}))));
        assert!(GmailProvider::expired(&map(
            json!({"expiry": "2020-01-01T00:00:00Z"})
        )));
        assert!(GmailProvider::expired(&map(
            json!({"expiry": "2020-01-01T00:00:00.123456Z"})
        )));
        let later = (chrono::Utc::now() + chrono::Duration::hours(1))
            .format("%Y-%m-%dT%H:%M:%S%.6fZ")
            .to_string();
        assert!(!GmailProvider::expired(&map(json!({"expiry": later}))));
        let soon = (chrono::Utc::now() + chrono::Duration::seconds(60))
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string();
        assert!(
            GmailProvider::expired(&map(json!({"expiry": soon}))),
            "inside the refresh threshold"
        );
    }
}
