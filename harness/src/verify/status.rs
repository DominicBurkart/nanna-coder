use chrono::{DateTime, Utc};

use super::outcome::{first_failure, passed_summary, CheckOutcome, ExecError};
use crate::budget::BudgetReport;
use crate::entities::context::types::ToolCallRecord;
use crate::identity::AgentIdentity;
use crate::qa::QaSummary;
use crate::task::{FailureDiagnostics, TaskResult, TaskStatus};

pub const VERIFIER_TOOL: &str = "run_checks";

pub const ERROR_CHECKS_INVALID: &str = "VerifierChecksInvalid";

pub const ERROR_REQUIRES_CONTAINER: &str = "VerifierRequiresContainer";

pub const ERROR_EXEC_FAILED: &str = "VerifierExecFailed";

pub const ERROR_VERIFICATION_FAILED: &str = "VerificationFailed";

pub const REQUIRES_CONTAINER_MESSAGE: &str = "a verifier runs its checks inside the repository's dev container, but this repository has not opted in to one (flake.nix and .devcontainer/); the host never runs repository commands";

/// Whether `identity` is a verifier card: its only tool is `run_checks`, so
/// it has no model-facing capability and the harness runs the repository's
/// declared checks itself.
///
/// ```
/// use harness::identity::AgentIdentity;
/// use harness::verify::is_verifier;
///
/// let card = |tools: &str| format!(r#"
/// [identity]
/// name = "verifier"
/// description = "Runs the declared checks."
/// loop = "inner"
/// model = "m"
/// system_prompt = {{ inline = "Verify." }}
///
/// [scope]
/// repos = ["github.com/example/repo"]
/// paths = []
/// max_effect = "workspace"
/// tools = {tools}
///
/// [limits]
/// max_iterations = 1
/// max_wall_clock_secs = 3600
/// max_concurrent = 1
/// "#);
/// let verifier = AgentIdentity::from_toml_str(&card(r#"["run_checks"]"#), "v.toml").unwrap();
/// assert!(is_verifier(&verifier));
/// let mixed = AgentIdentity::from_toml_str(&card(r#"["run_checks", "read_file"]"#), "m.toml").unwrap();
/// assert!(!is_verifier(&mixed));
/// ```
pub fn is_verifier(identity: &AgentIdentity) -> bool {
    matches!(identity.scope.tools.as_slice(), [only] if only.as_str() == VERIFIER_TOOL)
}

fn records(outcomes: &[CheckOutcome]) -> Vec<ToolCallRecord> {
    outcomes
        .iter()
        .enumerate()
        .map(|(index, outcome)| outcome.to_call_record(index))
        .collect()
}

fn diagnostics(
    error_type: &str,
    history: Vec<ToolCallRecord>,
    last: Option<ToolCallRecord>,
) -> FailureDiagnostics {
    FailureDiagnostics {
        error_type: error_type.to_string(),
        iterations_completed: 0,
        last_tool_call: last,
        partial_changes: None,
        tool_call_history: history,
        last_agent_state: None,
        conversation_snapshot: None,
        denials: Vec::new(),
        action_audit: Vec::new(),
    }
}

/// Turns the outcomes of a verification run into the task's terminal
/// status. Any failing or timed-out check makes the task `failed`, naming
/// that check in the error and as `last_tool_call`; every outcome is in
/// `tool_call_history`. A fully passing run is `completed` with the outcomes
/// in `qa_summary.checks`.
pub fn status_from_outcomes(
    outcomes: Vec<CheckOutcome>,
    model: String,
    budget: BudgetReport,
    finished_at: DateTime<Utc>,
) -> TaskStatus {
    let history = records(&outcomes);
    if let Some(failure) = first_failure(&outcomes) {
        let index = outcomes
            .iter()
            .position(|outcome| std::ptr::eq(outcome, failure))
            .unwrap_or_default();
        return TaskStatus::Failed {
            finished_at,
            error: failure.failure_message(),
            diagnostics: diagnostics(
                ERROR_VERIFICATION_FAILED,
                history.clone(),
                history.get(index).cloned(),
            ),
        };
    }
    TaskStatus::Completed {
        finished_at,
        result: TaskResult {
            result_summary: passed_summary(&outcomes),
            changes_patch: None,
            format_patch: None,
            files_modified: Vec::new(),
            tool_calls_made: history,
            denials: Vec::new(),
            action_audit: Vec::new(),
            iterations: 0,
            model_used: model,
            blast_radius: None,
            qa_summary: QaSummary {
                checks: outcomes,
                ..QaSummary::default()
            },
            budget,
        },
    }
}

pub fn status_requires_container(finished_at: DateTime<Utc>) -> TaskStatus {
    TaskStatus::Failed {
        finished_at,
        error: REQUIRES_CONTAINER_MESSAGE.to_string(),
        diagnostics: diagnostics(ERROR_REQUIRES_CONTAINER, Vec::new(), None),
    }
}

pub fn status_from_exec_error(error: ExecError, finished_at: DateTime<Utc>) -> TaskStatus {
    let history = records(&error.completed);
    TaskStatus::Failed {
        finished_at,
        error: error.to_string(),
        diagnostics: diagnostics(ERROR_EXEC_FAILED, history.clone(), history.last().cloned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{CheckStatus, ExecError};

    fn outcome(name: &str, status: CheckStatus, code: Option<i32>) -> CheckOutcome {
        CheckOutcome {
            name: name.to_string(),
            command: format!("run {name}"),
            status,
            exit_code: code,
            timeout_secs: 60,
            duration_ms: 5,
            output: String::new(),
            output_truncated: false,
        }
    }

    fn card(tools: &str) -> AgentIdentity {
        let toml = format!(
            r#"
[identity]
name = "verifier"
description = "Runs the declared checks."
loop = "inner"
model = "m"
system_prompt = {{ inline = "Verify." }}

[scope]
repos = ["github.com/example/repo"]
paths = []
max_effect = "workspace"
tools = {tools}

[limits]
max_iterations = 1
max_wall_clock_secs = 3600
max_concurrent = 1
"#
        );
        AgentIdentity::from_toml_str(&toml, "verifier.toml").unwrap()
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn only_a_card_whose_sole_tool_is_run_checks_is_a_verifier() {
        assert!(is_verifier(&card(r#"["run_checks"]"#)));
        assert!(!is_verifier(&card(r#"["run_checks", "read_file"]"#)));
        assert!(!is_verifier(&card(r#"["run_*"]"#)));
        assert!(!is_verifier(&card(r#"["read_file"]"#)));
        assert!(!is_verifier(&card("[]")));
        assert!(!is_verifier(&crate::identity::example()));
    }

    #[test]
    fn all_passing_outcomes_complete_the_task_with_checks_in_the_qa_summary() {
        let outcomes = vec![
            outcome("fmt", CheckStatus::Passed, Some(0)),
            outcome("test", CheckStatus::Passed, Some(0)),
        ];
        let TaskStatus::Completed { result, .. } = status_from_outcomes(
            outcomes.clone(),
            "m".to_string(),
            BudgetReport::default(),
            now(),
        ) else {
            panic!("expected a completed task");
        };
        assert_eq!(result.result_summary, "2/2 checks passed");
        assert_eq!(result.qa_summary.checks, outcomes);
        assert!(!result.qa_summary.is_empty());
        assert_eq!(result.tool_calls_made.len(), 2);
        assert_eq!(result.model_used, "m");
        assert_eq!(result.iterations, 0);
        assert!(result.changes_patch.is_none() && result.format_patch.is_none());
        assert!(result.files_modified.is_empty() && result.denials.is_empty());
        assert_eq!(result.to_json()["qa_summary"]["checks"][1]["name"], "test");
    }

    #[test]
    fn a_failing_check_fails_the_task_naming_that_check() {
        let outcomes = vec![
            outcome("fmt", CheckStatus::Passed, Some(0)),
            outcome("clippy", CheckStatus::Failed, Some(101)),
            outcome("test", CheckStatus::Passed, Some(0)),
        ];
        let TaskStatus::Failed {
            error, diagnostics, ..
        } = status_from_outcomes(outcomes, "m".to_string(), BudgetReport::default(), now())
        else {
            panic!("expected a failed task");
        };
        assert_eq!(error, "check `clippy` failed with exit code 101");
        assert_eq!(diagnostics.error_type, ERROR_VERIFICATION_FAILED);
        assert_eq!(diagnostics.tool_call_history.len(), 3);
        let last = diagnostics.last_tool_call.unwrap();
        assert_eq!(last.arguments["name"], "clippy");
        assert_eq!(last.call_id, "check-1");
        assert_eq!(diagnostics.iterations_completed, 0);
        assert!(diagnostics.partial_changes.is_none());
        assert!(diagnostics.denials.is_empty() && diagnostics.action_audit.is_empty());
    }

    #[test]
    fn a_timed_out_check_fails_the_task_and_skipped_checks_are_listed() {
        let outcomes = vec![
            outcome("build", CheckStatus::TimedOut, None),
            outcome("test", CheckStatus::Skipped, None),
        ];
        let TaskStatus::Failed {
            error, diagnostics, ..
        } = status_from_outcomes(outcomes, "m".to_string(), BudgetReport::default(), now())
        else {
            panic!("expected a failed task");
        };
        assert_eq!(error, "check `build` timed out after 60s");
        assert_eq!(diagnostics.tool_call_history.len(), 2);
        assert_eq!(
            diagnostics.last_tool_call.unwrap().arguments["name"],
            "build"
        );
    }

    #[test]
    fn an_exec_error_fails_with_its_own_label_and_keeps_completed_checks() {
        let error = ExecError {
            check: "test".to_string(),
            message: "no runtime".to_string(),
            completed: vec![outcome("fmt", CheckStatus::Passed, Some(0))],
        };
        let TaskStatus::Failed {
            error, diagnostics, ..
        } = status_from_exec_error(error, now())
        else {
            panic!("expected a failed task");
        };
        assert_eq!(error, "could not run check `test`: no runtime");
        assert_eq!(diagnostics.error_type, ERROR_EXEC_FAILED);
        assert_eq!(diagnostics.tool_call_history.len(), 1);
        assert_eq!(diagnostics.last_tool_call.unwrap().arguments["name"], "fmt");

        let none = ExecError {
            check: "a".to_string(),
            message: "x".to_string(),
            completed: vec![],
        };
        let TaskStatus::Failed { diagnostics, .. } = status_from_exec_error(none, now()) else {
            panic!("expected a failed task");
        };
        assert!(diagnostics.last_tool_call.is_none());
    }

    #[test]
    fn a_missing_container_fails_with_its_own_label_and_no_history() {
        let TaskStatus::Failed {
            error, diagnostics, ..
        } = status_requires_container(now())
        else {
            panic!("expected a failed task");
        };
        assert_eq!(error, REQUIRES_CONTAINER_MESSAGE);
        assert_eq!(diagnostics.error_type, ERROR_REQUIRES_CONTAINER);
        assert!(diagnostics.tool_call_history.is_empty());
        assert!(diagnostics.last_tool_call.is_none());
    }

    #[test]
    fn error_labels_are_distinct_and_pascal_case_like_the_existing_ones() {
        let labels = [
            ERROR_CHECKS_INVALID,
            ERROR_REQUIRES_CONTAINER,
            ERROR_EXEC_FAILED,
            ERROR_VERIFICATION_FAILED,
        ];
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len());
        for label in labels {
            assert!(label.chars().next().unwrap().is_ascii_uppercase());
            assert!(label.chars().all(|c| c.is_ascii_alphanumeric()));
        }
    }
}
