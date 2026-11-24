use chrono::{DateTime, Utc};

use super::outcome::{first_failure, passed_summary, CheckOutcome, CheckStatus, ExecError};
use crate::action_auditor::ActionAuditLogEntry;
use crate::budget::BudgetReport;
use crate::effects::EffectClass;
use crate::entities::context::types::ToolCallRecord;
use crate::identity::AgentIdentity;
use crate::qa::QaSummary;
use crate::task::{FailureDiagnostics, TaskResult, TaskStatus};

pub const VERIFIER_TOOL: &str = "run_checks";

pub const ERROR_CHECKS_INVALID: &str = "VerifierChecksInvalid";

pub const ERROR_REQUIRES_CONTAINER: &str = "VerifierRequiresContainer";

pub const ERROR_EXEC_FAILED: &str = "VerifierExecFailed";

pub const ERROR_VERIFICATION_FAILED: &str = "VerificationFailed";

pub const ERROR_SCOPE: &str = "ScopeError";

pub const ERROR_ACTION_DENIED: &str = "ScopeError";

pub const ERROR_ITERATION_BUDGET: &str = "MaxIterationsExceeded";

pub const VERIFIER_EFFECT: EffectClass = EffectClass::Workspace;

pub const REQUIRES_CONTAINER_MESSAGE: &str = "a verifier runs its checks inside the repository's dev container, but this repository has not opted in to one (flake.nix and .devcontainer/); the host never runs repository commands";

#[doc = r##"Whether `identity` is a verifier card: its only tool is `run_checks`, so
it has no model-facing capability and the harness runs the repository's
declared checks itself.

```
use harness::identity::AgentIdentity;
use harness::verify::is_verifier;

let card = |tools: &str| format!(r#"
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
"#);
let verifier = AgentIdentity::from_toml_str(&card(r#"["run_checks"]"#), "v.toml").unwrap();
assert!(is_verifier(&verifier));
let mixed = AgentIdentity::from_toml_str(&card(r#"["run_checks", "read_file"]"#), "m.toml").unwrap();
assert!(!is_verifier(&mixed));
```"##]
pub fn is_verifier(identity: &AgentIdentity) -> bool {
    matches!(identity.scope.tools.as_slice(), [only] if only.as_str() == VERIFIER_TOOL)
}

#[doc = r##"Why `identity` may not run as a verifier, or `None` when it may. A
verifier's ceiling is fixed at `workspace`: its checks are `workspace`
effects, and any higher ceiling would give its dev container network access
(and a lower one would make the gate refuse every check). A card for any
other identity is not this function's concern.

```
use harness::effects::EffectClass;
use harness::identity::AgentIdentity;
use harness::verify::ceiling_violation;

let card = |effect: &str| format!(r#"
[identity]
name = "verifier"
description = "Runs the declared checks."
loop = "inner"
model = "m"
system_prompt = {{ inline = "Verify." }}

[scope]
repos = ["github.com/example/repo"]
paths = []
max_effect = "{effect}"
tools = ["run_checks"]

[limits]
max_iterations = 1
max_wall_clock_secs = 3600
max_concurrent = 1
"#);
assert!(AgentIdentity::from_toml_str(&card("production"), "v.toml").is_err());
let mut identity = AgentIdentity::from_toml_str(&card("workspace"), "v.toml").unwrap();
assert!(ceiling_violation(&identity).is_none());
identity.scope.max_effect = EffectClass::Sandbox;
assert!(ceiling_violation(&identity).unwrap().contains("sandbox"));
```"##]
pub fn ceiling_violation(identity: &AgentIdentity) -> Option<String> {
    let declared = identity.scope.max_effect;
    (is_verifier(identity) && declared != VERIFIER_EFFECT).then(|| {
        format!(
            "a verifier's ceiling is fixed at `{VERIFIER_EFFECT}` but its card declares `{declared}`"
        )
    })
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
    iterations: usize,
    action_audit: Vec<ActionAuditLogEntry>,
) -> FailureDiagnostics {
    FailureDiagnostics {
        error_type: error_type.to_string(),
        iterations_completed: iterations,
        last_tool_call: last,
        partial_changes: None,
        tool_call_history: history,
        last_agent_state: None,
        conversation_snapshot: None,
        denials: Vec::new(),
        action_audit,
    }
}

#[doc = r#"Turns the outcomes of a verification run into the task's terminal
status. Any failing or timed-out check makes the task `failed`, naming
that check in the error and as `last_tool_call`; every outcome is in
`tool_call_history`. A fully passing run is `completed` with the outcomes
in `qa_summary.checks`."#]
pub fn status_from_outcomes(
    outcomes: Vec<CheckOutcome>,
    model: String,
    budget: BudgetReport,
    action_audit: Vec<ActionAuditLogEntry>,
    finished_at: DateTime<Utc>,
) -> TaskStatus {
    let history = records(&outcomes);
    let iterations = outcomes
        .iter()
        .filter(|outcome| outcome.status != CheckStatus::Skipped)
        .count();
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
                iterations,
                action_audit,
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
            action_audit,
            iterations,
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
        diagnostics: diagnostics(ERROR_REQUIRES_CONTAINER, Vec::new(), None, 0, Vec::new()),
    }
}

pub fn status_from_exec_error(
    error: ExecError,
    action_audit: Vec<ActionAuditLogEntry>,
    finished_at: DateTime<Utc>,
) -> TaskStatus {
    let history = records(&error.completed);
    let label = if error.denied {
        ERROR_ACTION_DENIED
    } else {
        ERROR_EXEC_FAILED
    };
    TaskStatus::Failed {
        finished_at,
        error: error.to_string(),
        diagnostics: diagnostics(
            label,
            history.clone(),
            history.last().cloned(),
            history.len(),
            action_audit,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_auditor::ActionAuditLogEntry;
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
            Vec::new(),
            now(),
        ) else {
            panic!("expected a completed task");
        };
        assert_eq!(result.result_summary, "2/2 checks passed");
        assert_eq!(result.qa_summary.checks, outcomes);
        assert!(!result.qa_summary.is_empty());
        assert_eq!(result.tool_calls_made.len(), 2);
        assert_eq!(result.model_used, "m");
        assert_eq!(result.iterations, 2);
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
        } = status_from_outcomes(
            outcomes,
            "m".to_string(),
            BudgetReport::default(),
            Vec::new(),
            now(),
        )
        else {
            panic!("expected a failed task");
        };
        assert_eq!(error, "check `clippy` failed with exit code 101");
        assert_eq!(diagnostics.error_type, ERROR_VERIFICATION_FAILED);
        assert_eq!(diagnostics.tool_call_history.len(), 3);
        let last = diagnostics.last_tool_call.unwrap();
        assert_eq!(last.arguments["name"], "clippy");
        assert_eq!(last.call_id, "check-1");
        assert_eq!(diagnostics.iterations_completed, 3);
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
        } = status_from_outcomes(
            outcomes,
            "m".to_string(),
            BudgetReport::default(),
            Vec::new(),
            now(),
        )
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
            denied: false,
        };
        let TaskStatus::Failed {
            error, diagnostics, ..
        } = status_from_exec_error(error, Vec::new(), now())
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
            denied: false,
        };
        let TaskStatus::Failed { diagnostics, .. } =
            status_from_exec_error(none, Vec::new(), now())
        else {
            panic!("expected a failed task");
        };
        assert!(diagnostics.last_tool_call.is_none());
    }

    #[test]
    fn a_refused_check_fails_with_the_scope_label_and_keeps_the_audit_records() {
        let refused = ExecError {
            check: "test".to_string(),
            message: "refused".to_string(),
            completed: vec![outcome("fmt", CheckStatus::Passed, Some(0))],
            denied: true,
        };
        let TaskStatus::Failed { diagnostics, .. } =
            status_from_exec_error(refused, vec![audit_entry()], now())
        else {
            panic!("expected a failed task");
        };
        assert_eq!(diagnostics.error_type, ERROR_ACTION_DENIED);
        assert_eq!(diagnostics.action_audit.len(), 1);
        assert_eq!(diagnostics.iterations_completed, 1);
    }

    fn audit_entry() -> ActionAuditLogEntry {
        ActionAuditLogEntry {
            review: crate::action_auditor::ActionReview {
                identity: "verifier".to_string(),
                task_id: crate::task::TaskId("t".to_string()),
                tool: VERIFIER_TOOL.to_string(),
                args: serde_json::json!({}),
                effect_class: EffectClass::Workspace,
                prior_actions: Vec::new(),
            },
            verdict: crate::action_auditor::ActionVerdict::Allow,
        }
    }

    #[test]
    fn audit_records_and_executed_checks_are_carried_into_both_outcomes() {
        let outcomes = vec![
            outcome("a", CheckStatus::Passed, Some(0)),
            outcome("b", CheckStatus::Skipped, None),
        ];
        let TaskStatus::Completed { result, .. } = status_from_outcomes(
            outcomes.clone(),
            "m".to_string(),
            BudgetReport::default(),
            vec![audit_entry()],
            now(),
        ) else {
            panic!("expected a completed task");
        };
        assert_eq!(result.action_audit.len(), 1);
        assert_eq!(result.iterations, 1);
        let failing = vec![
            outcome("a", CheckStatus::Failed, Some(1)),
            outcome("b", CheckStatus::Passed, Some(0)),
        ];
        let TaskStatus::Failed { diagnostics, .. } = status_from_outcomes(
            failing,
            "m".to_string(),
            BudgetReport::default(),
            vec![audit_entry(), audit_entry()],
            now(),
        ) else {
            panic!("expected a failed task");
        };
        assert_eq!(diagnostics.action_audit.len(), 2);
        assert_eq!(diagnostics.iterations_completed, 2);
    }

    fn card_at(effect: EffectClass) -> String {
        format!(
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
max_effect = "{effect}"
tools = ["run_checks"]

[limits]
max_iterations = 1
max_wall_clock_secs = 3600
max_concurrent = 1
"#
        )
    }

    #[test]
    fn the_effect_class_set_is_pinned_so_a_new_class_cannot_skip_the_ceiling_tests() {
        assert_eq!(EffectClass::ALL.len(), 6);
        assert_eq!(VERIFIER_EFFECT, EffectClass::Workspace);
    }

    #[test]
    fn a_verifier_card_with_any_ceiling_but_workspace_is_refused_at_load() {
        for effect in EffectClass::ALL {
            let loaded = AgentIdentity::from_toml_str(&card_at(effect), "v.toml");
            if effect == EffectClass::Workspace {
                assert!(is_verifier(&loaded.unwrap()));
                continue;
            }
            let error = loaded.unwrap_err().to_string();
            assert!(error.contains("scope.max_effect"), "{effect}: {error}");
            assert!(error.contains("fixed at `workspace`"), "{effect}: {error}");
            assert!(error.contains(&format!("declares `{effect}`")), "{error}");
        }
    }

    #[test]
    fn a_ceiling_widened_after_load_is_still_a_violation_and_other_cards_are_not() {
        let mut verifier = card_at_workspace();
        assert_eq!(ceiling_violation(&verifier), None);
        for effect in EffectClass::ALL {
            verifier.scope.max_effect = effect;
            assert_eq!(
                ceiling_violation(&verifier).is_some(),
                effect != EffectClass::Workspace,
                "{effect}"
            );
            let mut other = crate::identity::example();
            other.scope.max_effect = effect;
            assert_eq!(ceiling_violation(&other), None, "{effect}");
        }
    }

    fn card_at_workspace() -> AgentIdentity {
        AgentIdentity::from_toml_str(&card_at(EffectClass::Workspace), "v.toml").unwrap()
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
