pub mod cargo_json;
pub mod regex;

pub use cargo_json::parse_cargo_json;
pub use regex::parse_regex;

use crate::plugins::types::DiagnosticsParser;
use crate::tool_contract::Diagnostic;
use std::collections::HashSet;

pub fn parse(parser: DiagnosticsParser, output: &str, pattern: Option<&str>) -> Vec<Diagnostic> {
    match parser {
        DiagnosticsParser::None => Vec::new(),
        DiagnosticsParser::CargoJson => parse_cargo_json(output),
        DiagnosticsParser::Regex => pattern
            .map(|pattern| parse_regex(output, pattern))
            .unwrap_or_default(),
    }
}

fn key(diagnostic: &Diagnostic) -> (&str, Option<&str>, &str) {
    (
        diagnostic.file.as_str(),
        diagnostic.code.as_deref(),
        diagnostic.message.as_str(),
    )
}

pub fn new_diagnostics(before: &[Diagnostic], after: &[Diagnostic]) -> Vec<Diagnostic> {
    let known: HashSet<_> = before.iter().map(key).collect();
    after
        .iter()
        .filter(|diagnostic| !known.contains(&key(diagnostic)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::types::DiagnosticsParser;
    use crate::tool_contract::{Diagnostic, Severity};

    const BROKEN: &str = include_str!("cargo_check_broken.jsonl");

    fn diag(file: &str, line: u32, code: Option<&str>, message: &str) -> Diagnostic {
        Diagnostic {
            file: file.to_string(),
            line,
            col: 1,
            severity: Severity::Error,
            code: code.map(str::to_string),
            message: message.to_string(),
        }
    }

    #[test]
    fn recorded_cargo_output_yields_every_error_and_warning_with_primary_spans() {
        let found = parse_cargo_json(BROKEN);
        let summary: Vec<(String, u32, u32, Severity, Option<String>, String)> = found
            .iter()
            .map(|d| {
                (
                    d.file.clone(),
                    d.line,
                    d.col,
                    d.severity,
                    d.code.clone(),
                    d.message.clone(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    "src/main.rs".to_string(),
                    8,
                    20,
                    Severity::Error,
                    Some("E0425".to_string()),
                    "cannot find value `missing` in this scope".to_string()
                ),
                (
                    "src/main.rs".to_string(),
                    3,
                    5,
                    Severity::Error,
                    Some("E0308".to_string()),
                    "mismatched types".to_string()
                ),
                (
                    "src/main.rs".to_string(),
                    7,
                    18,
                    Severity::Error,
                    Some("E0308".to_string()),
                    "mismatched types".to_string()
                ),
                (
                    "src/main.rs".to_string(),
                    2,
                    9,
                    Severity::Warning,
                    Some("unused_variables".to_string()),
                    "unused variable: `unused`".to_string()
                ),
            ]
        );
    }

    #[test]
    fn summary_notes_artifacts_and_junk_lines_are_ignored() {
        let noisy = format!(
            "not json at all\n{{\"reason\":\"build-finished\"}}\n\n{{broken\n{BROKEN}trailing junk"
        );
        assert_eq!(parse_cargo_json(&noisy), parse_cargo_json(BROKEN));
        assert!(parse_cargo_json("").is_empty());
    }

    #[test]
    fn a_message_without_any_span_is_not_a_located_diagnostic() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"aborting due to 2 previous errors","spans":[],"code":null}}"#;
        assert!(parse_cargo_json(line).is_empty());
    }

    #[test]
    fn the_first_span_is_used_when_none_is_marked_primary() {
        let line = r#"{"reason":"compiler-message","message":{"level":"error","message":"m","spans":[{"file_name":"a.rs","line_start":4,"column_start":2,"is_primary":false}],"code":null}}"#;
        let found = parse_cargo_json(line);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].file.as_str(), found[0].line, found[0].col),
            ("a.rs", 4, 2)
        );
        assert_eq!(found[0].code, None);
    }

    #[test]
    fn regex_parsing_reads_named_groups() {
        let pattern = r"(?m)^(?P<file>[^:\n]+):(?P<line>\d+):(?P<col>\d+): (?P<message>.+)$";
        let output = "pkg/a.go:12:5: undefined: foo\nok  pkg\npkg/b.go:3:1: syntax error\n";
        let found = parse_regex(output, pattern);
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].file.as_str(), found[0].line, found[0].col),
            ("pkg/a.go", 12, 5)
        );
        assert_eq!(found[0].message, "undefined: foo");
        assert_eq!(found[0].severity, Severity::Error);
        assert_eq!(found[1].message, "syntax error");
    }

    #[test]
    fn regex_optional_groups_default_and_code_is_captured() {
        let pattern = r"(?m)^(?P<file>\S+):(?P<line>\d+) (?P<code>[A-Z]\d+)$";
        let found = parse_regex("x.py:9 E501\n", pattern);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].col, 0);
        assert_eq!(found[0].code.as_deref(), Some("E501"));
        assert_eq!(found[0].message, "");
    }

    #[test]
    fn an_invalid_pattern_or_missing_required_groups_yield_nothing() {
        assert!(parse_regex("a:1", "(unclosed").is_empty());
        assert!(parse_regex("a:1", r"(?P<file>a):1").is_empty());
        assert!(parse_regex("a:1", r"(?P<line>\d+)").is_empty());
    }

    #[test]
    fn parse_dispatches_on_the_parser_kind() {
        assert!(parse(DiagnosticsParser::None, BROKEN, None).is_empty());
        assert_eq!(parse(DiagnosticsParser::CargoJson, BROKEN, None).len(), 4);
        let pattern = r"(?P<file>\w+):(?P<line>\d+)";
        assert_eq!(
            parse(DiagnosticsParser::Regex, "a:1", Some(pattern)).len(),
            1
        );
        assert!(parse(DiagnosticsParser::Regex, "a:1", None).is_empty());
    }

    #[test]
    fn new_diagnostics_is_a_set_difference_ignoring_line_shifts() {
        let before = vec![
            diag("a.rs", 10, Some("E1"), "old"),
            diag("a.rs", 20, Some("E2"), "gone"),
        ];
        let after = vec![
            diag("a.rs", 14, Some("E1"), "old"),
            diag("a.rs", 3, Some("E3"), "fresh"),
            diag("b.rs", 3, Some("E3"), "fresh"),
        ];
        let fresh = new_diagnostics(&before, &after);
        assert_eq!(
            fresh,
            vec![
                diag("a.rs", 3, Some("E3"), "fresh"),
                diag("b.rs", 3, Some("E3"), "fresh")
            ]
        );
    }

    #[test]
    fn new_diagnostics_handles_empty_sides() {
        let some = vec![diag("a.rs", 1, None, "m")];
        assert!(new_diagnostics(&some, &some).is_empty());
        assert!(new_diagnostics(&some, &[]).is_empty());
        assert_eq!(new_diagnostics(&[], &some), some);
    }
}
