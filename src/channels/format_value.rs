//! Smart value formatter (Python `channels/format_value.py`).
//!
//! Nested objects (GitHub, Stripe, GitLab payloads) show a display name instead
//! of raw JSON. Priority: full_name, name, login, title, label, email,
//! html_url, url, id.

use serde_json::Value;

use super::pyfmt::{py_dumps, py_str, take_chars};

pub const DISPLAY_KEYS: &[&str] = &[
    "full_name",
    "name",
    "login",
    "title",
    "label",
    "email",
    "html_url",
    "url",
    "id",
];

/// Python's default `max_len`.
pub const DEFAULT_MAX_LEN: usize = 300;

fn truncate(s: String, max_len: usize) -> String {
    if s.chars().count() > max_len {
        format!("{}...", take_chars(&s, max_len))
    } else {
        s
    }
}

/// Python `format_value(val, max_len)`.
pub fn format_value(val: &Value, max_len: usize) -> String {
    match val {
        Value::Null => String::new(),
        Value::Array(items) => {
            if items.is_empty() {
                return "(empty)".into();
            }
            if items.iter().all(|v| !v.is_object() && !v.is_array()) {
                return items.iter().map(py_str).collect::<Vec<_>>().join(", ");
            }
            let joined = items
                .iter()
                .map(|v| format_value(v, 80))
                .collect::<Vec<_>>()
                .join(", ");
            truncate(joined, max_len)
        }
        Value::Object(map) => {
            for key in DISPLAY_KEYS {
                if let Some(v) = map.get(*key)
                    && !v.is_null()
                    && !v.is_object()
                    && !v.is_array()
                {
                    return py_str(v);
                }
            }
            truncate(py_dumps(val), max_len)
        }
        other => py_str(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn primitives_and_lists() {
        assert_eq!(format_value(&json!(null), 300), "");
        assert_eq!(format_value(&json!("hi"), 300), "hi");
        assert_eq!(format_value(&json!(true), 300), "True");
        assert_eq!(format_value(&json!(3), 300), "3");
        assert_eq!(format_value(&json!([]), 300), "(empty)");
        assert_eq!(
            format_value(&json!(["a", 1, null, false]), 300),
            "a, 1, None, False"
        );
    }

    #[test]
    fn display_names() {
        assert_eq!(
            format_value(&json!({"id": 5, "login": "octo"}), 300),
            "octo"
        );
        assert_eq!(format_value(&json!({"name": null, "id": 7}), 300), "7");
        assert_eq!(
            format_value(&json!({"name": {"x": 1}, "email": "a@b"}), 300),
            "a@b"
        );
        // A nested list inside a list of objects is formatted as its joined items.
        assert_eq!(
            format_value(&json!([{"login": "a"}, {"login": "b"}, [1, 2]]), 300),
            "a, b, 1, 2"
        );
    }

    #[test]
    fn json_fallback_truncates() {
        assert_eq!(
            format_value(&json!({"x": 1, "y": "z"}), 300),
            r#"{"x": 1, "y": "z"}"#
        );
        assert_eq!(format_value(&json!({"x": 12345}), 5), r#"{"x":..."#);
        let long: Vec<Value> = (0..40).map(|i| json!({"k": i})).collect();
        let out = format_value(&Value::Array(long), 50);
        assert_eq!(out.chars().count(), 53);
        assert!(out.ends_with("..."));
    }
}
