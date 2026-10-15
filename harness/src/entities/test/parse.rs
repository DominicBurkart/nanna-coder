//! Pure-function parsers for
//! `cargo nextest run --message-format libtest-json` (tests) and
//! `cargo clippy --message-format=json` (lints) output.
//!
//! Input is expected to be newline-delimited JSON objects (`jsonl`). Unknown
//! message kinds are ignored so that mixed streams (e.g. `compiler-artifact`
//! messages interleaved with diagnostics) parse cleanly.
//!
//! Nextest's `libtest-json` format is a stable superset of libtest's unstable
//! `--format=json` shape; the same parser handles both.

use super::types::{
    LintLocation, LintResult, LintTool, Severity, TestError, TestResult, TestStatus,
};
use serde::Deserialize;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// cargo test --message-format=json
// ---------------------------------------------------------------------------

/// Shape of a single line emitted by
/// `cargo nextest run --message-format libtest-json` (and the equivalent
/// unstable libtest JSON output).
///
/// We only capture the fields we care about; unknown fields are silently
/// ignored by serde.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CargoTestMessage {
    Test(CargoTestEvent),
    /// Suite start/ok/failed messages carry aggregate fields we don't consume.
    #[serde(other)]
    Suite,
}

#[derive(Debug, Deserialize)]
struct CargoTestEvent {
    #[serde(default)]
    event: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    exec_time: Option<f64>,
    #[serde(default)]
    stdout: Option<String>,
}

/// Parse the stdout produced by
/// `cargo nextest run --message-format libtest-json` (the stable path this
/// crate uses) or `cargo test -- -Z unstable-options --format=json` (the
/// legacy nightly path) into a list of [`TestResult`].
///
/// Empty input yields an empty vector. Lines that are blank or fail to parse
/// as JSON are treated as errors (except blank lines, which are skipped).
pub fn parse_cargo_test_messages(stdout: &str) -> Result<Vec<TestResult>, TestError> {
    let mut out = Vec::new();
    for (lineno, line) in stdout.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: CargoTestMessage = serde_json::from_str(trimmed)
            .map_err(|e| TestError::Parse(format!("line {}: {}", lineno + 1, e)))?;

        let ev = match msg {
            CargoTestMessage::Test(ev) => ev,
            CargoTestMessage::Suite => continue,
        };

        let status = match ev.event.as_str() {
            "ok" => TestStatus::Passed,
            "failed" => TestStatus::Failed {
                reason: ev.stdout.unwrap_or_default(),
            },
            "ignored" => TestStatus::Skipped,
            "timeout" => TestStatus::Timeout,
            // "started" events carry no result — skip them.
            "started" => continue,
            other => {
                return Err(TestError::Parse(format!(
                    "line {}: unknown test event {:?}",
                    lineno + 1,
                    other
                )));
            }
        };

        out.push(TestResult {
            name: ev.name,
            status,
            duration_ms: ev.exec_time.map(|s| (s * 1000.0).round() as u64),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// cargo clippy --message-format=json
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
enum ClippyMessage {
    CompilerMessage(CompilerMessage),
    /// Non-diagnostic messages (compiler-artifact, build-script-executed, etc.)
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct CompilerMessage {
    message: Diagnostic,
}

#[derive(Debug, Deserialize)]
struct Diagnostic {
    #[serde(default)]
    message: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    code: Option<DiagnosticCode>,
    #[serde(default)]
    spans: Vec<DiagnosticSpan>,
}

#[derive(Debug, Deserialize)]
struct DiagnosticCode {
    code: String,
}

#[derive(Debug, Deserialize)]
struct DiagnosticSpan {
    file_name: String,
    line_start: u32,
    column_start: u32,
    #[serde(default)]
    is_primary: bool,
}

/// Parse the stdout produced by `cargo clippy --message-format=json` into a
/// list of [`LintResult`].
///
/// Non-diagnostic messages (for example `compiler-artifact`) are skipped. A
/// diagnostic without any span is also skipped (there's nothing to locate it
/// in a source file).
pub fn parse_clippy_messages(stdout: &str) -> Result<Vec<LintResult>, TestError> {
    let mut out = Vec::new();
    for (lineno, line) in stdout.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let msg: ClippyMessage = serde_json::from_str(trimmed)
            .map_err(|e| TestError::Parse(format!("line {}: {}", lineno + 1, e)))?;

        let diag = match msg {
            ClippyMessage::CompilerMessage(cm) => cm.message,
            ClippyMessage::Other => continue,
        };

        // rustc/clippy emit `level: "error" | "warning" | "note" | "help" |
        // "info"`. Internal compiler errors also surface with `level: "error"`
        // — the ICE detail lives in `message`, not the level — so we do not
        // match a literal `"error: internal compiler error"` here.
        let severity = match diag.level.as_str() {
            "error" => Severity::Error,
            "warning" => Severity::Warning,
            "note" | "help" | "info" => Severity::Info,
            // Unknown levels default to Info to avoid losing data.
            _ => Severity::Info,
        };

        let primary = diag
            .spans
            .iter()
            .find(|s| s.is_primary)
            .or_else(|| diag.spans.first());

        let span = match primary {
            Some(s) => s,
            None => continue,
        };

        let rule = diag
            .code
            .as_ref()
            .map(|c| c.code.clone())
            .unwrap_or_default();

        let tool = if rule.starts_with("clippy::") {
            LintTool::Clippy
        } else {
            LintTool::Custom("rustc".to_string())
        };

        out.push(LintResult {
            tool,
            location: LintLocation {
                file: PathBuf::from(&span.file_name),
                line: span.line_start,
                column: span.column_start,
            },
            rule,
            message: diag.message,
            severity,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --------- cargo test parser ---------

    #[test]
    fn test_parse_cargo_test_passed() {
        let input = r#"{"type":"test","event":"ok","name":"a::b","exec_time":0.01}"#;
        let results = parse_cargo_test_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "a::b");
        assert_eq!(results[0].status, TestStatus::Passed);
        assert_eq!(results[0].duration_ms, Some(10));
    }

    #[test]
    fn test_parse_cargo_test_failed() {
        let input = r#"{"type":"test","event":"failed","name":"a::b","exec_time":0.2,"stdout":"panicked at foo"}"#;
        let results = parse_cargo_test_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        match &results[0].status {
            TestStatus::Failed { reason } => assert!(reason.contains("panicked")),
            other => panic!("expected Failed, got {:?}", other),
        }
        assert_eq!(results[0].duration_ms, Some(200));
    }

    #[test]
    fn test_parse_cargo_test_ignored() {
        let input = r#"{"type":"test","event":"ignored","name":"a::skip"}"#;
        let results = parse_cargo_test_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, TestStatus::Skipped);
        assert!(results[0].duration_ms.is_none());
    }

    #[test]
    fn test_parse_cargo_test_mixed_events_and_suite() {
        let input = concat!(
            r#"{"type":"suite","event":"started","test_count":3}"#,
            "\n",
            r#"{"type":"test","event":"started","name":"a::b"}"#,
            "\n",
            r#"{"type":"test","event":"ok","name":"a::b","exec_time":0.01}"#,
            "\n",
            r#"{"type":"test","event":"ignored","name":"a::c"}"#,
            "\n",
            r#"{"type":"suite","event":"ok","passed":1,"failed":0}"#,
        );
        let results = parse_cargo_test_messages(input).expect("parse");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_parse_cargo_test_malformed_json() {
        let input = "not-json";
        let err = parse_cargo_test_messages(input).unwrap_err();
        assert!(matches!(err, TestError::Parse(_)));
    }

    #[test]
    fn test_parse_cargo_test_empty() {
        assert!(parse_cargo_test_messages("").unwrap().is_empty());
        assert!(parse_cargo_test_messages("\n\n").unwrap().is_empty());
    }

    // --------- clippy parser ---------

    #[test]
    fn test_parse_clippy_warning() {
        let input = r#"{"reason":"compiler-message","message":{"message":"unused import","level":"warning","code":{"code":"unused_imports"},"spans":[{"file_name":"src/lib.rs","line_start":3,"column_start":1,"is_primary":true}]}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].severity, Severity::Warning);
        assert_eq!(results[0].rule, "unused_imports");
        assert_eq!(results[0].location.line, 3);
    }

    #[test]
    fn test_parse_clippy_error() {
        let input = r#"{"reason":"compiler-message","message":{"message":"broken","level":"error","code":{"code":"E0001"},"spans":[{"file_name":"src/lib.rs","line_start":5,"column_start":2,"is_primary":true}]}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].severity, Severity::Error);
        // Non-clippy code -> LintTool::Custom("rustc")
        match &results[0].tool {
            LintTool::Custom(name) => assert_eq!(name, "rustc"),
            other => panic!("expected Custom(\"rustc\"), got {:?}", other),
        }
    }

    #[test]
    fn test_parse_clippy_with_location() {
        let input = r#"{"reason":"compiler-message","message":{"message":"x","level":"warning","code":{"code":"clippy::needless_return"},"spans":[{"file_name":"src/main.rs","line_start":10,"column_start":4,"is_primary":true}]}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool, LintTool::Clippy);
        assert_eq!(results[0].location.file, PathBuf::from("src/main.rs"));
        assert_eq!(results[0].location.line, 10);
        assert_eq!(results[0].location.column, 4);
    }

    #[test]
    fn test_parse_clippy_non_diagnostic_message() {
        let input =
            r#"{"reason":"compiler-artifact","package_id":"foo 0.1.0","target":{"name":"foo"}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert!(results.is_empty());
    }

    #[test]
    fn test_parse_clippy_malformed_json() {
        let err = parse_clippy_messages("not-json").unwrap_err();
        assert!(matches!(err, TestError::Parse(_)));
    }

    #[test]
    fn test_parse_clippy_diagnostic_without_span_is_skipped() {
        let input = r#"{"reason":"compiler-message","message":{"message":"no span","level":"warning","spans":[]}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert!(results.is_empty());
    }

    // --------- coverage gap closers (PR #267 follow-up) ---------

    /// `cargo test --format=json` may emit `event: "timeout"` when a single
    /// test hits the configured timeout. Exercise the timeout branch in the
    /// status match so the `TestStatus::Timeout` arm is covered.
    #[test]
    fn test_parse_cargo_test_timeout_event() {
        let input = r#"{"type":"test","event":"timeout","name":"slow::test","exec_time":30.0}"#;
        let results = parse_cargo_test_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, TestStatus::Timeout);
        assert_eq!(results[0].duration_ms, Some(30_000));
    }

    /// An unknown `event` string must surface as `TestError::Parse` with the
    /// line number embedded — exercises the catch-all arm in the status match.
    #[test]
    fn test_parse_cargo_test_unknown_event_errors() {
        let input = r#"{"type":"test","event":"banana","name":"x::y"}"#;
        let err = parse_cargo_test_messages(input).expect_err("unknown event must fail");
        match err {
            TestError::Parse(msg) => {
                assert!(msg.contains("unknown test event"), "got: {msg}");
                assert!(msg.contains("banana"), "got: {msg}");
                assert!(msg.contains("line 1"), "got: {msg}");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    /// `started` events carry no result and must be silently skipped.
    #[test]
    fn test_parse_cargo_test_started_event_skipped() {
        let input = r#"{"type":"test","event":"started","name":"x::y"}"#;
        let results = parse_cargo_test_messages(input).expect("parse");
        assert!(results.is_empty());
    }

    /// Exercise the `note`/`help`/`info` severity arm.
    #[test]
    fn test_parse_clippy_info_levels() {
        for level in ["note", "help", "info"] {
            let input = format!(
                r#"{{"reason":"compiler-message","message":{{"message":"m","level":"{level}","code":{{"code":"c"}},"spans":[{{"file_name":"f","line_start":1,"column_start":1,"is_primary":true}}]}}}}"#
            );
            let results = parse_clippy_messages(&input).expect("parse");
            assert_eq!(results.len(), 1, "level {level}");
            assert_eq!(results[0].severity, Severity::Info, "level {level}");
        }
    }

    /// Unknown level falls back to `Severity::Info` per the catch-all arm.
    #[test]
    fn test_parse_clippy_unknown_level_defaults_to_info() {
        let input = r#"{"reason":"compiler-message","message":{"message":"m","level":"verbose","code":{"code":"c"},"spans":[{"file_name":"f","line_start":1,"column_start":1,"is_primary":true}]}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].severity, Severity::Info);
    }

    /// When no span is primary, the parser falls back to the first span.
    /// Exercises the `.or_else(|| diag.spans.first())` branch in `primary`.
    #[test]
    fn test_parse_clippy_falls_back_to_first_span_when_none_primary() {
        let input = r#"{"reason":"compiler-message","message":{"message":"m","level":"warning","code":{"code":"clippy::x"},"spans":[{"file_name":"first.rs","line_start":9,"column_start":2,"is_primary":false},{"file_name":"second.rs","line_start":20,"column_start":3,"is_primary":false}]}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        // No primary -> first span wins.
        assert_eq!(results[0].location.file, PathBuf::from("first.rs"));
        assert_eq!(results[0].location.line, 9);
        assert_eq!(results[0].location.column, 2);
    }

    /// A clippy message with no `code` field still yields a result; `rule`
    /// is empty because of the `unwrap_or_default()`. Exercises the
    /// `None`-branch of `diag.code.as_ref().map(...)` and the non-clippy
    /// branch of the `tool` selector.
    #[test]
    fn test_parse_clippy_missing_code_yields_empty_rule() {
        let input = r#"{"reason":"compiler-message","message":{"message":"m","level":"warning","spans":[{"file_name":"f.rs","line_start":1,"column_start":1,"is_primary":true}]}}"#;
        let results = parse_clippy_messages(input).expect("parse");
        assert_eq!(results.len(), 1);
        assert!(results[0].rule.is_empty());
        match &results[0].tool {
            LintTool::Custom(name) => assert_eq!(name, "rustc"),
            other => panic!("expected Custom(\"rustc\"), got {other:?}"),
        }
    }

    /// Blank lines in clippy stdout are skipped (the `if trimmed.is_empty()`
    /// continue branch).
    #[test]
    fn test_parse_clippy_skips_blank_lines() {
        let input = "\n\n   \n";
        let results = parse_clippy_messages(input).expect("parse");
        assert!(results.is_empty());
    }
}
