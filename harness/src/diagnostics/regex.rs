use crate::tool_contract::{Diagnostic, Severity};

pub fn parse_regex(output: &str, pattern: &str) -> Vec<Diagnostic> {
    let Ok(re) = ::regex::Regex::new(pattern) else {
        return Vec::new();
    };
    let names: Vec<&str> = re.capture_names().flatten().collect();
    if !names.contains(&"file") || !names.contains(&"line") {
        return Vec::new();
    }
    re.captures_iter(output)
        .filter_map(|caps| {
            let text = |name: &str| caps.name(name).map(|m| m.as_str().to_string());
            let number = |name: &str| text(name).and_then(|t| t.trim().parse::<u32>().ok());
            let severity = match text("severity").as_deref().map(str::to_ascii_lowercase) {
                Some(s) if s.starts_with("warn") => Severity::Warning,
                Some(s) if s.starts_with("note") || s.starts_with("info") => Severity::Note,
                _ => Severity::Error,
            };
            Some(Diagnostic {
                file: text("file")?,
                line: number("line")?,
                col: number("col").unwrap_or(0),
                severity,
                code: text("code"),
                message: text("message").unwrap_or_default(),
            })
        })
        .collect()
}
