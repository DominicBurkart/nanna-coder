use serde::Serialize;
use serde_json::Value;
use std::fmt::Write;
use std::process;

/// Exit codes for agent-consumable CLI output.
///
/// - 0: Success
/// - 1: User/input error
/// - 2: State error (task not found, wrong state)
/// - 3: Infrastructure error (Ollama unreachable, etc.)
/// - 130: Interrupted (Ctrl+C / SIGINT)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    UserError = 1,
    StateError = 2,
    InfraError = 3,
    Interrupted = 130,
}

impl ExitCode {
    pub fn process_exit(self) -> process::ExitCode {
        process::ExitCode::from(self as u8)
    }
}

/// Stable JSON envelope for structured CLI output.
///
/// All `--json` output is wrapped in this envelope so that callers can
/// rely on the `version` field for forward-compatible parsing.
#[derive(Debug, Clone, Serialize)]
pub struct JsonEnvelope {
    pub version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonErrorDetail>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonErrorDetail {
    pub code: String,
    pub message: String,
}

impl JsonEnvelope {
    pub fn success(data: Value) -> Self {
        Self {
            version: 1,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(code: &str, message: &str) -> Self {
        Self {
            version: 1,
            ok: false,
            data: None,
            error: Some(JsonErrorDetail {
                code: code.to_string(),
                message: message.to_string(),
            }),
        }
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Human,
    Json,
}

pub fn render(value: &Value, format: OutputFormat) -> String {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(value).unwrap_or_default(),
        OutputFormat::Human => {
            let mut out = String::new();
            render_value(&mut out, value, 0);
            out
        }
    }
}

fn render_value(out: &mut String, value: &Value, indent: usize) {
    let pad = " ".repeat(indent);
    match value {
        Value::Null => {
            let _ = writeln!(out, "{pad}(none)");
        }
        Value::Bool(b) => {
            let _ = writeln!(out, "{pad}{b}");
        }
        Value::Number(n) => {
            let _ = writeln!(out, "{pad}{n}");
        }
        Value::String(s) => {
            let _ = writeln!(out, "{pad}{s}");
        }
        Value::Array(arr) if arr.is_empty() => {
            let _ = writeln!(out, "{pad}(empty)");
        }
        Value::Array(arr) => {
            for item in arr {
                let _ = write!(out, "{pad}- ");
                match item {
                    Value::String(s) => {
                        let _ = writeln!(out, "{s}");
                    }
                    Value::Number(n) => {
                        let _ = writeln!(out, "{n}");
                    }
                    Value::Object(_) | Value::Array(_) => {
                        let _ = writeln!(out);
                        render_value(out, item, indent + 4);
                    }
                    _ => {
                        let _ =
                            writeln!(out, "{}", serde_json::to_string(item).unwrap_or_default());
                    }
                }
            }
        }
        Value::Object(map) => {
            for (key, val) in map {
                match val {
                    Value::Object(_) | Value::Array(_) => {
                        let _ = writeln!(out, "{pad}{key}:");
                        render_value(out, val, indent + 2);
                    }
                    Value::Null => {
                        let _ = writeln!(out, "{pad}{key}: (none)");
                    }
                    Value::String(s) => {
                        let _ = writeln!(out, "{pad}{key}: {s}");
                    }
                    Value::Number(n) => {
                        let _ = writeln!(out, "{pad}{key}: {n}");
                    }
                    Value::Bool(b) => {
                        let _ = writeln!(out, "{pad}{key}: {b}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_roundtrip() {
        let v = serde_json::json!({"a":"b"});
        let s = render(&v, OutputFormat::Json);
        let p: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v, p);
    }

    #[test]
    fn test_human_object() {
        let v = serde_json::json!({"name":"test"});
        assert!(render(&v, OutputFormat::Human).contains("name: test"));
    }

    #[test]
    fn test_empty_array() {
        assert!(render(&serde_json::json!([]), OutputFormat::Human).contains("(empty)"));
    }

    #[test]
    fn test_null() {
        assert!(render(&Value::Null, OutputFormat::Human).contains("(none)"));
    }

    #[test]
    fn test_exit_code_values() {
        assert_eq!(ExitCode::Success as u8, 0);
        assert_eq!(ExitCode::UserError as u8, 1);
        assert_eq!(ExitCode::StateError as u8, 2);
        assert_eq!(ExitCode::InfraError as u8, 3);
        assert_eq!(ExitCode::Interrupted as u8, 130);
    }

    #[test]
    fn test_json_envelope_success() {
        let env = JsonEnvelope::success(serde_json::json!({"task_id": "abc"}));
        assert!(env.ok);
        assert_eq!(env.version, 1);
        assert!(env.error.is_none());
        let s = env.to_json_string();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"]["task_id"], "abc");
    }

    #[test]
    fn test_json_envelope_error() {
        let env = JsonEnvelope::error("TASK_NOT_FOUND", "No task with that ID");
        assert!(!env.ok);
        assert_eq!(env.version, 1);
        assert!(env.data.is_none());
        let s = env.to_json_string();
        let parsed: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"]["code"], "TASK_NOT_FOUND");
        assert_eq!(parsed["error"]["message"], "No task with that ID");
    }

    #[test]
    fn test_human_nested_array_items() {
        let v = serde_json::json!([{"id": "a"}, {"id": "b"}]);
        let s = render(&v, OutputFormat::Human);
        assert!(s.contains("- "));
        assert!(s.contains("id: a"));
        assert!(s.contains("id: b"));
    }

    #[test]
    fn test_human_bool_value() {
        let v = serde_json::json!({"flag": true});
        assert!(render(&v, OutputFormat::Human).contains("flag: true"));
    }

    #[test]
    fn test_human_number_value() {
        let v = serde_json::json!({"count": 42});
        assert!(render(&v, OutputFormat::Human).contains("count: 42"));
    }

    #[test]
    fn test_human_null_in_object() {
        let v = serde_json::json!({"field": null});
        assert!(render(&v, OutputFormat::Human).contains("field: (none)"));
    }

    #[test]
    fn test_human_string_array() {
        let v = serde_json::json!(["hello", "world"]);
        let s = render(&v, OutputFormat::Human);
        assert!(s.contains("- hello"));
        assert!(s.contains("- world"));
    }

    #[test]
    fn test_human_number_array() {
        let v = serde_json::json!([1, 2, 3]);
        let s = render(&v, OutputFormat::Human);
        assert!(s.contains("- 1"));
        assert!(s.contains("- 2"));
    }

    #[test]
    fn test_human_bool_standalone() {
        let s = render(&serde_json::json!(true), OutputFormat::Human);
        assert!(s.contains("true"));
    }

    #[test]
    fn test_human_number_standalone() {
        let s = render(&serde_json::json!(99), OutputFormat::Human);
        assert!(s.contains("99"));
    }

    #[test]
    fn test_human_string_standalone() {
        let s = render(&serde_json::json!("hello"), OutputFormat::Human);
        assert!(s.contains("hello"));
    }
}
