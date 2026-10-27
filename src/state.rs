//! Shared application state handed to every handler.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::Config;
use crate::limiter::{Counters, SlidingWindow};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    /// SSRF-safe client (`security::safe_http_client`) for every outgoing request.
    /// Tests can pass a plain client to reach local mock servers.
    pub http: reqwest::Client,
    /// Per-IP request limiter (Python: Redis `ratelimit:{ip}`).
    pub rate_limit: Arc<SlidingWindow>,
    /// Expiring counters (Python: Redis `auth_fail:{ip}` and email rate keys).
    pub counters: Arc<Counters>,
}

impl AppState {
    pub fn new(db: SqlitePool, config: Config, http: reqwest::Client) -> Self {
        AppState {
            db,
            config: Arc::new(config),
            http,
            rate_limit: Arc::new(SlidingWindow::default()),
            counters: Arc::new(Counters::default()),
        }
    }
}
