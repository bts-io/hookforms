//! API key authentication (Python: `app/auth.py`).
//!
//! - the admin key (`ADMIN_API_KEY`) has every scope,
//! - database keys are PBKDF2-SHA256 hashes in passlib's
//!   `$pbkdf2-sha256$<rounds>$<salt>$<checksum>` format (so keys imported from
//!   a Python install keep working), looked up by `key_prefix` (first 12
//!   characters), with the Python fallback to rows that have no prefix yet,
//! - 10 failed attempts per client IP within 300 s lock that IP out (429),
//!   counted in `AppState::counters` under `auth_fail:{ip}`.
//!
//! Handlers add `_key: RequireWebhooks` (or `RequireAdmin`) as an argument.

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};

use crate::db;
use crate::error::AppError;
use crate::middleware::client_ip;
use crate::state::AppState;

pub const ALL_SCOPES: &[&str] = &["webhooks", "admin"];

pub const LOCKOUT_THRESHOLD: u64 = 10;
pub const LOCKOUT_WINDOW: Duration = Duration::from_secs(300);
/// passlib's default for `pbkdf2_sha256`.
pub const PBKDF2_ROUNDS: u32 = 29_000;
const PREFIX_LEN: usize = 12;

/// The authenticated caller. `id` is `None` for the admin key.
#[derive(Debug, Clone)]
pub struct AuthKey {
    pub id: Option<String>,
    pub name: String,
    pub scopes: Vec<String>,
}

impl AuthKey {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == "admin" || s == scope)
    }
}

/// Constant-time string comparison (Python: `secrets.compare_digest`).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

/// passlib's "adapted base64": standard alphabet with `.` for `+`, no padding.
fn ab64_encode(bytes: &[u8]) -> String {
    STANDARD_NO_PAD.encode(bytes).replace('+', ".")
}

fn ab64_decode(s: &str) -> Option<Vec<u8>> {
    STANDARD_NO_PAD.decode(s.replace('.', "+")).ok()
}

fn pbkdf2(key: &str, salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(key.as_bytes(), salt, rounds, &mut out);
    out
}

/// Python `hash_key` (passlib `pbkdf2_sha256.hash`, 16-byte salt).
pub fn hash_key(key: &str) -> String {
    let mut salt = [0u8; 16];
    getrandom::getrandom(&mut salt).expect("random salt");
    let digest = pbkdf2(key, &salt, PBKDF2_ROUNDS);
    format!(
        "$pbkdf2-sha256${PBKDF2_ROUNDS}${}${}",
        ab64_encode(&salt),
        ab64_encode(&digest)
    )
}

/// Python `verify_key`. Malformed hashes never match.
pub fn verify_key(key: &str, key_hash: &str) -> bool {
    let parts: Vec<&str> = key_hash.split('$').collect();
    let ["", "pbkdf2-sha256", rounds, salt, sum] = parts.as_slice() else {
        return false;
    };
    let (Ok(rounds), Some(salt), Some(sum)) =
        (rounds.parse::<u32>(), ab64_decode(salt), ab64_decode(sum))
    else {
        return false;
    };
    if rounds == 0 || sum.len() != 32 {
        return false;
    }
    let digest = pbkdf2(key, &salt, rounds);
    let mut diff = 0u8;
    for (a, b) in digest.iter().zip(sum.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// Python `generate_key`: `hf_` + `secrets.token_urlsafe(32)` (43 characters).
pub fn generate_key() -> String {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("random key");
    format!("hf_{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// The lookup prefix: the first 12 characters (Python `api_key[:12]`).
pub fn key_prefix(key: &str) -> String {
    key.chars().take(PREFIX_LEN).collect()
}

fn fail_key(ip: &str) -> String {
    format!("auth_fail:{ip}")
}

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: String,
    name: String,
    key_hash: String,
    key_prefix: Option<String>,
    scopes: Option<String>,
}

/// Python `get_current_key`: resolve the `X-API-Key` header to a key.
pub async fn current_key(parts: &Parts, state: &AppState) -> Result<AuthKey, AppError> {
    let api_key = parts
        .headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if api_key.is_empty() {
        return Err(AppError::unauthorized("Missing API key"));
    }

    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let ip = client_ip(&parts.headers, peer);
    if state.counters.get(&fail_key(&ip)) >= LOCKOUT_THRESHOLD {
        return Err(AppError::too_many(
            "Too many failed authentication attempts. Try again later.",
        ));
    }

    if constant_time_eq(api_key, &state.config.admin_api_key) {
        state.counters.remove(&fail_key(&ip));
        return Ok(AuthKey {
            id: None,
            name: "admin".into(),
            scopes: ALL_SCOPES.iter().map(|s| s.to_string()).collect(),
        });
    }

    let prefix = key_prefix(api_key);
    let cols = "SELECT id, name, key_hash, key_prefix, scopes FROM api_keys WHERE is_active = 1";
    let mut rows: Vec<KeyRow> = sqlx::query_as(&format!("{cols} AND key_prefix = ?"))
        .bind(&prefix)
        .fetch_all(&state.db)
        .await?;
    if rows.is_empty() {
        rows = sqlx::query_as(&format!("{cols} AND key_prefix IS NULL"))
            .fetch_all(&state.db)
            .await?;
    }

    // PBKDF2 is deliberately slow: keep it off the async workers.
    let candidate = api_key.to_string();
    let found = tokio::task::spawn_blocking(move || {
        rows.into_iter()
            .find(|row| verify_key(&candidate, &row.key_hash))
    })
    .await
    .map_err(AppError::internal)?;

    if let Some(row) = found {
        state.counters.remove(&fail_key(&ip));
        if row.key_prefix.as_deref().is_none_or(str::is_empty) {
            sqlx::query("UPDATE api_keys SET last_used_at = ?, key_prefix = ? WHERE id = ?")
                .bind(db::now())
                .bind(&prefix)
                .bind(&row.id)
                .execute(&state.db)
                .await?;
        } else {
            sqlx::query("UPDATE api_keys SET last_used_at = ? WHERE id = ?")
                .bind(db::now())
                .bind(&row.id)
                .execute(&state.db)
                .await?;
        }
        let scopes = row
            .scopes
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default();
        return Ok(AuthKey {
            id: Some(row.id),
            name: row.name,
            scopes,
        });
    }

    state.counters.incr(&fail_key(&ip), LOCKOUT_WINDOW);
    tracing::warn!("Failed auth attempt from {ip}");
    Err(AppError::unauthorized("Invalid API key"))
}

async fn require(parts: &Parts, state: &AppState, scope: &str) -> Result<AuthKey, AppError> {
    let key = current_key(parts, state).await?;
    if key.has_scope(scope) {
        Ok(key)
    } else {
        Err(AppError::forbidden(format!(
            "Key lacks required scope: {scope}"
        )))
    }
}

/// Extractor for routes that need the `webhooks` scope.
pub struct RequireWebhooks(pub AuthKey);

/// Extractor for routes that need the `admin` scope.
pub struct RequireAdmin(pub AuthKey);

impl FromRequestParts<AppState> for RequireWebhooks {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        require(parts, state, "webhooks").await.map(RequireWebhooks)
    }
}

impl FromRequestParts<AppState> for RequireAdmin {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, AppError> {
        require(parts, state, "admin").await.map(RequireAdmin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trip() {
        let key = generate_key();
        assert!(key.starts_with("hf_"));
        assert_eq!(key.len(), 46);
        let h = hash_key(&key);
        assert!(h.starts_with("$pbkdf2-sha256$29000$"));
        assert!(verify_key(&key, &h));
        assert!(!verify_key("hf_other", &h));
        assert!(!verify_key(&key, "garbage"));
        assert!(!verify_key(&key, ""));
    }

    /// A hash in passlib's format, built with Python's hashlib:
    /// `hashlib.pbkdf2_hmac("sha256", b"hf_passlib-compat", b"0123456789abcdef", 1000)`,
    /// salt and digest in passlib's ab64 (note the `.` standing for `+`).
    #[test]
    fn verifies_passlib_hashes() {
        let h = "$pbkdf2-sha256$1000$MDEyMzQ1Njc4OWFiY2RlZg$RoWBFpu.pyH9zxbXd0a.qWRJpF8IY0qofcfg9t/O688";
        assert!(verify_key("hf_passlib-compat", h));
        assert!(!verify_key("hf_passlib-compat2", h));
    }

    #[test]
    fn prefix_is_twelve_chars() {
        assert_eq!(key_prefix("hf_abcdefghijklmnop"), "hf_abcdefghi");
        assert_eq!(key_prefix("short"), "short");
    }
}
