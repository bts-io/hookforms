//! Errors with the same JSON bodies the Python (FastAPI) version returns.
//!
//! The Python app installs its own handlers, so there are two shapes:
//! - anything the app raises (`HTTPException`, validation, unhandled errors):
//!   `{"error": {"code": 404, "message": "Inbox not found"}}`, and for
//!   validation errors also `"details": [...]` with message "Validation error";
//! - routing errors from Starlette itself (no route matched, wrong method),
//!   which bypass those handlers: `{"detail": "Not Found"}` and
//!   `{"detail": "Method Not Allowed"}`. See `not_found` / `method_not_allowed`.

use axum::Json;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

#[derive(Debug)]
pub enum AppError {
    /// FastAPI `HTTPException(status_code, detail)`. `detail` is usually a string.
    Http { status: StatusCode, message: Value },
    /// FastAPI `RequestValidationError`: 422 with a `details` list in pydantic's shape
    /// (`[{"type": ..., "loc": [...], "msg": ..., "input": ...}]`).
    Validation(Vec<Value>),
    /// Anything unexpected. Logged, answered as a plain 500.
    Internal(String),
}

pub type AppResult<T> = Result<T, AppError>;

/// Marks the response of an unhandled error. Python answers those from
/// Starlette's `ServerErrorMiddleware`, outside every other middleware, so the
/// outermost layer (`middleware::unhandled_error`) drops the headers the inner
/// layers added (security, rate limit, CORS).
#[derive(Debug, Clone, Copy)]
pub struct Unhandled;

impl AppError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        AppError::Http {
            status,
            message: Value::String(message.into()),
        }
    }
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    pub fn too_many(message: impl Into<String>) -> Self {
        Self::new(StatusCode::TOO_MANY_REQUESTS, message)
    }
    pub fn internal(e: impl std::fmt::Display) -> Self {
        AppError::Internal(e.to_string())
    }
}

/// Body for an app-level error: `{"error": {"code": N, "message": ...}}`.
pub fn error_body(status: StatusCode, message: impl Into<Value>) -> Value {
    json!({ "error": { "code": status.as_u16(), "message": message.into() } })
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::Http { status, message } => {
                (status, Json(error_body(status, message))).into_response()
            }
            AppError::Validation(details) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({
                    "error": { "code": 422, "message": "Validation error", "details": details }
                })),
            )
                .into_response(),
            AppError::Internal(msg) => {
                tracing::error!(error = %msg, "unhandled error");
                let status = StatusCode::INTERNAL_SERVER_ERROR;
                let mut resp =
                    (status, Json(error_body(status, "Internal server error"))).into_response();
                resp.extensions_mut().insert(Unhandled);
                resp
            }
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        AppError::Internal(format!("database: {e}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Internal(format!("json: {e}"))
    }
}

/// Router fallback: Starlette's `redirect_slashes` (307 to the same path
/// without its trailing slashes when that path is a route, for any method),
/// else Starlette's own 404 body.
pub async fn not_found(OriginalUri(uri): OriginalUri, headers: HeaderMap) -> Response {
    if let Some(location) = slash_redirect(&uri, &headers) {
        let mut resp = StatusCode::TEMPORARY_REDIRECT.into_response();
        if let Ok(v) = HeaderValue::from_str(&location) {
            resp.headers_mut().insert(header::LOCATION, v);
        }
        return resp;
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "detail": "Not Found" })),
    )
        .into_response()
}

/// The Python app's routes in the order FastAPI registers them, one entry per
/// endpoint, with that endpoint's methods. Starlette answers a wrong method
/// with the `Allow` of the FIRST route whose path matches, so a path served by
/// several endpoints lists only the first one's method (`PUT /v1/auth/keys`
/// gets `Allow: POST`). `/hooks/{slug}` keeps the decorator's order; Python
/// joins a set there, so its order changes with the hash seed.
const PY_ROUTES: &[(&str, &str)] = &[
    ("/v1/auth/keys", "POST"),
    ("/v1/auth/keys", "GET"),
    ("/v1/auth/keys/{key_id}", "DELETE"),
    ("/v1/hooks/inboxes", "GET"),
    ("/v1/hooks/inboxes", "POST"),
    ("/v1/hooks/inboxes/{slug}", "PATCH"),
    ("/v1/hooks/inboxes/{slug}", "DELETE"),
    ("/v1/hooks/{slug}/events", "GET"),
    ("/v1/hooks/{slug}/events/{event_id}", "GET"),
    ("/v1/hooks/inboxes/{slug}/channels", "POST"),
    ("/v1/hooks/inboxes/{slug}/channels", "GET"),
    ("/v1/hooks/inboxes/{slug}/channels/{channel_id}", "PATCH"),
    ("/v1/hooks/inboxes/{slug}/channels/{channel_id}", "DELETE"),
    ("/v1/hooks/config/email-provider", "GET"),
    ("/v1/hooks/config/email-provider", "PUT"),
    ("/v1/hooks/config/email-provider", "DELETE"),
    ("/hooks/{slug}", "GET, POST, PUT, PATCH, DELETE"),
    ("/", "GET"),
    ("/health", "GET"),
];

/// Where Starlette redirects an unmatched path ending in `/`: the path with
/// its trailing slashes removed, as an absolute URL built from the `Host`
/// header (plain http, like uvicorn without trusted proxy headers).
fn slash_redirect(uri: &Uri, headers: &HeaderMap) -> Option<String> {
    let path = uri.path();
    if path == "/" || !path.ends_with('/') {
        return None;
    }
    let stripped = path.trim_end_matches('/');
    if !PY_ROUTES.iter().any(|(p, _)| path_matches(p, stripped)) {
        return None;
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or_else(|| uri.authority().map(|a| a.to_string()));
    let mut url = match host {
        Some(h) => format!("http://{h}{stripped}"),
        None => stripped.to_string(),
    };
    if let Some(q) = uri.query().filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    Some(url)
}

/// Starlette path matching: literal segments match exactly, `{param}` matches
/// one non-empty segment.
fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pat = pattern.split('/');
    let mut got = path.split('/');
    loop {
        match (pat.next(), got.next()) {
            (None, None) => return true,
            (Some(p), Some(g)) if p.starts_with('{') => {
                if g.is_empty() {
                    return false;
                }
            }
            (Some(p), Some(g)) if p == g => {}
            _ => return false,
        }
    }
}

/// The `Allow` value Starlette sends for a wrong method on `path`.
fn starlette_allow(path: &str) -> Option<&'static str> {
    PY_ROUTES
        .iter()
        .find(|(pattern, _)| path_matches(pattern, path))
        .map(|(_, methods)| *methods)
}

/// Wrong method on a known path: Starlette's own 405 body and `Allow` header.
pub async fn method_not_allowed(OriginalUri(uri): OriginalUri) -> Response {
    let mut resp = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({ "detail": "Method Not Allowed" })),
    )
        .into_response();
    if let Some(allow) = starlette_allow(uri.path()) {
        resp.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static(allow));
    }
    resp
}
