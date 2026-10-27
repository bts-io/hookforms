//! CORS, ported from Starlette's `CORSMiddleware` as the Python app configures
//! it: `allow_origins` from `CORS_ORIGINS` (or `["*"]` when unset),
//! `allow_credentials=True`, `allow_methods=["*"]`, `allow_headers=["*"]`.
//!
//! - No `Origin`: the request passes through untouched (no CORS headers at all).
//! - `OPTIONS` with `Origin` and `Access-Control-Request-Method`: a preflight,
//!   answered here with `OK` (or 400 `Disallowed CORS ...`) as text/plain.
//! - Any other request with `Origin`: the response gets
//!   `Access-Control-Allow-Origin: *` (or the echoed origin when a specific
//!   list is configured, or when cookies are sent) and
//!   `Access-Control-Allow-Credentials: true`.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::state::AppState;

/// Starlette's `ALL_METHODS`, sent as `Access-Control-Allow-Methods`.
const ALL_METHODS: &[&str] = &["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"];

/// The origins policy built from `CORS_ORIGINS` like `main.py` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origins {
    allow_all: bool,
    list: Vec<String>,
}

impl Origins {
    /// Python: an empty setting means `["*"]`; otherwise the comma-separated,
    /// trimmed, non-empty entries (which may be empty, allowing nothing).
    pub fn from_setting(raw: &str) -> Self {
        if raw.is_empty() {
            return Origins {
                allow_all: true,
                list: vec!["*".into()],
            };
        }
        let list: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(String::from)
            .collect();
        Origins {
            allow_all: list.iter().any(|o| o == "*"),
            list,
        }
    }

    fn is_allowed(&self, origin: &str) -> bool {
        self.allow_all || self.list.iter().any(|o| o == origin)
    }
}

fn set(headers: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        headers.insert(name, v);
    }
}

/// Starlette's `MutableHeaders.add_vary_header`.
fn add_vary_origin(headers: &mut HeaderMap) {
    let vary = match headers.get(header::VARY).and_then(|v| v.to_str().ok()) {
        Some(existing) => format!("{existing}, Origin"),
        None => "Origin".to_string(),
    };
    set(headers, "vary", &vary);
}

/// Starlette's `preflight_response`.
pub fn preflight(origins: &Origins, req_headers: &HeaderMap) -> Response {
    let text = |name: header::HeaderName| {
        req_headers
            .get(name)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
    };
    let origin = text(header::ORIGIN).unwrap_or_default();
    let method = text(header::ACCESS_CONTROL_REQUEST_METHOD).unwrap_or_default();
    let requested_headers = text(header::ACCESS_CONTROL_REQUEST_HEADERS);

    let mut headers = HeaderMap::new();
    // allow_credentials is always on, so the origin is always explicit.
    set(&mut headers, "vary", "Origin");
    set(
        &mut headers,
        "access-control-allow-methods",
        &ALL_METHODS.join(", "),
    );
    set(&mut headers, "access-control-max-age", "600");
    set(&mut headers, "access-control-allow-credentials", "true");

    let mut failures = Vec::new();
    if origins.is_allowed(&origin) {
        set(&mut headers, "access-control-allow-origin", &origin);
    } else {
        failures.push("origin");
    }
    if !ALL_METHODS.contains(&method.as_str()) {
        failures.push("method");
    }
    if let Some(h) = requested_headers {
        set(&mut headers, "access-control-allow-headers", &h);
    }

    let (status, body) = if failures.is_empty() {
        (StatusCode::OK, "OK".to_string())
    } else {
        (
            StatusCode::BAD_REQUEST,
            format!("Disallowed CORS {}", failures.join(", ")),
        )
    };
    set(&mut headers, "content-type", "text/plain; charset=utf-8");
    let mut resp = (status, Body::from(body)).into_response();
    resp.headers_mut().extend(headers);
    resp
}

/// Starlette's `simple_response` headers, added to a response.
pub fn add_simple_headers(origins: &Origins, req_headers: &HeaderMap, resp: &mut Response) {
    let origin = req_headers
        .get(header::ORIGIN)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .unwrap_or_default();
    let headers = resp.headers_mut();
    if origins.allow_all {
        set(headers, "access-control-allow-origin", "*");
    }
    set(headers, "access-control-allow-credentials", "true");
    let has_cookie = req_headers.contains_key(header::COOKIE);
    if (origins.allow_all && has_cookie) || (!origins.allow_all && origins.is_allowed(&origin)) {
        set(headers, "access-control-allow-origin", &origin);
        add_vary_origin(headers);
    }
}

/// The CORS middleware (sits inside the size limit, outside the routes).
pub async fn cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if !req.headers().contains_key(header::ORIGIN) {
        return next.run(req).await;
    }
    let origins = Origins::from_setting(&state.config.cors_origins);
    if req.method() == Method::OPTIONS
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        return preflight(&origins, req.headers());
    }
    let req_headers = req.headers().clone();
    let mut resp = next.run(req).await;
    add_simple_headers(&origins, &req_headers, &mut resp);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn origins_setting_like_python() {
        assert!(Origins::from_setting("").allow_all);
        let o = Origins::from_setting(" https://a.example , ,https://b.example");
        assert!(!o.allow_all);
        assert!(o.is_allowed("https://b.example"));
        assert!(!o.is_allowed("https://c.example"));
        // A non-empty setting with no entries allows nothing.
        assert!(!Origins::from_setting(" , ").is_allowed("https://a.example"));
        assert!(Origins::from_setting("https://a.example,*").allow_all);
    }

    #[test]
    fn preflight_rejects_unknown_origin_and_method() {
        let o = Origins::from_setting("https://a.example");
        let resp = preflight(
            &o,
            &headers(&[
                ("origin", "https://x.example"),
                ("access-control-request-method", "BREW"),
            ]),
        );
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(!resp.headers().contains_key("access-control-allow-origin"));
        assert_eq!(resp.headers()["vary"], "Origin");
    }

    #[test]
    fn simple_headers_echo_with_cookie_or_list() {
        let all = Origins::from_setting("");
        let mut resp = Response::new(Body::empty());
        add_simple_headers(
            &all,
            &headers(&[("origin", "https://s.example")]),
            &mut resp,
        );
        assert_eq!(resp.headers()["access-control-allow-origin"], "*");
        assert!(!resp.headers().contains_key("vary"));

        let mut resp = Response::new(Body::empty());
        add_simple_headers(
            &all,
            &headers(&[("origin", "https://s.example"), ("cookie", "a=1")]),
            &mut resp,
        );
        assert_eq!(
            resp.headers()["access-control-allow-origin"],
            "https://s.example"
        );
        assert_eq!(resp.headers()["vary"], "Origin");

        let list = Origins::from_setting("https://s.example");
        let mut resp = Response::new(Body::empty());
        add_simple_headers(
            &list,
            &headers(&[("origin", "https://z.example")]),
            &mut resp,
        );
        assert!(!resp.headers().contains_key("access-control-allow-origin"));
        assert_eq!(resp.headers()["access-control-allow-credentials"], "true");
    }
}
