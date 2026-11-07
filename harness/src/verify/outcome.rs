use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::config::VerifyConfig;
use super::runner::{CheckRunner, RawRun};
use crate::entities::context::types::ToolCallRecord;

pub const MAX_OUTPUT_BYTES: usize = 8 * 1024;

pub const CHECK_TOOL_NAME: &str = "run_check";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    TimedOut,
    Skipped,
}

/// How one declared check ended: its name and command, status, exit code
/// (`None` when it timed out, was signalled or never ran), duration and the
/// tail of its combined output.
///
/// ```
/// use harness::verify::{CheckOutcome, CheckStatus};
///
/// let outcome = CheckOutcome {
///     name: "clippy".into(),
///     command: "cargo clippy".into(),
///     status: CheckStatus::Failed,
///     exit_code: Some(101),
///     timeout_secs: 900,
///     duration_ms: 42,
///     output: "error: ...".into(),
///     output_truncated: false,
/// };
/// assert_eq!(outcome.failure_message(), "check `clippy` failed with exit code 101");
/// let json = serde_json::to_value(&outcome).unwrap();
/// assert_eq!(json["status"], "failed");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutcome {
    pub name: String,
    pub command: String,
    pub status: CheckStatus,
    pub exit_code: Option<i32>,
    pub timeout_secs: u64,
    pub duration_ms: u64,
    pub output: String,
    pub output_truncated: bool,
}

#[derive(Debug, Error)]
#[error("could not run check `{check}`: {message}")]
pub struct ExecError {
    pub check: String,
    pub message: String,
    pub completed: Vec<CheckOutcome>,
}

pub fn truncate_tail(text: &str, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text.to_string(), false);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_string(), true)
}

fn combine(raw: &RawRun) -> String {
    if raw.stderr.is_empty() {
        raw.stdout.clone()
    } else {
        format!("{}\n[stderr]\n{}", raw.stdout, raw.stderr)
    }
}

impl CheckOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self.status, CheckStatus::Failed | CheckStatus::TimedOut)
    }

    pub fn failure_message(&self) -> String {
        match (self.status, self.exit_code) {
            (CheckStatus::Failed, Some(code)) => {
                format!("check `{}` failed with exit code {code}", self.name)
            }
            (CheckStatus::Failed, None) => {
                format!("check `{}` was terminated by a signal", self.name)
            }
            (CheckStatus::TimedOut, _) => {
                format!(
                    "check `{}` timed out after {}s",
                    self.name, self.timeout_secs
                )
            }
            (CheckStatus::Passed, _) => format!("check `{}` passed", self.name),
            (CheckStatus::Skipped, _) => format!("check `{}` was skipped", self.name),
        }
    }

    pub fn to_call_record(&self, index: usize) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: CHECK_TOOL_NAME.to_string(),
            arguments: serde_json::json!({ "name": self.name, "command": self.command }),
            call_id: format!("check-{index}"),
            result: serde_json::to_string(self).expect("CheckOutcome serialises"),
            effect: None,
        }
    }
}

pub fn first_failure(outcomes: &[CheckOutcome]) -> Option<&CheckOutcome> {
    outcomes.iter().find(|outcome| outcome.is_failure())
}

pub fn passed_summary(outcomes: &[CheckOutcome]) -> String {
    let passed = outcomes
        .iter()
        .filter(|outcome| outcome.status == CheckStatus::Passed)
        .count();
    format!("{passed}/{} checks passed", outcomes.len())
}

/// Runs every declared check in order and returns one outcome per check. A
/// failing check does not stop the rest, so the report is complete; a check
/// that times out stops the run and marks the remaining checks skipped,
/// because its process may still be running in the container.
///
/// ```
/// use harness::verify::{run_checks, CheckRunner, CheckStatus, RawRun, VerifyConfig};
/// use std::time::Duration;
///
/// struct Fixed(i32);
/// impl CheckRunner for Fixed {
///     fn run(&self, _: &str, _: Duration) -> Result<RawRun, String> {
///         Ok(RawRun { exit_code: Some(self.0), timed_out: false, stdout: "ok".into(), stderr: String::new() })
///     }
/// }
///
/// let config = VerifyConfig::parse("[[check]]\nname = \"a\"\ncommand = \"true\"\n").unwrap();
/// let outcomes = run_checks(&config, &Fixed(1)).unwrap();
/// assert_eq!(outcomes[0].status, CheckStatus::Failed);
/// assert_eq!(outcomes[0].exit_code, Some(1));
/// ```
pub fn run_checks(
    config: &VerifyConfig,
    runner: &dyn CheckRunner,
) -> Result<Vec<CheckOutcome>, ExecError> {
    let mut outcomes: Vec<CheckOutcome> = Vec::new();
    let mut stopped = false;
    for check in config.checks() {
        if stopped {
            outcomes.push(CheckOutcome {
                name: check.name.clone(),
                command: check.command.clone(),
                status: CheckStatus::Skipped,
                exit_code: None,
                timeout_secs: check.timeout_secs,
                duration_ms: 0,
                output: String::new(),
                output_truncated: false,
            });
            continue;
        }
        let started = Instant::now();
        let raw = runner
            .run(&check.command, Duration::from_secs(check.timeout_secs))
            .map_err(|message| ExecError {
                check: check.name.clone(),
                message,
                completed: outcomes.clone(),
            })?;
        let status = if raw.timed_out {
            CheckStatus::TimedOut
        } else if raw.exit_code == Some(0) {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        };
        stopped = raw.timed_out;
        let (output, output_truncated) = truncate_tail(&combine(&raw), MAX_OUTPUT_BYTES);
        outcomes.push(CheckOutcome {
            name: check.name.clone(),
            command: check.command.clone(),
            status,
            exit_code: raw.exit_code,
            timeout_secs: check.timeout_secs,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            output,
            output_truncated,
        });
    }
    Ok(outcomes)
}

pub async fn run_verification(
    config: VerifyConfig,
    runner: Arc<dyn CheckRunner>,
) -> Result<Vec<CheckOutcome>, ExecError> {
    tokio::task::spawn_blocking(move || run_checks(&config, runner.as_ref()))
        .await
        .map_err(|e| ExecError {
            check: String::new(),
            message: format!("the verification worker failed: {e}"),
            completed: Vec::new(),
        })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Mutex;

    struct Scripted(Mutex<Vec<Result<RawRun, String>>>);

    impl Scripted {
        fn new(runs: Vec<Result<RawRun, String>>) -> Self {
            Self(Mutex::new(runs.into_iter().rev().collect()))
        }
    }

    impl CheckRunner for Scripted {
        fn run(&self, _command: &str, _timeout: Duration) -> Result<RawRun, String> {
            self.0
                .lock()
                .unwrap()
                .pop()
                .expect("scripted run available")
        }
    }

    fn raw(code: Option<i32>, stdout: &str, stderr: &str) -> Result<RawRun, String> {
        Ok(RawRun {
            exit_code: code,
            timed_out: false,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }

    fn timed_out() -> Result<RawRun, String> {
        Ok(RawRun {
            exit_code: None,
            timed_out: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    fn config(names: &[&str]) -> VerifyConfig {
        let text: String = names
            .iter()
            .map(|n| {
                format!("[[check]]\nname = \"{n}\"\ncommand = \"run {n}\"\ntimeout_secs = 7\n")
            })
            .collect();
        VerifyConfig::parse(&text).unwrap()
    }

    #[test]
    fn every_check_runs_and_a_failure_does_not_hide_later_results() {
        let runner = Scripted::new(vec![raw(Some(1), "bad", ""), raw(Some(0), "fine", "")]);
        let outcomes = run_checks(&config(&["fmt", "test"]), &runner).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].status, CheckStatus::Failed);
        assert_eq!(outcomes[0].exit_code, Some(1));
        assert_eq!(outcomes[0].command, "run fmt");
        assert_eq!(outcomes[0].timeout_secs, 7);
        assert_eq!(outcomes[1].status, CheckStatus::Passed);
        assert_eq!(first_failure(&outcomes).unwrap().name, "fmt");
        assert_eq!(passed_summary(&outcomes), "1/2 checks passed");
    }

    #[test]
    fn stderr_is_appended_after_stdout_under_a_marker() {
        let runner = Scripted::new(vec![raw(Some(0), "out", "err")]);
        let outcomes = run_checks(&config(&["a"]), &runner).unwrap();
        assert_eq!(outcomes[0].output, "out\n[stderr]\nerr");
        let runner = Scripted::new(vec![raw(Some(0), "out", "")]);
        assert_eq!(
            run_checks(&config(&["a"]), &runner).unwrap()[0].output,
            "out"
        );
    }

    #[test]
    fn a_signalled_check_fails_without_an_exit_code() {
        let runner = Scripted::new(vec![raw(None, "", "")]);
        let outcome = run_checks(&config(&["a"]), &runner).unwrap().remove(0);
        assert_eq!(outcome.status, CheckStatus::Failed);
        assert_eq!(outcome.exit_code, None);
        assert_eq!(
            outcome.failure_message(),
            "check `a` was terminated by a signal"
        );
    }

    #[test]
    fn a_timeout_stops_the_run_and_skips_the_rest() {
        let runner = Scripted::new(vec![raw(Some(0), "", ""), timed_out()]);
        let outcomes = run_checks(&config(&["a", "b", "c", "d"]), &runner).unwrap();
        let statuses: Vec<_> = outcomes.iter().map(|o| o.status).collect();
        assert_eq!(
            statuses,
            [
                CheckStatus::Passed,
                CheckStatus::TimedOut,
                CheckStatus::Skipped,
                CheckStatus::Skipped
            ]
        );
        assert_eq!(
            outcomes[1].failure_message(),
            "check `b` timed out after 7s"
        );
        assert_eq!(outcomes[3].duration_ms, 0);
        assert_eq!(outcomes[3].exit_code, None);
        assert_eq!(first_failure(&outcomes).unwrap().name, "b");
        assert_eq!(passed_summary(&outcomes), "1/4 checks passed");
    }

    #[test]
    fn a_runner_error_aborts_and_keeps_the_completed_outcomes() {
        let runner = Scripted::new(vec![raw(Some(0), "", ""), Err("no runtime".to_string())]);
        let err = run_checks(&config(&["a", "b", "c"]), &runner).unwrap_err();
        assert_eq!(err.check, "b");
        assert_eq!(err.message, "no runtime");
        assert_eq!(err.completed.len(), 1);
        assert_eq!(err.to_string(), "could not run check `b`: no runtime");
    }

    #[test]
    fn messages_cover_every_status() {
        let mut outcome = run_checks(&config(&["a"]), &Scripted::new(vec![raw(Some(0), "", "")]))
            .unwrap()
            .remove(0);
        assert_eq!(outcome.failure_message(), "check `a` passed");
        assert!(!outcome.is_failure());
        outcome.status = CheckStatus::Skipped;
        assert_eq!(outcome.failure_message(), "check `a` was skipped");
        assert!(!outcome.is_failure());
        outcome.status = CheckStatus::Failed;
        outcome.exit_code = Some(2);
        assert_eq!(
            outcome.failure_message(),
            "check `a` failed with exit code 2"
        );
        assert!(outcome.is_failure());
    }

    #[test]
    fn an_all_passing_run_has_no_failure() {
        let runner = Scripted::new(vec![raw(Some(0), "", ""), raw(Some(0), "", "")]);
        let outcomes = run_checks(&config(&["a", "b"]), &runner).unwrap();
        assert!(first_failure(&outcomes).is_none());
        assert_eq!(passed_summary(&outcomes), "2/2 checks passed");
    }

    #[test]
    fn long_output_keeps_the_tail_and_says_it_was_truncated() {
        let body = format!("{}{}", "x".repeat(MAX_OUTPUT_BYTES * 2), "LAST");
        let runner = Scripted::new(vec![raw(Some(1), &body, "")]);
        let outcome = run_checks(&config(&["a"]), &runner).unwrap().remove(0);
        assert!(outcome.output_truncated);
        assert_eq!(outcome.output.len(), MAX_OUTPUT_BYTES);
        assert!(outcome.output.ends_with("LAST"));
    }

    #[test]
    fn call_records_carry_the_outcome_as_json() {
        let runner = Scripted::new(vec![raw(Some(3), "boom", "")]);
        let outcome = run_checks(&config(&["lint"]), &runner).unwrap().remove(0);
        let record = outcome.to_call_record(4);
        assert_eq!(record.tool_name, CHECK_TOOL_NAME);
        assert_eq!(record.call_id, "check-4");
        assert_eq!(record.arguments["name"], "lint");
        assert_eq!(record.arguments["command"], "run lint");
        assert!(record.effect.is_none());
        let back: CheckOutcome = serde_json::from_str(&record.result).unwrap();
        assert_eq!(back, outcome);
    }

    #[tokio::test]
    async fn run_verification_runs_off_the_async_thread() {
        let runner: Arc<dyn CheckRunner> = Arc::new(Scripted::new(vec![raw(Some(0), "ok", "")]));
        let outcomes = run_verification(config(&["a"]), runner).await.unwrap();
        assert_eq!(outcomes[0].status, CheckStatus::Passed);
    }

    #[tokio::test]
    async fn a_panicking_runner_becomes_an_exec_error() {
        let runner: Arc<dyn CheckRunner> = Arc::new(Scripted::new(vec![]));
        let err = run_verification(config(&["a"]), runner).await.unwrap_err();
        assert!(err.message.contains("worker failed"), "{}", err.message);
        assert!(err.completed.is_empty());
    }

    proptest! {
        #[test]
        fn truncate_tail_is_a_suffix_within_the_limit_on_a_char_boundary(text in ".{0,200}", max in 0usize..64) {
            let (kept, truncated) = truncate_tail(&text, max);
            prop_assert!(text.ends_with(&kept));
            prop_assert!(kept.len() <= max.max(text.len().min(max)) || !truncated);
            prop_assert_eq!(truncated, text.len() > max);
            if truncated {
                prop_assert!(kept.len() <= max);
                prop_assert!(max - kept.len() < 4);
            } else {
                prop_assert_eq!(kept, text);
            }
        }

        #[test]
        fn verdict_matches_exit_codes(codes in prop::collection::vec(prop::option::of(0i32..4), 1..6)) {
            let names: Vec<String> = (0..codes.len()).map(|i| format!("c{i}")).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let runner = Scripted::new(codes.iter().map(|c| raw(*c, "", "")).collect());
            let outcomes = run_checks(&config(&refs), &runner).unwrap();
            let all_zero = codes.iter().all(|c| *c == Some(0));
            prop_assert_eq!(first_failure(&outcomes).is_none(), all_zero);
            prop_assert_eq!(outcomes.len(), codes.len());
        }
    }
}
