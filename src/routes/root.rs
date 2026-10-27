//! `/` and `/health` (Python: `app/main.py`).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Timelike, Utc};
use serde_json::{Value, json};

use crate::state::AppState;

/// Version reported by `/` and `/health`. The Python version reported "0.1.0".
pub const API_VERSION: &str = "0.2.0";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(root).head(crate::error::method_not_allowed))
        .route(
            "/health",
            get(health).head(crate::error::method_not_allowed),
        )
}

async fn root(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "name": state.config.app_name, "status": "ok", "version": API_VERSION }))
}

/// Python's `datetime.now(timezone.utc).isoformat()`: microseconds only when non-zero.
pub fn python_isoformat(ts: DateTime<Utc>) -> String {
    if ts.nanosecond() / 1000 == 0 {
        ts.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        ts.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

/// Always 200, like Python. `status` is "degraded" when the database check fails.
/// Python reported `checks.postgres` and `checks.redis`; this version has one
/// SQLite database and no Redis, so it reports `checks.database`.
async fn health(State(state): State<AppState>) -> Json<Value> {
    let db_ok = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    Json(json!({
        "status": if db_ok { "healthy" } else { "degraded" },
        "timestamp": python_isoformat(Utc::now()),
        "version": API_VERSION,
        "checks": { "database": if db_ok { "ok" } else { "unavailable" } },
    }))
}
