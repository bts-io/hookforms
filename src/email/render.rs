//! Notification email subject and HTML body for the legacy `notify_email`
//! path in `routers/webhooks.py`, rendered exactly like the Python version.
//! The email channel type renders its own page in `channels::email`.
//!
//! It sends a single `text/html` part, as Python does. The Python builtins the
//! template leans on (`str()`, truthiness, `html.escape`, `str.title`,
//! `json.dumps`) come from `channels::pyfmt`, so field labels and values come
//! out byte for byte the same on every path.

use serde_json::{Map, Value};

/// One copy of each Python formatting helper, shared with the channel code.
pub use crate::channels::format_value::format_value;
pub use crate::channels::pyfmt::{html_escape, py_str, title as py_title, truthy};

/// Keys never shown as fields in the email.
const SKIP_KEYS: &[&str] = &["raw", "source", "cf-turnstile-response"];

/// Field rows of the table, shared by both templates.
fn field_rows(body: &Map<String, Value>) -> String {
    let mut rows = String::new();
    for (key, val) in body {
        if SKIP_KEYS.contains(&key.as_str()) || !truthy(val) {
            continue;
        }
        let label = html_escape(&py_title(&key.replace('_', " ")));
        let escaped_val = html_escape(&format_value(val, 300));
        rows.push_str(&format!(
            "<tr>\
             <td style=\"padding:10px 14px;font-weight:600;color:#555;\
             white-space:nowrap;vertical-align:top;border-bottom:1px solid #eee;\">{label}</td>\
             <td style=\"padding:10px 14px;color:#222;border-bottom:1px solid #eee;\">{escaped_val}</td>\
             </tr>"
        ));
    }
    rows
}

/// `str(body.get(key, default))`.
fn get_str(body: &Map<String, Value>, key: &str, default: &str) -> String {
    body.get(key)
        .map(py_str)
        .unwrap_or_else(|| default.to_string())
}

fn page(heading: &str, rows: &str, reply_button: &str, footer_name: &str, slug: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body style="margin:0;padding:0;background:#f4f5f7;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;">
  <table width="100%" cellpadding="0" cellspacing="0" style="background:#f4f5f7;padding:32px 0;">
    <tr><td align="center">
      <table width="600" cellpadding="0" cellspacing="0" style="background:#ffffff;border-radius:8px;overflow:hidden;box-shadow:0 2px 8px rgba(0,0,0,0.08);">
        <tr>
          <td style="background:#1a1a2e;padding:24px 32px;">
            <h1 style="margin:0;color:#ffffff;font-size:20px;font-weight:600;">{heading}</h1>
          </td>
        </tr>
        <tr>
          <td style="padding:24px 32px;">
            <p style="margin:0 0 16px;color:#666;font-size:14px;">A new form submission was received:</p>
            <table width="100%" cellpadding="0" cellspacing="0" style="border:1px solid #eee;border-radius:6px;overflow:hidden;">
              {rows}
            </table>
          </td>
        </tr>
        {reply_button}
        <tr>
          <td style="padding:16px 32px;background:#fafafa;border-top:1px solid #eee;">
            <p style="margin:0;color:#999;font-size:12px;">Delivered by {footer_name} &middot; <code style="background:#eee;padding:2px 6px;border-radius:3px;font-size:11px;">/hooks/{slug}</code></p>
          </td>
        </tr>
      </table>
    </td></tr>
  </table>
</body>
</html>"#
    )
}

fn reply_button(email_escaped: &str, name_escaped: &str) -> String {
    format!(
        "<tr><td style=\"padding:0 32px 24px;\"><a href=\"mailto:{email_escaped}\" \
         style=\"display:inline-block;padding:10px 20px;background:#1a1a2e;color:#fff;\
         text-decoration:none;border-radius:5px;font-size:14px;\">Reply to {name_escaped}</a></td></tr>"
    )
}

/// A rendered notification: subject plus HTML body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub subject: String,
    pub html: String,
}

/// Legacy `notify_email` email (`routers/webhooks.py`): the heading carries
/// the subject prefix, the footer names `inbox.sender_name or 'HookForms'`.
pub fn render_legacy(
    slug: &str,
    subject_prefix: Option<&str>,
    inbox_sender_name: Option<&str>,
    body: &Map<String, Value>,
) -> Rendered {
    let plain_prefix = match subject_prefix {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => format!("[{slug}]"),
    };
    let prefix = html_escape(&plain_prefix);
    let sender_name = html_escape(&get_str(body, "name", "Unknown"));
    let sender_email_raw = get_str(body, "email", "");
    let sender_email = html_escape(&sender_email_raw);
    let subject_detail = if sender_name != "Unknown" {
        format!("from {sender_name}")
    } else {
        "New Submission".to_string()
    };
    let button = if sender_email_raw.is_empty() {
        String::new()
    } else {
        reply_button(&sender_email, &sender_name)
    };
    let footer = html_escape(match inbox_sender_name {
        Some(n) if !n.is_empty() => n,
        _ => "HookForms",
    });
    let html = page(
        &format!("{prefix} {subject_detail}"),
        &field_rows(body),
        &button,
        &footer,
        &html_escape(slug),
    );

    let plain_detail = match body.get("name") {
        Some(v) if truthy(v) => format!("from {}", py_str(v)),
        _ => "New Submission".to_string(),
    };
    Rendered {
        subject: format!("{plain_prefix} {plain_detail}"),
        html,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn python_helpers() {
        assert_eq!(py_title("first name"), "First Name");
        assert_eq!(py_title("e-mail ADDRESS"), "E-Mail Address");
        assert_eq!(py_title("abc123def"), "Abc123Def");
        assert_eq!(
            html_escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;&#x27;&lt;/a&gt;"
        );
        assert_eq!(py_str(&json!(null)), "None");
        assert_eq!(py_str(&json!(true)), "True");
        assert_eq!(py_str(&json!(12)), "12");
        assert_eq!(py_str(&json!(1.5)), "1.5");
        assert_eq!(py_str(&json!(["a", 1, null])), "['a', 1, None]");
        assert_eq!(py_str(&json!({"k": "it's"})), r#"{'k': "it's"}"#);
        assert_eq!(
            crate::channels::pyfmt::py_dumps(&json!({"b": [1, 2], "a": "é\n"})),
            r#"{"b": [1, 2], "a": "\u00e9\n"}"#
        );
        assert!(!truthy(&json!(0)));
        assert!(!truthy(&json!("")));
        assert!(!truthy(&json!({})));
        assert!(truthy(&json!("x")));
    }

    #[test]
    fn format_value_matches_python() {
        assert_eq!(format_value(&json!(null), 300), "");
        assert_eq!(format_value(&json!([]), 300), "(empty)");
        assert_eq!(format_value(&json!(["a", 2, true]), 300), "a, 2, True");
        assert_eq!(
            format_value(&json!({"login": "octo", "id": 1}), 300),
            "octo"
        );
        assert_eq!(
            format_value(&json!([{"name": "a"}, {"title": "b"}]), 300),
            "a, b"
        );
        assert_eq!(
            format_value(&json!({"x": 1, "y": [1]}), 300),
            r#"{"x": 1, "y": [1]}"#
        );
        assert_eq!(format_value(&json!({"x": "abcdef"}), 5), r#"{"x":..."#);
    }

    /// Floats print like Python's `str(float)` (values from Python 3).
    #[test]
    fn floats_match_python() {
        assert_eq!(format_value(&json!(1e-05), 300), "1e-05");
        assert_eq!(format_value(&json!([1.5e-07, "x"]), 300), "1.5e-07, x");
        assert_eq!(
            format_value(&json!({"a": 1e-05, "b": [1e16]}), 300),
            r#"{"a": 1e-05, "b": [1e+16]}"#
        );
        let r = render_legacy("c", None, None, &obj(json!({"name": 1.5e-07})));
        assert_eq!(r.subject, "[c] from 1.5e-07");
        assert!(r.html.contains(">[c] from 1.5e-07</h1>"));
    }

    #[test]
    fn legacy_render_matches_python() {
        let body = obj(json!({
            "name": "Ann <A>",
            "email": "ann@example.com",
            "first_name": "Ann",
            "empty": "",
            "source": "web",
            "tags": ["a", "b"]
        }));
        let r = render_legacy("contact", Some("[Site]"), Some("Acme & Co"), &body);
        assert_eq!(r.subject, "[Site] from Ann <A>");
        assert!(r.html.contains(">[Site] from Ann &lt;A&gt;</h1>"));
        assert!(r.html.contains(r#"<a href="mailto:ann@example.com" style="display:inline-block;padding:10px 20px;background:#1a1a2e;color:#fff;text-decoration:none;border-radius:5px;font-size:14px;">Reply to Ann &lt;A&gt;</a>"#));
        assert!(r.html.contains(r#"<td style="padding:10px 14px;font-weight:600;color:#555;white-space:nowrap;vertical-align:top;border-bottom:1px solid #eee;">First Name</td><td style="padding:10px 14px;color:#222;border-bottom:1px solid #eee;">Ann</td>"#));
        assert!(r.html.contains(">Tags</td>"));
        assert!(r.html.contains(">a, b</td>"));
        assert!(!r.html.contains(">Empty<"));
        assert!(!r.html.contains(">Source<"));
        assert!(r.html.contains("Delivered by Acme &amp; Co &middot;"));
        assert!(r.html.contains(">/hooks/contact</code>"));
        // Field order follows the submission.
        let name_at = r.html.find(">Name</td>").unwrap();
        let tags_at = r.html.find(">Tags</td>").unwrap();
        assert!(name_at < tags_at);
    }

    #[test]
    fn legacy_render_defaults() {
        let r = render_legacy("contact", None, None, &obj(json!({"message": "hi"})));
        assert_eq!(r.subject, "[contact] New Submission");
        assert!(r.html.contains(">[contact] New Submission</h1>"));
        assert!(!r.html.contains("mailto:"));
        assert!(r.html.contains("Delivered by HookForms &middot;"));
        // Python quirk: a null name reads "None" in the HTML, "New Submission" in the subject.
        let r = render_legacy("c", Some(""), None, &obj(json!({"name": null})));
        assert_eq!(r.subject, "[c] New Submission");
        assert!(r.html.contains(">[c] from None</h1>"));
    }

    #[test]
    fn full_page_shape() {
        let r = render_legacy("s", None, None, &obj(json!({"a": "1"})));
        assert!(
            r.html
                .starts_with("<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"></head>\n")
        );
        assert!(r.html.ends_with("  </table>\n</body>\n</html>"));
        assert!(
            r.html
                .contains("\n        \n        <tr>\n          <td style=\"padding:16px 32px;")
        );
    }
}
