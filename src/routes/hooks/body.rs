//! Request body parsing in the order `receive_webhook` uses: JSON first, then
//! form data (urlencoded or multipart, string fields only), then the raw text.

use serde_json::{Map, Value};

/// Starlette's multipart limits (`max_files`, `max_fields`, `max_part_size`).
const MAX_FILES: usize = 1000;
const MAX_FIELDS: usize = 1000;
const MAX_PART_SIZE: usize = 1024 * 1024;

/// Parse a request body exactly as the Python receiver does:
/// 1. `json.loads(body)`; any valid JSON wins, `null` gives `None`.
/// 2. If that fails and the raw `Content-Type` contains the urlencoded or
///    multipart type (case-sensitive substring, like Python), parse the form.
///    The parsed media type decides the parser; an unknown one yields `{}`,
///    a broken multipart body falls through to step 3.
/// 3. Non-empty bytes become `{"raw": "<utf-8 text, invalid bytes replaced>"}`.
pub async fn parse(content_type: &str, raw: &[u8]) -> Option<Value> {
    if let Ok(v) = parse_json(raw) {
        return if v.is_null() { None } else { Some(v) };
    }
    let mut body = None;
    if content_type.contains("application/x-www-form-urlencoded")
        || content_type.contains("multipart/form-data")
    {
        body = parse_form(content_type, raw).await.map(Value::Object);
    }
    if body.is_none() && !raw.is_empty() {
        let mut m = Map::new();
        m.insert(
            "raw".into(),
            Value::String(String::from_utf8_lossy(raw).into_owned()),
        );
        body = Some(Value::Object(m));
    }
    body
}

/// `json.loads` on bytes: a UTF-8 byte order mark is accepted.
fn parse_json(raw: &[u8]) -> Result<Value, ()> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    serde_json::from_slice(raw).map_err(|_| ())
}

/// Media type of a Content-Type header, lowercased, without parameters.
fn media_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

enum FormValue {
    Text(String),
    File,
}

/// Starlette `FormData` turned into `{k: v for k, v in form.items() if isinstance(v, str)}`:
/// the last value of a repeated key wins but keeps the first key's position, and
/// keys whose last value is a file are dropped.
fn to_object(items: Vec<(String, FormValue)>) -> Map<String, Value> {
    let mut merged: Vec<(String, FormValue)> = Vec::new();
    for (k, v) in items {
        match merged.iter_mut().find(|(key, _)| *key == k) {
            Some(slot) => slot.1 = v,
            None => merged.push((k, v)),
        }
    }
    merged
        .into_iter()
        .filter_map(|(k, v)| match v {
            FormValue::Text(s) => Some((k, Value::String(s))),
            FormValue::File => None,
        })
        .collect()
}

/// `None` when Starlette would raise (broken multipart), so the caller falls back to raw.
async fn parse_form(content_type: &str, raw: &[u8]) -> Option<Map<String, Value>> {
    match media_type(content_type).as_str() {
        "application/x-www-form-urlencoded" => Some(to_object(
            url::form_urlencoded::parse(raw)
                .map(|(k, v)| (k.into_owned(), FormValue::Text(v.into_owned())))
                .collect(),
        )),
        "multipart/form-data" => parse_multipart(content_type, raw).await.map(to_object),
        _ => Some(Map::new()),
    }
}

/// python-multipart's start state: leading CR/LF are skipped, then the body
/// must begin with `--boundary` followed by CRLF (or `-` for an empty
/// message). Any other byte is a parse error (Python falls back to raw);
/// running out of bytes is not.
fn start_boundary_ok(boundary: &str, raw: &[u8]) -> bool {
    let start = raw
        .iter()
        .position(|b| *b != b'\r' && *b != b'\n')
        .unwrap_or(raw.len());
    let rest = &raw[start..];
    let delim = format!("--{boundary}");
    let delim = delim.as_bytes();
    let n = rest.len().min(delim.len());
    if rest[..n] != delim[..n] {
        return false;
    }
    match rest.get(delim.len()) {
        None | Some(b'-') => true,
        Some(b'\r') => rest.get(delim.len() + 1).is_none_or(|b| *b == b'\n'),
        Some(_) => false,
    }
}

async fn parse_multipart(content_type: &str, raw: &[u8]) -> Option<Vec<(String, FormValue)>> {
    let boundary = multer::parse_boundary(content_type).ok()?;
    if !start_boundary_ok(&boundary, raw) {
        return None;
    }
    let mut multipart = multer::Multipart::with_reader(raw, boundary);
    let mut items = Vec::new();
    let (mut files, mut fields) = (0usize, 0usize);
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            // A body that stops early is not an error for python-multipart:
            // the parts completed so far are kept, an unfinished one dropped.
            Err(
                multer::Error::IncompleteStream
                | multer::Error::IncompleteHeaders
                | multer::Error::IncompleteFieldData { .. },
            ) => break,
            Err(_) => return None,
        };
        let name = field.name()?.to_string();
        if field.file_name().is_some() {
            files += 1;
            if files > MAX_FILES {
                return None;
            }
            items.push((name, FormValue::File));
        } else {
            fields += 1;
            if fields > MAX_FIELDS {
                return None;
            }
            let data = match field.bytes().await {
                Ok(d) => d,
                Err(
                    multer::Error::IncompleteFieldData { .. } | multer::Error::IncompleteStream,
                ) => {
                    break;
                }
                Err(_) => return None,
            };
            if data.len() > MAX_PART_SIZE {
                return None;
            }
            items.push((
                name,
                FormValue::Text(String::from_utf8_lossy(&data).into_owned()),
            ));
        }
    }
    Some(items)
}

/// Python truthiness of a JSON value (one copy, in `channels::pyfmt`).
pub use crate::channels::pyfmt::truthy;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn json_wins_over_content_type() {
        let b = parse("application/x-www-form-urlencoded", br#"{"a":1,"b":[2]}"#).await;
        assert_eq!(b, Some(json!({"a": 1, "b": [2]})));
        assert_eq!(parse("", b"null").await, None);
        assert_eq!(parse("", b"[1,2]").await, Some(json!([1, 2])));
        assert_eq!(parse("", b"\"x\"").await, Some(json!("x")));
        assert_eq!(parse("", b"\xef\xbb\xbf{}").await, Some(json!({})));
    }

    #[tokio::test]
    async fn urlencoded_last_value_first_position() {
        let b = parse(
            "application/x-www-form-urlencoded; charset=utf-8",
            b"name=Ann+Lee&email=a%40b.c&name=Bo&empty=&flag",
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::to_string(&b).unwrap(),
            r#"{"name":"Bo","email":"a@b.c","empty":"","flag":""}"#
        );
        assert_eq!(
            parse("application/x-www-form-urlencoded", b"").await,
            Some(json!({}))
        );
    }

    #[tokio::test]
    async fn substring_match_with_other_media_type_gives_empty_form() {
        let b = parse("text/plain; note=application/x-www-form-urlencoded", b"a=1").await;
        assert_eq!(b, Some(json!({})));
        // The substring check is case-sensitive, so this one is raw text.
        let b = parse("Application/X-WWW-Form-Urlencoded", b"a=1").await;
        assert_eq!(b, Some(json!({"raw": "a=1"})));
    }

    #[tokio::test]
    async fn multipart_keeps_text_fields_only() {
        let body = "--XX\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nAnn\r\n\
                    --XX\r\nContent-Disposition: form-data; name=\"cv\"; filename=\"a.txt\"\r\n\
                    Content-Type: text/plain\r\n\r\nfile body\r\n\
                    --XX\r\nContent-Disposition: form-data; name=\"msg\"\r\n\r\nhi there\r\n--XX--\r\n";
        let b = parse("multipart/form-data; boundary=XX", body.as_bytes()).await;
        assert_eq!(b, Some(json!({"name": "Ann", "msg": "hi there"})));
    }

    #[tokio::test]
    async fn truncated_multipart_keeps_finished_parts_like_python() {
        // Headers that never end: python-multipart reports no error, Starlette
        // gives an empty form, so Python stores `{}`.
        let b = parse("multipart/form-data; boundary=BOUND", b"--BOUND\r\nbroken").await;
        assert_eq!(b, Some(json!({})));
        let body = "--XX\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n\
                    --XX\r\nContent-Disposition: form-data; name=\"b\"\r\n\r\nunfinished";
        let b = parse("multipart/form-data; boundary=XX", body.as_bytes()).await;
        assert_eq!(b, Some(json!({"a": "1"})));
        // Only the start of the boundary: still no error.
        let b = parse("multipart/form-data; boundary=XX", b"\r\n--X").await;
        assert_eq!(b, Some(json!({})));
        // A wrong byte after the boundary is an error.
        let b = parse("multipart/form-data; boundary=XX", b"--XXy").await;
        assert_eq!(b, Some(json!({"raw": "--XXy"})));
    }

    #[tokio::test]
    async fn broken_multipart_falls_back_to_raw() {
        let b = parse("multipart/form-data", b"a=1").await;
        assert_eq!(b, Some(json!({"raw": "a=1"})));
        let b = parse("multipart/form-data; boundary=XX", b"garbage").await;
        assert_eq!(b, Some(json!({"raw": "garbage"})));
    }

    #[tokio::test]
    async fn raw_fallback_and_empty() {
        assert_eq!(parse("text/plain", b"").await, None);
        assert_eq!(
            parse("text/plain", b"hi \xff there").await,
            Some(json!({"raw": "hi \u{fffd} there"}))
        );
    }

    #[test]
    fn python_truthiness() {
        for v in [
            json!(null),
            json!(false),
            json!(0),
            json!(0.0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            assert!(!truthy(&v), "{v}");
        }
        for v in [
            json!(true),
            json!(1),
            json!("a"),
            json!([0]),
            json!({"a": null}),
        ] {
            assert!(truthy(&v), "{v}");
        }
    }
}
