mod config;
mod outcome;
mod preflight;
mod runner;
mod status;
mod tool;

pub use config::{
    Check, VerifyConfig, VerifyConfigError, DEFAULT_TIMEOUT_SECS, MAX_CHECKS, MAX_COMMAND_BYTES,
    MAX_NAME_LEN, MAX_TIMEOUT_SECS, MAX_VERIFY_BYTES, VERIFY_FILE,
};
pub use outcome::{
    first_failure, passed_summary, truncate_tail, CheckOutcome, CheckStatus, ExecError,
    CHECK_TOOL_NAME, MAX_OUTPUT_BYTES,
};
pub use preflight::{preflight, Refusal};
pub use runner::{CheckRunner, ProcessCheckRunner, RawRun, MAX_CAPTURE_BYTES};
pub use status::{
    ceiling_violation, is_verifier, status_from_exec_error, status_from_outcomes,
    status_requires_container, ERROR_ACTION_DENIED, ERROR_CHECKS_INVALID, ERROR_EXEC_FAILED,
    ERROR_ITERATION_BUDGET, ERROR_REQUIRES_CONTAINER, ERROR_SCOPE, ERROR_VERIFICATION_FAILED,
    REQUIRES_CONTAINER_MESSAGE, VERIFIER_EFFECT, VERIFIER_TOOL,
};
pub use tool::{run_gated, CheckTool};

#[cfg(test)]
pub(crate) mod test_support {
    use super::{CheckRunner, RawRun};
    use crate::action_auditor::{
        ActionAuditError, ActionAuditLog, ActionAuditor, ActionContext, ActionGate, ActionReview,
        ActionVerdict,
    };
    use crate::auditor::{Reason, ReasonCode};
    use crate::effects::EffectClass;
    use crate::tools::ActionSubject;
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    pub(crate) struct Verdict(pub(crate) bool);

    #[async_trait]
    impl ActionAuditor for Verdict {
        fn name(&self) -> &str {
            "verdict-for-the-test"
        }

        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            Ok(if self.0 {
                ActionVerdict::Allow
            } else {
                ActionVerdict::block(vec![Reason::new(ReasonCode::Other, "blocked for the test")])
            })
        }
    }

    pub(crate) fn gate(allow: bool) -> Arc<ActionGate> {
        Arc::new(ActionGate::new(
            Arc::new(Verdict(allow)),
            ActionAuditLog::in_memory(),
        ))
    }

    pub(crate) fn subject() -> ActionSubject {
        ActionSubject {
            task_id: crate::task::TaskId("t1".to_string()),
            max_effect: EffectClass::Workspace,
            window: None,
            repo: "example/repo".to_string(),
            branch: None,
            pr: None,
            environment: None,
            paths: vec![],
            description: String::new(),
        }
    }

    #[derive(Default)]
    pub(crate) struct Recording(pub(crate) Mutex<Vec<String>>);

    impl CheckRunner for Recording {
        fn run(&self, command: &str, _timeout: Duration) -> Result<RawRun, String> {
            self.0.lock().unwrap().push(command.to_string());
            raw(Some(0), "", "")
        }
    }

    pub(crate) struct ScriptedRunner(Mutex<Vec<Result<RawRun, String>>>);

    impl ScriptedRunner {
        pub(crate) fn new(runs: Vec<Result<RawRun, String>>) -> Self {
            Self(Mutex::new(runs.into_iter().rev().collect()))
        }
    }

    impl CheckRunner for ScriptedRunner {
        fn run(&self, _command: &str, _timeout: Duration) -> Result<RawRun, String> {
            self.0
                .lock()
                .unwrap()
                .pop()
                .expect("scripted run available")
        }
    }

    pub(crate) fn raw(code: Option<i32>, stdout: &str, stderr: &str) -> Result<RawRun, String> {
        Ok(RawRun {
            exit_code: code,
            timed_out: false,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
        })
    }

    pub(crate) fn timed_out() -> Result<RawRun, String> {
        Ok(RawRun {
            exit_code: None,
            timed_out: true,
            stdout: String::new(),
            stderr: String::new(),
        })
    }
}
