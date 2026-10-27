// SPDX-License-Identifier: MIT
//! Typed readers for `config.json` values. Precedence in the app: command line > environment > config.json > default.

use serde_json::{Map, Value};

pub const DEFAULT_PORT_RANGE: (u16, u16) = (47800, 47899);
pub const DEFAULT_IDLE_HOURS: f64 = 2.0;
pub const DEFAULT_INVENTORY_GIANT_LINES: i64 = 1500;
pub const DEFAULT_INVENTORY_GIANT_KB: i64 = 200;
pub const DEFAULT_MAX_PARALLEL_MAPS: i64 = 6;
pub const DEFAULT_ORCHESTRATOR_MODEL: &str = "opus";

/// Python-style `repr` of a JSON value (used in user-facing notes that quote the offending value).
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e16 {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                }
            }
        }
        Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
            let mut out = String::new();
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
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        Value::Array(items) => {
            format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", "))
        }
        Value::Object(map) => {
            format!("{{{}}}", map.iter().map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v))).collect::<Vec<_>>().join(", "))
        }
    }
}

/// A positive whole number from config.json, else `default` and a note (absent key = default, no note).
pub fn read_positive_threshold(config: &Map<String, Value>, key: &str, default: i64, notes: &mut Vec<String>) -> i64 {
    let Some(value) = config.get(key) else {
        return default;
    };
    let number = match value {
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    let usable = number.is_some_and(|f| f.is_finite() && f > 0.0);
    if !usable || (number.unwrap_or(0.0) as i64) <= 0 {
        notes.push(format!("{key} in config.json is not a positive number ({}): using {default}", py_repr(value)));
        return default;
    }
    number.unwrap_or(0.0) as i64
}

/// A number from config.json (a number or numeric text such as `"8"`); anything else gives `default`.
pub fn read_float(value: Option<&Value>, default: f64) -> f64 {
    match value {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default),
        Some(Value::String(s)) => s.trim().parse::<f64>().unwrap_or(default),
        _ => default,
    }
}

/// `orchestrator_model`: a model alias or id; `""` = no `--model` flag; absent or not text = the default.
pub fn read_orchestrator_model(config: &Map<String, Value>) -> String {
    match config.get("orchestrator_model") {
        Some(Value::String(s)) => s.trim().to_string(),
        _ => DEFAULT_ORCHESTRATOR_MODEL.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn idle_hours_tolerates_numeric_text() {
        assert_eq!(read_float(Some(&json!("8")), 2.0), 8.0);
        assert_eq!(read_float(Some(&json!(1.5)), 2.0), 1.5);
        assert_eq!(read_float(Some(&json!("soon")), 2.0), 2.0);
        assert_eq!(read_float(Some(&json!(true)), 2.0), 2.0);
        assert_eq!(read_float(Some(&json!(null)), 2.0), 2.0);
        assert_eq!(read_float(None, 2.0), 2.0);
    }

    #[test]
    fn threshold_notes_quote_the_value_like_python() {
        let mut notes = vec![];
        assert_eq!(read_positive_threshold(&cfg(json!({"k": "abc"})), "k", 5, &mut notes), 5);
        assert_eq!(read_positive_threshold(&cfg(json!({"k": 0})), "k", 5, &mut notes), 5);
        assert_eq!(read_positive_threshold(&cfg(json!({"k": 0.5})), "k", 5, &mut notes), 5);
        assert_eq!(read_positive_threshold(&cfg(json!({"k": true})), "k", 5, &mut notes), 5);
        assert_eq!(read_positive_threshold(&cfg(json!({"k": 7})), "k", 5, &mut notes), 7);
        assert_eq!(read_positive_threshold(&cfg(json!({})), "k", 5, &mut notes), 5);
        assert_eq!(notes[0], "k in config.json is not a positive number ('abc'): using 5");
        assert_eq!(notes[1], "k in config.json is not a positive number (0): using 5");
        assert_eq!(notes[2], "k in config.json is not a positive number (0.5): using 5");
        assert_eq!(notes[3], "k in config.json is not a positive number (True): using 5");
        assert_eq!(notes.len(), 4);
    }

    #[test]
    fn orchestrator_model_rules() {
        assert_eq!(read_orchestrator_model(&cfg(json!({}))), "opus");
        assert_eq!(read_orchestrator_model(&cfg(json!({"orchestrator_model": " sonnet "}))), "sonnet");
        assert_eq!(read_orchestrator_model(&cfg(json!({"orchestrator_model": ""}))), "");
        assert_eq!(read_orchestrator_model(&cfg(json!({"orchestrator_model": 3}))), "opus");
    }
}
