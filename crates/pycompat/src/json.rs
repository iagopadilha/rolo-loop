// SPDX-License-Identifier: MIT
//! `json.dumps` with Python's default separators (`, ` and `: `), with or without `ensure_ascii`, and the `indent=N` form.

use serde_json::Value;

fn json_string(text: &str, ensure_ascii: bool, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if ensure_ascii && (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn number(n: &serde_json::Number, out: &mut String) {
    if n.is_f64() {
        let f = n.as_f64().unwrap_or(0.0);
        if f.fract() == 0.0 && f.abs() < 1e16 {
            out.push_str(&format!("{f:.1}"));
        } else {
            out.push_str(&format!("{f}"));
        }
    } else {
        out.push_str(&n.to_string());
    }
}

/// `json.dumps(value, ensure_ascii=...)`.
pub fn py_json_dumps(value: &Value, ensure_ascii: bool) -> String {
    let mut out = String::new();
    write_value(value, ensure_ascii, &mut out);
    out
}

fn write_value(value: &Value, ensure_ascii: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => number(n, out),
        Value::String(s) => json_string(s, ensure_ascii, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_value(item, ensure_ascii, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_string(k, ensure_ascii, out);
                out.push_str(": ");
                write_value(v, ensure_ascii, out);
            }
            out.push('}');
        }
    }
}

/// `json.dumps(value, ensure_ascii=..., indent=N)`.
pub fn py_json_dumps_indent(value: &Value, ensure_ascii: bool, indent: usize) -> String {
    let mut out = String::new();
    write_indented(value, ensure_ascii, indent, 0, &mut out);
    out
}

fn write_indented(value: &Value, ensure_ascii: bool, indent: usize, level: usize, out: &mut String) {
    let pad = |n: usize| " ".repeat(indent * n);
    match value {
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&pad(level + 1));
                write_indented(item, ensure_ascii, indent, level + 1, out);
                if i + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(level));
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            let len = map.len();
            for (i, (k, v)) in map.iter().enumerate() {
                out.push_str(&pad(level + 1));
                json_string(k, ensure_ascii, out);
                out.push_str(": ");
                write_indented(v, ensure_ascii, indent, level + 1, out);
                if i + 1 < len {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(level));
            out.push('}');
        }
        other => write_value(other, ensure_ascii, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_separators() {
        // built from pieces so no source line carries a literal backslash-u sequence
        let u = "\\u";
        assert_eq!(py_json_dumps(&json!("\u{e9}\n"), true), format!("\"{u}00e9\\n\""));
        assert_eq!(py_json_dumps(&json!("\u{1F600}"), true), format!("\"{u}d83d{u}de00\""));
        assert_eq!(py_json_dumps(&json!("\u{e9}"), false), "\"\u{e9}\"");
        assert_eq!(py_json_dumps(&json!({"a": 1, "b": [true, null, "x"], "c": {"d": 1.5}}), false), r#"{"a": 1, "b": [true, null, "x"], "c": {"d": 1.5}}"#);
        assert_eq!(py_json_dumps(&json!("\u{e9}"), false), "\"\u{e9}\"");
    }

    #[test]
    fn indented_form() {
        assert_eq!(py_json_dumps_indent(&json!({"a": [1, 2], "b": {}, "c": []}), false, 2), "{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {},\n  \"c\": []\n}");
    }
}
