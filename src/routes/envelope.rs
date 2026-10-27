//! Response envelopes (Python: `app/response.py`).

use serde::Serialize;
use serde_json::{Value, json};

/// `{"data": [...], "meta": {"total", "limit", "offset"}}`
pub fn paginated<T: Serialize>(items: &[T], total: i64, limit: i64, offset: i64) -> Value {
    json!({
        "data": items,
        "meta": { "total": total, "limit": limit, "offset": offset },
    })
}

/// FastAPI's `status_code=204` routes: no body, but the default response
/// class still sends `content-type: application/json`.
pub fn no_content() -> axum::response::Response {
    use axum::http::{HeaderValue, StatusCode, header};
    use axum::response::IntoResponse;
    (
        StatusCode::NO_CONTENT,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
    )
        .into_response()
}

/// `{"data": item}`
pub fn single<T: Serialize>(item: &T) -> Value {
    json!({ "data": item })
}
