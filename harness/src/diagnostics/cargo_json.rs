use crate::tool_contract::{Diagnostic, Severity};
use serde_json::Value;

pub fn parse_cargo_json(stdout: &str) -> Vec<Diagnostic> {
    stdout.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<Diagnostic> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }
    let message = value.get("message")?;
    let severity = match message.get("level")?.as_str()? {
        "error" => Severity::Error,
        "warning" => Severity::Warning,
        _ => return None,
    };
    let spans = message.get("spans")?.as_array()?;
    let span = spans
        .iter()
        .find(|span| span.get("is_primary").and_then(Value::as_bool) == Some(true))
        .or_else(|| spans.first())?;
    Some(Diagnostic {
        file: span.get("file_name")?.as_str()?.to_string(),
        line: number(span.get("line_start")),
        col: number(span.get("column_start")),
        severity,
        code: message
            .get("code")
            .and_then(|code| code.get("code"))
            .and_then(Value::as_str)
            .map(str::to_string),
        message: message.get("message")?.as_str()?.to_string(),
    })
}

fn number(value: Option<&Value>) -> u32 {
    value
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0)
}
