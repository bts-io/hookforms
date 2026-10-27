//! SSRF protection for outgoing requests to user-supplied URLs, ported from
//! `selfhosted/api/app/security.py`.
//!
//! - `is_safe_url(url)`: the upfront check with the same messages as Python.
//! - `safe_http_client()`: a reqwest client whose DNS resolver refuses private
//!   and reserved addresses and whose redirect policy refuses blocked hosts and
//!   IP literals, like Python's `SSRFSafeTransport`. It is `AppState::http`.
//!
//! Shared by step 3 (`forward_url`) and step 4 (channel senders); neither step
//! needs to change this file.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

const BLOCKED_HOSTNAMES: &[&str] = &[
    "localhost",
    "postgres",
    "redis",
    "api",
    "worker",
    "cloudflared",
    "nginx",
    "metadata",
    "metadata.google.internal",
];

/// True for the networks Python blocks: 10/8, 172.16/12, 192.168/16, 127/8,
/// 169.254/16, 0/8, ::1, fc00::/7, fe80::/10. IPv4-mapped IPv6 addresses are
/// checked as their IPv4 form (Python let those through; this is stricter).
pub fn is_ip_blocked(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 10
                || (o[0] == 172 && (16..=31).contains(&o[1]))
                || (o[0] == 192 && o[1] == 168)
                || o[0] == 127
                || (o[0] == 169 && o[1] == 254)
                || o[0] == 0
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            v6.is_loopback() || (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80
        }
    }
}

pub fn is_blocked_hostname(host: &str) -> bool {
    BLOCKED_HOSTNAMES.contains(&host.to_ascii_lowercase().as_str())
}

/// Host of a parsed URL without IPv6 brackets, lowercased like Python's `hostname`.
fn url_host(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Upfront URL check. `Ok(())` when safe, else the same reason text Python returns.
pub async fn is_safe_url(url: &str) -> Result<(), String> {
    let parsed = match Url::parse(url) {
        Ok(u) => u,
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            return Err("Scheme '' not allowed. Use http or https.".into());
        }
        Err(_) => return Err("Invalid URL".into()),
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "Scheme '{}' not allowed. Use http or https.",
            parsed.scheme()
        ));
    }
    let Some(host) = url_host(&parsed) else {
        return Err("URL has no hostname".into());
    };
    if is_blocked_hostname(&host) {
        return Err(format!("Hostname '{host}' is not allowed"));
    }
    match tokio::net::lookup_host((host.as_str(), 0)).await {
        Ok(addrs) => {
            for a in addrs {
                if is_ip_blocked(a.ip()) {
                    return Err("URL resolves to private/reserved IP address".into());
                }
            }
            Ok(())
        }
        Err(_) => Err("Cannot resolve hostname".into()),
    }
}

/// Test-only: when `HOOKFORMS_TEST_UPSTREAM_BASE` is set (for example
/// `http://198.18.0.10:8080`), the fixed third-party endpoints (Turnstile
/// siteverify, Resend, SendGrid) are sent to that base instead, keeping their
/// paths. Used by the Python comparison harness in `compare/`. Unset in any
/// real deployment, which keeps the real endpoints.
pub fn test_upstream_base() -> Option<String> {
    std::env::var("HOOKFORMS_TEST_UPSTREAM_BASE")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
}

/// DNS resolver that refuses names resolving to blocked addresses.
#[derive(Debug, Default)]
struct SafeResolver;

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let host = name.as_str().to_string();
            if is_blocked_hostname(&host) {
                return Err(format!("Blocked hostname: {host}").into());
            }
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|_| format!("Cannot resolve hostname: {host}"))?
                .collect();
            if addrs.iter().any(|a| is_ip_blocked(a.ip())) {
                return Err(format!("DNS resolved to blocked IP for {host}").into());
            }
            let iter: Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

/// IP literals skip DNS, so the redirect policy and `check_target` cover them.
fn target_blocked(url: &Url) -> bool {
    match url_host(url) {
        None => true,
        Some(h) => is_blocked_hostname(&h) || h.parse::<IpAddr>().is_ok_and(is_ip_blocked),
    }
}

/// Cheap synchronous check for a URL about to be requested with the safe client:
/// refuses blocked hostnames and private IP literals (names are checked by the resolver).
pub fn check_target(url: &Url) -> Result<(), String> {
    if target_blocked(url) {
        Err(format!("Blocked target: {}", url.host_str().unwrap_or("")))
    } else {
        Ok(())
    }
}

/// Client for outgoing requests (15 s timeout). It never follows redirects by
/// itself, like httpx's default; requests to user-supplied URLs go through
/// [`send_following`], which follows them the way httpx does.
pub fn safe_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(SafeResolver))
        .user_agent(concat!("HookForms/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}

/// httpx's `DEFAULT_MAX_REDIRECTS`.
pub const MAX_REDIRECTS: usize = 20;

/// httpx's same-origin test for keeping `Authorization`: same scheme, host and
/// port, or an upgrade from http on port 80 to https on port 443.
fn same_origin(a: &Url, b: &Url) -> bool {
    let same = a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default();
    let https_upgrade = a.scheme() == "http"
        && b.scheme() == "https"
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == Some(80)
        && b.port_or_known_default() == Some(443);
    same || https_upgrade
}

/// Send a request to a user-supplied URL and follow redirects like httpx with
/// `follow_redirects=True`: 303 (and 301/302 after a POST) switch to GET and
/// drop the body, `Content-Length` and `Transfer-Encoding` but keep the other
/// headers (`Content-Type` included); no `Referer` is added; `Authorization` is
/// dropped across origins; at most [`MAX_REDIRECTS`] hops. Every hop is
/// checked against blocked hosts (names are checked by the resolver).
pub async fn send_following(
    http: &reqwest::Client,
    mut req: reqwest::Request,
) -> Result<reqwest::Response, String> {
    use reqwest::Method;
    use reqwest::StatusCode;
    use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, LOCATION, TRANSFER_ENCODING};

    for _ in 0..=MAX_REDIRECTS {
        check_target(req.url())?;
        let next = req.try_clone();
        let resp = http.execute(req).await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let is_redirect = matches!(
            status,
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        );
        let location = resp
            .headers()
            .get(LOCATION)
            .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned());
        let (Some(location), Some(mut next)) = (location.filter(|_| is_redirect), next) else {
            return Ok(resp);
        };
        let url = next
            .url()
            .join(&location)
            .map_err(|e| format!("invalid redirect location: {e}"))?;
        let method = match (status, next.method()) {
            (StatusCode::SEE_OTHER, m) if *m != Method::HEAD => Method::GET,
            (StatusCode::FOUND | StatusCode::MOVED_PERMANENTLY, m) if *m == Method::POST => {
                Method::GET
            }
            (_, m) => m.clone(),
        };
        if !same_origin(next.url(), &url) {
            next.headers_mut().remove(AUTHORIZATION);
        }
        if method != *next.method() && method == Method::GET {
            next.headers_mut().remove(CONTENT_LENGTH);
            next.headers_mut().remove(TRANSFER_ENCODING);
            *next.body_mut() = None;
        }
        *next.method_mut() = method;
        *next.url_mut() = url;
        req = next;
    }
    Err("too many redirects".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_ranges() {
        for ip in [
            "10.1.2.3",
            "172.20.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.169.254",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_ip_blocked(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "172.32.0.1", "2606:4700::1111"] {
            assert!(!is_ip_blocked(ip.parse().unwrap()), "{ip}");
        }
    }

    /// A POST answered with 302 is followed as a GET that keeps Content-Type
    /// and custom headers, drops the body and adds no Referer (httpx), and a
    /// 307 keeps method and body.
    #[tokio::test]
    async fn redirects_follow_httpx_rules() {
        use axum::Router;
        use axum::body::Bytes;
        use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
        use axum::response::IntoResponse;
        use std::sync::Mutex;

        type Seen = Arc<Mutex<Vec<(Method, String, HeaderMap, Bytes)>>>;
        let seen: Seen = Arc::default();
        let record = {
            let seen = seen.clone();
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let seen = seen.clone();
                async move {
                    let path = uri.path().to_string();
                    seen.lock()
                        .unwrap()
                        .push((method, path.clone(), headers, body));
                    match path.as_str() {
                        "/found" => {
                            (StatusCode::FOUND, [(header::LOCATION, "/landed")]).into_response()
                        }
                        "/temp" => (
                            StatusCode::TEMPORARY_REDIRECT,
                            [(header::LOCATION, "/landed")],
                        )
                            .into_response(),
                        "/loop" => {
                            (StatusCode::FOUND, [(header::LOCATION, "/loop")]).into_response()
                        }
                        _ => StatusCode::OK.into_response(),
                    }
                }
            }
        };
        let app = Router::new().fallback(record);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .resolve("mock.test", addr)
            .build()
            .unwrap();
        let base = format!("http://mock.test:{}", addr.port());
        let post = |path: &str| {
            http.post(format!("{base}{path}"))
                .header("x-forwarded-from", "hookforms/hooks/x")
                .json(&serde_json::json!({"r": 1}))
                .build()
                .unwrap()
        };

        let resp = send_following(&http, post("/found")).await.unwrap();
        assert_eq!(resp.status(), 200);
        {
            let s = seen.lock().unwrap();
            let (m, p, h, b) = &s[1];
            assert_eq!((m, p.as_str()), (&Method::GET, "/landed"));
            assert_eq!(h["content-type"], "application/json");
            assert_eq!(h["x-forwarded-from"], "hookforms/hooks/x");
            assert!(!h.contains_key("referer"));
            assert!(b.is_empty());
        }

        seen.lock().unwrap().clear();
        send_following(&http, post("/temp")).await.unwrap();
        {
            let s = seen.lock().unwrap();
            let (m, p, _, b) = &s[1];
            assert_eq!((m, p.as_str()), (&Method::POST, "/landed"));
            assert_eq!(&b[..], br#"{"r":1}"#);
        }

        seen.lock().unwrap().clear();
        let err = send_following(&http, post("/loop")).await.unwrap_err();
        assert_eq!(err, "too many redirects");
        assert_eq!(seen.lock().unwrap().len(), MAX_REDIRECTS + 1);
    }

    #[tokio::test]
    async fn upfront_messages_match_python() {
        assert_eq!(
            is_safe_url("ftp://x.example").await.unwrap_err(),
            "Scheme 'ftp' not allowed. Use http or https."
        );
        assert_eq!(
            is_safe_url("http://LocalHost/x").await.unwrap_err(),
            "Hostname 'localhost' is not allowed"
        );
        assert_eq!(
            is_safe_url("http://127.0.0.1/").await.unwrap_err(),
            "URL resolves to private/reserved IP address"
        );
        assert_eq!(
            is_safe_url("noscheme").await.unwrap_err(),
            "Scheme '' not allowed. Use http or https."
        );
    }
}
