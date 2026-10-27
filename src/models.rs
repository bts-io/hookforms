//! Row structs shared by the route modules, the timestamp format the API
//! returns, and small helpers that reproduce FastAPI/pydantic request
//! validation (body parsing, field checks, `limit`/`offset` query params)
//! with the same 422 `details` entries the Python version sends.
//!
//! `Inbox` comes from step 2, `Event` from step 3.

use axum::http::HeaderMap;
use axum::http::header::CONTENT_TYPE;
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row, SqlitePool};

use crate::error::AppError;

/// A row of `webhook_inboxes`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Inbox {
    pub id: String,
    pub slug: String,
    pub description: Option<String>,
    pub forward_url: Option<String>,
    pub notify_email: Option<String>,
    pub email_subject_prefix: Option<String>,
    pub sender_name: Option<String>,
    pub turnstile_secret: Option<String>,
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Inbox {
    pub const COLUMNS: &'static str = "id, slug, description, forward_url, notify_email, \
         email_subject_prefix, sender_name, turnstile_secret, is_active, created_at, updated_at";

    pub async fn by_slug(db: &SqlitePool, slug: &str) -> Result<Option<Inbox>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM webhook_inboxes WHERE slug = ?",
            Self::COLUMNS
        ))
        .bind(slug)
        .fetch_optional(db)
        .await
    }

    pub async fn by_id(db: &SqlitePool, id: &str) -> Result<Option<Inbox>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {} FROM webhook_inboxes WHERE id = ?",
            Self::COLUMNS
        ))
        .bind(id)
        .fetch_optional(db)
        .await
    }

    /// Python `WebhookInboxResponse.from_inbox`: the secret is never returned,
    /// only `has_turnstile`.
    pub fn response(&self) -> InboxResponse {
        InboxResponse {
            id: self.id.clone(),
            slug: self.slug.clone(),
            description: self.description.clone(),
            forward_url: self.forward_url.clone(),
            notify_email: self.notify_email.clone(),
            email_subject_prefix: self.email_subject_prefix.clone(),
            sender_name: self.sender_name.clone(),
            has_turnstile: self
                .turnstile_secret
                .as_deref()
                .is_some_and(|s| !s.is_empty()),
            is_active: self.is_active,
            created_at: api_timestamp(&self.created_at),
        }
    }
}

/// Python `WebhookInboxResponse`.
#[derive(Debug, Clone, Serialize)]
pub struct InboxResponse {
    pub id: String,
    pub slug: String,
    pub description: Option<String>,
    pub forward_url: Option<String>,
    pub notify_email: Option<String>,
    pub email_subject_prefix: Option<String>,
    pub sender_name: Option<String>,
    pub has_turnstile: bool,
    pub is_active: bool,
    pub created_at: String,
}

/// A row of `webhook_events`. JSON columns are decoded; a missing or invalid
/// `headers` / `query_params` reads as `{}`, a NULL or JSON `null` body as `None`.
#[derive(Debug, Clone)]
pub struct Event {
    pub id: String,
    pub inbox_id: String,
    pub method: String,
    pub headers: Map<String, Value>,
    pub body: Option<Value>,
    pub query_params: Map<String, Value>,
    pub source_ip: Option<String>,
    pub received_at: String,
}

fn json_column(text: Option<String>) -> Map<String, Value> {
    match text.and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

impl<'r> FromRow<'r, SqliteRow> for Event {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        let body: Option<String> = row.try_get("body")?;
        let body = match body {
            None => None,
            Some(t) => match serde_json::from_str::<Value>(&t) {
                Ok(Value::Null) => None,
                Ok(v) => Some(v),
                Err(e) => return Err(sqlx::Error::Decode(Box::new(e))),
            },
        };
        Ok(Event {
            id: row.try_get("id")?,
            inbox_id: row.try_get("inbox_id")?,
            method: row.try_get("method")?,
            headers: json_column(row.try_get("headers")?),
            body,
            query_params: json_column(row.try_get("query_params")?),
            source_ip: row.try_get("source_ip")?,
            received_at: row.try_get("received_at")?,
        })
    }
}

/// Python `WebhookEventResponse`.
#[derive(Debug, Serialize)]
pub struct EventResponse<'a> {
    pub id: &'a str,
    pub method: &'a str,
    pub headers: &'a Map<String, Value>,
    pub body: &'a Option<Value>,
    pub query_params: &'a Map<String, Value>,
    pub source_ip: &'a Option<String>,
    pub received_at: String,
}

impl Event {
    pub fn response(&self) -> EventResponse<'_> {
        EventResponse {
            id: &self.id,
            method: &self.method,
            headers: &self.headers,
            body: &self.body,
            query_params: &self.query_params,
            source_ip: &self.source_ip,
            received_at: api_timestamp(&self.received_at),
        }
    }
}

/// A stored timestamp (`2026-10-27T15:28:02.123456Z`) as the Python API
/// returns it: pydantic 2.10 prints an aware UTC datetime with `Z`, drops the
/// fraction when it is zero (`2026-10-27T15:28:02Z`) and otherwise keeps all
/// six digits (`2026-10-27T15:28:02.120000Z`). The one formatter for every
/// timestamp in a response.
pub fn api_timestamp(stored: &str) -> String {
    match stored.strip_suffix(".000000Z") {
        Some(base) => format!("{base}Z"),
        None => stored.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Request validation in pydantic's shape
// ---------------------------------------------------------------------------

/// One entry of the 422 `details` list (`ctx` is dropped, as Python's handler does).
pub fn verr(kind: &str, loc: Value, msg: impl Into<String>, input: Value) -> Value {
    json!({ "type": kind, "loc": loc, "msg": msg.into(), "input": input })
}

/// FastAPI's JSON body handling for a model parameter: the parsed object, or
/// the 422 FastAPI sends for an empty body, invalid JSON or a non-object.
pub fn json_object(headers: &HeaderMap, body: &[u8]) -> Result<Map<String, Value>, AppError> {
    if body.is_empty() {
        return Err(AppError::Validation(vec![verr(
            "missing",
            json!(["body"]),
            "Field required",
            Value::Null,
        )]));
    }
    let is_json = match headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(ct) => {
            let mime = ct
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            mime == "application/json"
                || (mime.starts_with("application/") && mime.ends_with("+json"))
        }
    };
    let value: Value = if is_json {
        match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(e) => {
                let pos = if e.is_eof() {
                    body.len()
                } else {
                    byte_pos(body, e.line(), e.column())
                };
                return Err(AppError::Validation(vec![verr(
                    "json_invalid",
                    json!(["body", pos]),
                    "JSON decode error",
                    json!({}),
                )]));
            }
        }
    } else {
        Value::String(String::from_utf8_lossy(body).into_owned())
    };
    match value {
        Value::Object(map) => Ok(map),
        other => Err(AppError::Validation(vec![verr(
            "model_attributes_type",
            json!(["body"]),
            "Input should be a valid dictionary or object to extract fields from",
            other,
        )])),
    }
}

/// serde_json reports line and column (1-based); Python reports a 0-based offset.
fn byte_pos(body: &[u8], line: usize, column: usize) -> usize {
    let mut offset = 0;
    for (i, l) in body.split(|b| *b == b'\n').enumerate() {
        if i + 1 == line {
            return (offset + column.saturating_sub(1)).min(body.len());
        }
        offset += l.len() + 1;
    }
    body.len()
}

/// Collects field errors so every bad field is reported at once, like pydantic.
#[derive(Debug, Default)]
pub struct Fields {
    pub errors: Vec<Value>,
}

impl Fields {
    fn loc(field: &str) -> Value {
        json!(["body", field])
    }

    /// Required `str` field.
    pub fn req_str(&mut self, obj: &Map<String, Value>, field: &str) -> Option<String> {
        match obj.get(field) {
            None => {
                self.errors.push(verr(
                    "missing",
                    Self::loc(field),
                    "Field required",
                    Value::Object(obj.clone()),
                ));
                None
            }
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => {
                self.errors.push(verr(
                    "string_type",
                    Self::loc(field),
                    "Input should be a valid string",
                    other.clone(),
                ));
                None
            }
        }
    }

    /// `Optional[str]`: `None` when absent, `Some(None)` for null.
    pub fn opt_str(&mut self, obj: &Map<String, Value>, field: &str) -> Option<Option<String>> {
        match obj.get(field)? {
            Value::Null => Some(None),
            Value::String(s) => Some(Some(s.clone())),
            other => {
                self.errors.push(verr(
                    "string_type",
                    Self::loc(field),
                    "Input should be a valid string",
                    other.clone(),
                ));
                None
            }
        }
    }

    /// `Optional[bool]` in pydantic's lax mode: `None` when absent, `Some(None)` for null.
    pub fn opt_bool(&mut self, obj: &Map<String, Value>, field: &str) -> Option<Option<bool>> {
        let v = obj.get(field)?;
        let parsed = match v {
            Value::Null => return Some(None),
            Value::Bool(b) => Ok(*b),
            Value::Number(n) => match n.as_f64() {
                Some(0.0) => Ok(false),
                Some(1.0) => Ok(true),
                _ => Err((
                    "bool_parsing",
                    "Input should be a valid boolean, unable to interpret input",
                )),
            },
            Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
                "0" | "off" | "f" | "false" | "n" | "no" => Ok(false),
                "1" | "on" | "t" | "true" | "y" | "yes" => Ok(true),
                _ => Err((
                    "bool_parsing",
                    "Input should be a valid boolean, unable to interpret input",
                )),
            },
            _ => Err(("bool_type", "Input should be a valid boolean")),
        };
        match parsed {
            Ok(b) => Some(Some(b)),
            Err((kind, msg)) => {
                self.errors
                    .push(verr(kind, Self::loc(field), msg, v.clone()));
                None
            }
        }
    }

    pub fn finish(self) -> Result<(), AppError> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(AppError::Validation(self.errors))
        }
    }
}

/// `limit: int = Query(50, le=200)` and `offset: int = Query(0)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

impl Page {
    /// Parse from a raw query string. The last value of a repeated key wins.
    /// Negative values are refused: Python passed them to Postgres, which failed
    /// with a 500, and SQLite would treat a negative limit as "no limit".
    pub fn from_query(query: Option<&str>) -> Result<Page, AppError> {
        let mut limit_raw = None;
        let mut offset_raw = None;
        for (k, v) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
            match k.as_ref() {
                "limit" => limit_raw = Some(v.into_owned()),
                "offset" => offset_raw = Some(v.into_owned()),
                _ => {}
            }
        }
        let mut errors = Vec::new();
        let limit = parse_int(&mut errors, "limit", limit_raw, 50, Some(200));
        let offset = parse_int(&mut errors, "offset", offset_raw, 0, None);
        if errors.is_empty() {
            Ok(Page { limit, offset })
        } else {
            Err(AppError::Validation(errors))
        }
    }
}

fn parse_int(
    errors: &mut Vec<Value>,
    name: &str,
    raw: Option<String>,
    default: i64,
    le: Option<i64>,
) -> i64 {
    let Some(raw) = raw else { return default };
    let loc = json!(["query", name]);
    let Ok(n) = raw.trim().parse::<i64>() else {
        errors.push(verr(
            "int_parsing",
            loc,
            "Input should be a valid integer, unable to parse string as an integer",
            Value::String(raw),
        ));
        return default;
    };
    if let Some(max) = le
        && n > max
    {
        errors.push(verr(
            "less_than_equal",
            loc,
            format!("Input should be less than or equal to {max}"),
            Value::String(raw),
        ));
        return default;
    }
    if n < 0 {
        errors.push(verr(
            "greater_than_equal",
            loc,
            "Input should be greater than or equal to 0",
            Value::String(raw),
        ));
        return default;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values from pydantic 2.10.4 (the pinned version) through
    /// FastAPI 0.115.6's `jsonable_encoder` for an aware UTC datetime.
    #[test]
    fn timestamps_like_pydantic() {
        let cases = [
            ("2026-10-27T15:28:02.123456Z", "2026-10-27T15:28:02.123456Z"),
            ("2026-10-27T15:28:02.120000Z", "2026-10-27T15:28:02.120000Z"),
            ("2026-10-27T15:28:02.000100Z", "2026-10-27T15:28:02.000100Z"),
            ("2026-10-27T15:28:02.000000Z", "2026-10-27T15:28:02Z"),
        ];
        for (stored, want) in cases {
            assert_eq!(api_timestamp(stored), want);
            let inbox = Inbox {
                id: "i".into(),
                slug: "s".into(),
                description: None,
                forward_url: None,
                notify_email: None,
                email_subject_prefix: None,
                sender_name: None,
                turnstile_secret: None,
                is_active: true,
                created_at: stored.into(),
                updated_at: stored.into(),
            };
            assert_eq!(inbox.response().created_at, want);
        }
    }

    #[test]
    fn page_defaults_and_errors() {
        assert_eq!(
            Page::from_query(None).unwrap(),
            Page {
                limit: 50,
                offset: 0
            }
        );
        assert_eq!(
            Page::from_query(Some("limit=5&offset=10")).unwrap(),
            Page {
                limit: 5,
                offset: 10
            }
        );
        let AppError::Validation(d) = Page::from_query(Some("limit=201&offset=x")).unwrap_err()
        else {
            panic!()
        };
        assert_eq!(
            Value::Array(d),
            json!([
                {"type": "less_than_equal", "loc": ["query", "limit"],
                 "msg": "Input should be less than or equal to 200", "input": "201"},
                {"type": "int_parsing", "loc": ["query", "offset"],
                 "msg": "Input should be a valid integer, unable to parse string as an integer",
                 "input": "x"}
            ])
        );
    }

    #[test]
    fn body_errors() {
        let h = HeaderMap::new();
        let AppError::Validation(d) = json_object(&h, b"").unwrap_err() else {
            panic!()
        };
        assert_eq!(d[0]["loc"], json!(["body"]));
        let AppError::Validation(d) = json_object(&h, b"[1]").unwrap_err() else {
            panic!()
        };
        assert_eq!(d[0]["type"], "model_attributes_type");
        let AppError::Validation(d) = json_object(&h, b"{\"a\":").unwrap_err() else {
            panic!()
        };
        assert_eq!(d[0]["type"], "json_invalid");
        assert_eq!(d[0]["loc"], json!(["body", 5]));
    }

    #[test]
    fn lax_bools() {
        let obj = json!({"a": "yes", "b": 0, "c": "maybe", "d": []});
        let obj = obj.as_object().unwrap();
        let mut f = Fields::default();
        assert_eq!(f.opt_bool(obj, "a"), Some(Some(true)));
        assert_eq!(f.opt_bool(obj, "b"), Some(Some(false)));
        assert_eq!(f.opt_bool(obj, "c"), None);
        assert_eq!(f.opt_bool(obj, "d"), None);
        assert_eq!(f.opt_bool(obj, "z"), None);
        assert_eq!(f.errors[0]["type"], "bool_parsing");
        assert_eq!(f.errors[1]["type"], "bool_type");
    }
}
