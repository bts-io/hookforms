//! Small helpers that reproduce Python's text output for JSON values, so the
//! notifications read the same as the Python version: `str()`, `repr()`,
//! `json.dumps()` (default separators, `ensure_ascii=True`), `str.title()`,
//! truthiness and `html.escape()`.

use serde_json::{Number, Value};

/// Python truthiness of a JSON value (`None`, `False`, `0`, `""`, `[]`, `{}` are falsy).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `repr(float)`: shortest round-trip digits, exponent form outside 1e-4..1e16.
pub fn py_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').expect("exponent");
    let exp: i32 = exp.parse().expect("exponent digits");
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    if (-4..16).contains(&exp) {
        if exp >= 0 {
            let point = exp as usize + 1;
            let (int, frac) = if digits.len() > point {
                (digits[..point].to_string(), digits[point..].to_string())
            } else {
                (
                    format!("{digits}{}", "0".repeat(point - digits.len())),
                    "0".to_string(),
                )
            };
            format!("{sign}{int}.{frac}")
        } else {
            format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let rest = &digits[1..];
        let mant = if rest.is_empty() {
            digits[..1].to_string()
        } else {
            format!("{}.{rest}", &digits[..1])
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{mant}e{esign}{:02}", exp.abs())
    }
}

fn py_number(n: &Number) -> String {
    if n.is_i64() || n.is_u64() {
        n.to_string()
    } else {
        py_float(n.as_f64().unwrap_or(0.0))
    }
}

/// Python `str(value)` for a value parsed from JSON.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

/// Python `repr(value)` for a value parsed from JSON.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => py_number(n),
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => {
            let items: Vec<String> = a.iter().map(py_repr).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(o) => {
            let items: Vec<String> = o
                .iter()
                .map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr(v)))
                .collect();
            format!("{{{}}}", items.join(", "))
        }
    }
}

fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `json.dumps(value)` with default settings: `", "` and `": "`
/// separators and non-ASCII escaped as `\uXXXX`.
pub fn py_dumps(v: &Value) -> String {
    let mut out = String::new();
    dump_into(v, &mut out);
    out
}

fn dump_into(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                out.push_str(&n.to_string())
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_nan() {
                    out.push_str("NaN")
                } else if f.is_infinite() {
                    out.push_str(if f > 0.0 { "Infinity" } else { "-Infinity" })
                } else {
                    out.push_str(&py_float(f))
                }
            }
        }
        Value::String(s) => dump_str(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_into(item, out);
            }
            out.push(']');
        }
        Value::Object(o) => {
            out.push('{');
            for (i, (k, item)) in o.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                dump_str(k, out);
                out.push_str(": ");
                dump_into(item, out);
            }
            out.push('}');
        }
    }
}

fn dump_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Python `str.title()`: a cased letter after a non-cased character is upper
/// case, any other cased letter is lower case.
pub fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        let cased = c.is_lowercase() || c.is_uppercase();
        if cased {
            if prev_cased {
                out.extend(c.to_lowercase());
            } else {
                out.extend(c.to_uppercase());
            }
        } else {
            out.push(c);
        }
        prev_cased = cased;
    }
    out
}

/// `k.replace("_", " ").title()`, the field label every sender uses.
pub fn label(key: &str) -> String {
    title(&key.replace('_', " "))
}

/// Python `html.escape(s, quote=True)`.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            c => out.push(c),
        }
    }
    out
}

/// Python `s[:n]` on characters.
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python `s[-n:]` on characters.
pub fn last_chars(s: &str, n: usize) -> String {
    let len = s.chars().count();
    s.chars().skip(len.saturating_sub(n)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_match_python_repr() {
        assert_eq!(py_float(1.5), "1.5");
        assert_eq!(py_float(100.0), "100.0");
        assert_eq!(py_float(0.0001), "0.0001");
        assert_eq!(py_float(0.00001), "1e-05");
        assert_eq!(py_float(1.5e-7), "1.5e-07");
        assert_eq!(py_float(1e16), "1e+16");
        assert_eq!(py_float(1234567890123456.0), "1234567890123456.0");
        assert_eq!(py_float(-2.25), "-2.25");
        assert_eq!(py_float(0.1), "0.1");
        assert_eq!(py_float(1.0), "1.0");
        assert_eq!(py_float(1e15), "1000000000000000.0");
        let n = |s: &str| py_str(&serde_json::from_str(s).unwrap());
        assert_eq!(n("1"), "1");
        assert_eq!(n("-7"), "-7");
        assert_eq!(n("1.0"), "1.0");
        assert_eq!(n("1e-5"), "1e-05");
    }

    #[test]
    fn dumps_matches_python() {
        let v = json!({"b": [1, 2.5, true, null], "a": "caf\u{e9} \"q\"\n", "e": "\u{1F600}"});
        assert_eq!(
            py_dumps(&v),
            r#"{"b": [1, 2.5, true, null], "a": "caf\u00e9 \"q\"\n", "e": "\ud83d\ude00"}"#
        );
        assert_eq!(py_dumps(&json!({})), "{}");
        assert_eq!(py_dumps(&json!([])), "[]");
    }

    #[test]
    fn repr_matches_python() {
        assert_eq!(
            py_repr(&json!({"a": [1, "x", null, false]})),
            "{'a': [1, 'x', None, False]}"
        );
        assert_eq!(py_repr(&json!("it's")), "\"it's\"");
        assert_eq!(py_str(&json!("plain")), "plain");
        assert_eq!(py_str(&json!(true)), "True");
    }

    #[test]
    fn title_matches_python() {
        assert_eq!(label("first_name"), "First Name");
        assert_eq!(title("e-mail"), "E-Mail");
        assert_eq!(title("user's NAME"), "User'S Name");
        assert_eq!(title("field1name"), "Field1Name");
        assert_eq!(title("cf-turnstile-response"), "Cf-Turnstile-Response");
    }

    #[test]
    fn truthiness() {
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

    #[test]
    fn escape_and_slices() {
        assert_eq!(
            html_escape("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;&#x27;&lt;/a&gt;"
        );
        assert_eq!(take_chars("h\u{e9}llo", 2), "h\u{e9}");
        assert_eq!(last_chars("h\u{e9}llo", 3), "llo");
        assert_eq!(last_chars("ab", 4), "ab");
    }
}
