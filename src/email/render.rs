//! The notification email HookForms sends for a submission, used by both
//! email paths: the legacy `notify_email` field and the `email` channel type.
//!
//! One entry point, [`render`], builds the subject, a designed HTML part
//! (table layout, one 600px column, inline styles, light only, system fonts)
//! and a plain-text twin from the same rows, so the two never drift apart.
//! Every value from the submission is HTML-escaped. Providers send both parts
//! as `multipart/alternative`.

use serde_json::{Map, Value};

use crate::channels::format_value::DEFAULT_MAX_LEN;
/// One copy of each formatting helper, shared with the channel code.
pub use crate::channels::format_value::format_value;
pub use crate::channels::pyfmt::{html_escape, label, py_str, truthy};

const FONT: &str = "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
const MONO: &str = "ui-monospace,SFMono-Regular,Menlo,Consolas,'Courier New',monospace";
const CANVAS: &str = "#f4f5f7";
const PAPER: &str = "#ffffff";
const INK: &str = "#1a1a2e";
const BODY: &str = "#333333";
const MUTED: &str = "#666666";
const RULE: &str = "#e5e7eb";
const TINT: &str = "#f6f7f9";
const GOLD: &str = "#d4a843";
const NO_FIELDS: &str = "The submission had no fields.";

/// Keys never shown as fields in the email.
const SKIP_KEYS: &[&str] = crate::channels::SKIP_KEYS;

/// A rendered notification: subject, HTML part and plain-text part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub subject: String,
    pub html: String,
    pub text: String,
}

/// The submitted text when the body holds nothing but `raw` (a body that was
/// not a form or a JSON object), else `None`. A form field named `raw` next to
/// other fields is not shown.
fn raw_only(body: &Map<String, Value>) -> Option<&str> {
    let mut keys = body.keys().filter(|k| *k != "cf-turnstile-response");
    match (keys.next(), keys.next()) {
        (Some(k), None) if k == "raw" => body.get("raw").and_then(Value::as_str),
        _ => None,
    }
}

/// `body[key]` as text when it is set and not empty.
fn field(body: &Map<String, Value>, key: &str) -> Option<String> {
    body.get(key).filter(|v| truthy(v)).map(py_str)
}

/// A reply address worth a `mailto:` link: one token with an `@` in it.
fn reply_address(body: &Map<String, Value>) -> Option<String> {
    field(body, "email").filter(|e| e.contains('@') && !e.chars().any(char::is_whitespace))
}

/// Build the notification email for a submission. `subject` is final (the
/// caller adds the inbox prefix).
pub fn render(slug: &str, subject: &str, body: &Map<String, Value>) -> Rendered {
    let raw = raw_only(body);
    let rows: Vec<(String, String)> = if raw.is_some() {
        Vec::new()
    } else {
        body.iter()
            .filter(|(k, v)| !SKIP_KEYS.contains(&k.as_str()) && truthy(v))
            .map(|(k, v)| (label(k), format_value(v, DEFAULT_MAX_LEN)))
            .collect()
    };

    let name = field(body, "name");
    let email = reply_address(body);
    let headline = match &name {
        Some(n) => format!("New submission from {n}"),
        None => "New submission".to_string(),
    };
    let preheader = match &name {
        Some(n) => format!("New submission from {n}."),
        None => "A new submission arrived.".to_string(),
    };
    let intro = if raw.is_some() {
        "A new submission was received:"
    } else {
        "A new form submission was received:"
    };
    let reason =
        format!("You get this email because the inbox /hooks/{slug} sends its submissions here.");
    let reply_to = name.clone().or_else(|| email.clone()).unwrap_or_default();

    let content = if let Some(raw) = raw {
        format!(
            "<pre style=\"margin:0 0 20px;padding:12px 14px;font-family:{MONO};font-size:13px;line-height:1.5;color:{INK};background-color:{TINT};border:1px solid {RULE};white-space:pre-wrap;word-break:break-word;\">{}</pre>",
            html_escape(raw)
        )
    } else if rows.is_empty() {
        format!(
            "<p style=\"margin:0 0 16px;font-family:{FONT};font-size:14px;line-height:1.6;color:{MUTED};\">{NO_FIELDS}</p>"
        )
    } else {
        let last = rows.len() - 1;
        let cells: String = rows
            .iter()
            .enumerate()
            .map(|(i, (label, value))| {
                let border = if i < last {
                    format!("border-bottom:1px solid {RULE};")
                } else {
                    String::new()
                };
                format!(
                    "<tr><td width=\"35%\" valign=\"top\" style=\"padding:10px 14px;font-family:{FONT};font-size:14px;line-height:1.5;color:{MUTED};{border}\">{}</td><td valign=\"top\" style=\"padding:10px 14px;font-family:{FONT};font-size:15px;line-height:1.5;font-weight:600;color:{INK};word-break:break-word;{border}\">{}</td></tr>",
                    html_escape(label),
                    html_escape(value).replace("\r\n", "<br>").replace('\n', "<br>"),
                )
            })
            .collect();
        format!(
            "<table role=\"presentation\" cellspacing=\"0\" cellpadding=\"0\" border=\"0\" width=\"100%\" style=\"width:100%;margin:0 0 20px;border:1px solid {RULE};\">{cells}</table>"
        )
    };

    let button = match &email {
        Some(addr) => format!(
            "<table role=\"presentation\" cellspacing=\"0\" cellpadding=\"0\" border=\"0\" style=\"margin:8px 0 24px;\"><tr><td bgcolor=\"{INK}\" style=\"border-radius:6px;background-color:{INK};\"><a href=\"mailto:{}\" style=\"display:inline-block;padding:14px 28px;font-family:{FONT};font-size:16px;font-weight:bold;line-height:1.2;color:#ffffff;background-color:{INK};border-radius:6px;text-decoration:none;\">Reply to {}</a></td></tr></table>",
            html_escape(addr),
            html_escape(&reply_to),
        ),
        None => String::new(),
    };

    let preheader_fill = "&#847;&zwnj;&nbsp;".repeat(30);
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en" xmlns="http://www.w3.org/1999/xhtml" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:o="urn:schemas-microsoft-com:office:office">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="X-UA-Compatible" content="IE=edge">
<meta name="x-apple-disable-message-reformatting">
<meta name="color-scheme" content="light">
<meta name="supported-color-schemes" content="light">
<title>{title}</title>
<!--[if mso]><noscript><xml><o:OfficeDocumentSettings><o:PixelsPerInch>96</o:PixelsPerInch></o:OfficeDocumentSettings></xml></noscript><![endif]-->
<style>
body{{margin:0;padding:0;background-color:{CANVAS};-webkit-text-size-adjust:100%;-ms-text-size-adjust:100%;}}
table{{border-collapse:collapse;mso-table-lspace:0pt;mso-table-rspace:0pt;}}
a[x-apple-data-detectors]{{color:inherit !important;text-decoration:none !important;}}
@media only screen and (max-width:620px){{
.container{{width:100% !important;}}
.pad{{padding-left:20px !important;padding-right:20px !important;}}
}}
</style>
</head>
<body style="margin:0;padding:0;background-color:{CANVAS};" bgcolor="{CANVAS}">
<div style="display:none;font-size:1px;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden;mso-hide:all;">{preheader}{preheader_fill}</div>
<table role="presentation" cellspacing="0" cellpadding="0" border="0" width="100%" style="width:100%;background-color:{CANVAS};" bgcolor="{CANVAS}">
<tr><td align="center" style="padding:24px 12px;">
<!--[if mso]><table role="presentation" width="600" align="center" cellspacing="0" cellpadding="0" border="0"><tr><td><![endif]-->
<table role="presentation" cellspacing="0" cellpadding="0" border="0" class="container" width="600" align="center" style="width:600px;max-width:600px;background-color:{PAPER};" bgcolor="{PAPER}">
<tr><td class="pad" bgcolor="{INK}" style="padding:20px 32px;background-color:{INK};font-family:{FONT};font-size:20px;line-height:1.2;font-weight:800;color:#ffffff;">HookForms</td></tr>
<tr><td style="height:4px;font-size:0;line-height:0;background-color:{GOLD};" bgcolor="{GOLD}">&nbsp;</td></tr>
<tr><td class="pad" style="padding:28px 32px 0;">
<h1 style="margin:0 0 16px;font-family:{FONT};font-size:26px;line-height:1.25;font-weight:800;color:{INK};">{headline_html}</h1>
</td></tr>
<tr><td class="pad" style="padding:0 32px 12px;">
<p style="margin:0 0 16px;font-family:{FONT};font-size:16px;line-height:1.6;color:{BODY};">{intro}</p>
{content}
{button}
</td></tr>
<tr><td class="pad" style="padding:20px 32px 28px;border-top:1px solid {RULE};">
<p style="margin:0;font-family:{FONT};font-size:13px;line-height:1.5;color:{MUTED};">{reason_html}</p>
</td></tr>
</table>
<!--[if mso]></td></tr></table><![endif]-->
</td></tr></table>
</body>
</html>"#,
        title = html_escape(subject),
        preheader = html_escape(&preheader),
        headline_html = html_escape(&headline),
        reason_html = html_escape(&reason),
    );

    let mut parts = vec![headline, intro.to_string()];
    if let Some(raw) = raw {
        parts.push(raw.to_string());
    } else if rows.is_empty() {
        parts.push(NO_FIELDS.to_string());
    } else {
        parts.push(
            rows.iter()
                .map(|(l, v)| format!("{l}: {v}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if let Some(addr) = &email {
        parts.push(format!("Reply to {reply_to}: mailto:{addr}"));
    }
    let text = format!("{}\n\n--\n{reason}\n", parts.join("\n\n"));

    Rendered {
        subject: subject.to_string(),
        html,
        text,
    }
}

/// The subject both paths use: the prefix, then `from {name}` or
/// `New Submission` when the submission has no name.
pub fn subject_line(prefix: &str, body: &Map<String, Value>) -> String {
    match field(body, "name") {
        Some(n) => format!("{prefix} from {n}"),
        None => format!("{prefix} New Submission"),
    }
}

/// The legacy `notify_email` email: the subject prefix defaults to `[slug]`.
pub fn render_legacy(
    slug: &str,
    subject_prefix: Option<&str>,
    body: &Map<String, Value>,
) -> Rendered {
    let prefix = match subject_prefix {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => format!("[{slug}]"),
    };
    render(slug, &subject_line(&prefix, body), body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn helpers() {
        assert_eq!(label("first_name"), "First Name");
        assert_eq!(
            html_escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;&#x27;&lt;/a&gt;"
        );
        assert_eq!(format_value(&json!(["a", 2, true]), 300), "a, 2, True");
        assert_eq!(format_value(&json!(1e-05), 300), "1e-05");
        assert!(!truthy(&json!("")));
    }

    #[test]
    fn subject_lines() {
        let r = render_legacy("contact", None, &obj(json!({"message": "hi"})));
        assert_eq!(r.subject, "[contact] New Submission");
        let r = render_legacy("c", Some("[Site]"), &obj(json!({"name": "Ann <A>"})));
        assert_eq!(r.subject, "[Site] from Ann <A>");
        assert!(r.html.contains("<title>[Site] from Ann &lt;A&gt;</title>"));
        let r = render_legacy("c", Some(""), &obj(json!({"name": null})));
        assert_eq!(r.subject, "[c] New Submission");
        assert!(r.html.contains(">New submission</h1>"));
    }

    #[test]
    fn html_and_text_from_the_same_rows() {
        let body = obj(json!({
            "name": "Ann <A>",
            "email": "ann@example.com",
            "first_name": "Ann",
            "message": "Line 1\nLine 2 & more",
            "empty": "",
            "source": "landing-page",
            "cf-turnstile-response": "t",
            "tags": ["a", "b"]
        }));
        let r = render("contact", "[Site] from Ann <A>", &body);

        // Masthead, gold rule, headline, escaped values, line breaks kept.
        assert!(r.html.contains(">HookForms</td></tr>"));
        assert!(r.html.contains("background-color:#d4a843;"));
        assert!(r.html.contains(">New submission from Ann &lt;A&gt;</h1>"));
        assert!(r.html.contains(">First Name</td>"));
        assert!(r.html.contains(">Line 1<br>Line 2 &amp; more</td>"));
        assert!(r.html.contains(">a, b</td>"));
        for hidden in [">Empty<", ">Source<", "Cf-Turnstile", "landing-page"] {
            assert!(!r.html.contains(hidden), "{hidden}");
        }
        assert!(r.html.contains(r#"<a href="mailto:ann@example.com""#));
        assert!(r.html.contains(">Reply to Ann &lt;A&gt;</a>"));
        assert!(!r.html.contains("Delivered by"));
        assert!(r.html.contains("the inbox /hooks/contact sends"));
        assert!(!r.html.contains("<script"));

        assert_eq!(
            r.text,
            "New submission from Ann <A>\n\n\
             A new form submission was received:\n\n\
             Name: Ann <A>\nEmail: ann@example.com\nFirst Name: Ann\n\
             Message: Line 1\nLine 2 & more\nTags: a, b\n\n\
             Reply to Ann <A>: mailto:ann@example.com\n\n\
             --\n\
             You get this email because the inbox /hooks/contact sends its submissions here.\n"
        );
        // Field order follows the submission.
        assert!(r.html.find(">Name</td>").unwrap() < r.html.find(">Tags</td>").unwrap());
    }

    #[test]
    fn escapes_hostile_values() {
        let body = obj(json!({
            "name": "<script>alert(1)</script>",
            "email": "\"><img src=x>@example.com",
            "x<y": "'quoted'"
        }));
        let r = render("s<l>ug", "S", &body);
        assert!(!r.html.contains("<script>"));
        assert!(!r.html.contains("<img"));
        assert!(r.html.contains("/hooks/s&lt;l&gt;ug"));
        assert!(r.html.contains(">X&lt;Y</td>"));
        assert!(r.html.contains("&#x27;quoted&#x27;"));
    }

    #[test]
    fn defaults_and_no_reply_button() {
        let r = render_legacy("contact", None, &obj(json!({"message": "hi"})));
        assert!(r.html.contains(">New submission</h1>"));
        assert!(r.html.contains("A new submission arrived."));
        assert!(!r.html.contains("mailto:"));
        assert!(!r.text.contains("mailto:"));
        assert!(!r.html.contains("Delivered by"));
        assert!(r.text.ends_with(
            "--\nYou get this email because the inbox /hooks/contact sends its submissions here.\n"
        ));
        // Not an address: no button.
        let r = render("s", "S", &obj(json!({"email": "not an address"})));
        assert!(!r.html.contains("mailto:"));
        // Address but no name: the button names the address.
        let r = render("s", "S", &obj(json!({"email": "e@example.com"})));
        assert!(r.html.contains(">Reply to e@example.com</a>"));
        assert!(
            r.text
                .contains("Reply to e@example.com: mailto:e@example.com")
        );
    }

    #[test]
    fn raw_body_and_empty_body() {
        let r = render("s", "S", &obj(json!({"raw": "plain <text>\nline 2"})));
        assert!(r.html.contains("A new submission was received:"));
        assert!(r.html.contains(">plain &lt;text&gt;\nline 2</pre>"));
        assert!(r.text.contains("\n\nplain <text>\nline 2\n\n--\n"));
        // `raw` next to other fields is not shown.
        let r = render("s", "S", &obj(json!({"raw": "r", "a": "1"})));
        assert!(!r.html.contains("<pre"));
        assert!(!r.html.contains(">Raw</td>"));
        let r = render("s", "S", &Map::new());
        assert!(r.html.contains("The submission had no fields."));
        assert!(r.text.contains("The submission had no fields."));
    }

    #[test]
    fn page_shape() {
        let r = render("s", "S", &obj(json!({"a": "1"})));
        assert!(r.html.starts_with("<!DOCTYPE html>\n<html lang=\"en\""));
        assert!(r.html.ends_with("</body>\n</html>"));
        assert!(r.html.contains(r#"width="600""#));
        assert!(r.html.contains(r#"role="presentation""#));
        assert!(!r.html.contains('\u{2014}') && !r.html.contains('\u{2013}'));
    }
}
