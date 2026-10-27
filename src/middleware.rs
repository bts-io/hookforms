//! Rate limit, security headers and body size limit, ported from
//! `selfhosted/api/app/middleware.py`, plus the client IP helper.
//!
//! Layer order matches Starlette's: unhandled-error stripping (outermost) >
//! rate limit > size limit > CORS > routes.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::{Unhandled, error_body};
use crate::state::AppState;

pub const RATE_LIMIT: usize = 100;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
pub const MAX_BODY_SIZE: u64 = 2 * 1024 * 1024;

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("x-content-type-options", "nosniff"),
    ("x-frame-options", "DENY"),
    ("x-xss-protection", "1; mode=block"),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    (
        "strict-transport-security",
        "max-age=31536000; includeSubDomains",
    ),
];

fn add_security_headers(headers: &mut HeaderMap) {
    for (k, v) in SECURITY_HEADERS {
        headers.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
    }
}

/// Client IP like Python's `_get_client_ip`: CF-Connecting-IP, then the first
/// X-Forwarded-For entry, then the peer address, else "unknown".
pub fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
    };
    if let Some(v) = header("cf-connecting-ip") {
        return v.to_string();
    }
    if let Some(v) = header("x-forwarded-for") {
        return v.split(',').next().unwrap_or("").trim().to_string();
    }
    peer.map(|a| a.ip().to_string())
        .unwrap_or_else(|| "unknown".into())
}

/// Client IP of a request, reading the peer address from `ConnectInfo` when present.
pub fn request_client_ip(req: &Request) -> String {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    client_ip(req.headers(), peer)
}

pub async fn rate_limit(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/health" {
        let mut resp = next.run(req).await;
        add_security_headers(resp.headers_mut());
        return resp;
    }

    let ip = request_client_ip(&req);
    let current = state
        .rate_limit
        .hit(&format!("ratelimit:{ip}"), RATE_WINDOW);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let reset_at = ((now + RATE_WINDOW.as_secs_f64()) as u64).to_string();

    if current >= RATE_LIMIT {
        let status = StatusCode::TOO_MANY_REQUESTS;
        let mut resp = (
            status,
            Json(error_body(status, "Too many requests. Please retry later.")),
        )
            .into_response();
        let h = resp.headers_mut();
        add_security_headers(h);
        h.insert("x-ratelimit-limit", HeaderValue::from(RATE_LIMIT));
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        h.insert(
            "x-ratelimit-reset",
            HeaderValue::from_str(&reset_at).expect("digits"),
        );
        return resp;
    }

    let remaining = RATE_LIMIT.saturating_sub(current + 1);
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert("x-ratelimit-limit", HeaderValue::from(RATE_LIMIT));
    h.insert("x-ratelimit-remaining", HeaderValue::from(remaining));
    h.insert(
        "x-ratelimit-reset",
        HeaderValue::from_str(&reset_at).expect("digits"),
    );
    add_security_headers(h);
    resp
}

pub async fn size_limit(req: Request, next: Next) -> Response {
    if req.uri().path() != "/health" {
        let len = req
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if len.is_some_and(|n| n > MAX_BODY_SIZE) {
            let status = StatusCode::PAYLOAD_TOO_LARGE;
            let msg = format!(
                "Request body too large. Max size is {} MB.",
                MAX_BODY_SIZE / (1024 * 1024)
            );
            return (status, Json(error_body(status, msg))).into_response();
        }
    }
    next.run(req).await
}

/// Starlette's `ServerErrorMiddleware`: an unhandled error is answered outside
/// every other middleware, so only `content-type` (and the length) is sent.
pub async fn unhandled_error(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    if resp.extensions().get::<Unhandled>().is_some() {
        let keep = resp
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .cloned();
        let headers = resp.headers_mut();
        headers.clear();
        if let Some(ct) = keep {
            headers.insert(axum::http::header::CONTENT_TYPE, ct);
        }
    }
    resp
}
