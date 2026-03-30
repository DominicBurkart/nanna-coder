use serde_json::Value;
use std::fmt::Write;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat { Human, Json }

pub fn render(value: &Value, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(value).unwrap_or_default(),
        OutputFormat::Human => { let mut out = String::new(); render_value(&mut out, value, 0); out }
    }
}

fn render_value(out: &mut String, value: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match value {
        Value::Null => { let _ = writeln!(out, "{pad}(none)"); }
        Value::Bool(b) => { let _ = writeln!(out, "{pad}{b}"); }
        Value::Number(n) => { let _ = writeln!(out, "{pad}{n}"); }
        Value::String(s) => { let _ = writeln!(out, "{pad}{s}"); }
        Value::Array(arr) if arr.is_empty() => { let _ = writeln!(out, "{pad}(empty)"); }
        Value::Array(arr) => { for item in arr { let _ = write!(out, "{pad}- "); match item { Value::String(s) => { let _ = writeln!(out, "{s}"); } Value::Number(n) => { let _ = writeln!(out, "{n}"); } _ => { let _ = writeln!(out, "{}", serde_json::to_string(item).unwrap_or_default()); } } } }
        Value::Object(map) => { for (key, val) in map { match val { Value::Object(_) | Value::Array(_) => { let _ = writeln!(out, "{pad}{key}:"); render_value(out, val, indent + 2); } Value::Null => { let _ = writeln!(out, "{pad}{key}: (none)"); } Value::String(s) => { let _ = writeln!(out, "{pad}{key}: {s}"); } Value::Number(n) => { let _ = writeln!(out, "{pad}{key}: {n}"); } Value::Bool(b) => { let _ = writeln!(out, "{pad}{key}: {b}"); } } } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_json_roundtrip() { let v = serde_json::json!({"a":"b"}); let s = render(&v, OutputFormat::Json); let p: Value = serde_json::from_str(&s).unwrap(); assert_eq!(v, p); }
    #[test] fn test_human_object() { let v = serde_json::json!({"name":"test"}); assert!(render(&v, OutputFormat::Human).contains("name: test")); }
    #[test] fn test_empty_array() { assert!(render(&serde_json::json!([]), OutputFormat::Human).contains("(empty)")); }
    #[test] fn test_null() { assert!(render(&Value::Null, OutputFormat::Human).contains("(none)")); }
}
