mod config;
mod outcome;
mod runner;
mod status;

pub use config::{
    Check, VerifyConfig, VerifyConfigError, DEFAULT_TIMEOUT_SECS, MAX_CHECKS, MAX_COMMAND_BYTES,
    MAX_NAME_LEN, MAX_TIMEOUT_SECS, MAX_VERIFY_BYTES, VERIFY_FILE,
};
pub use outcome::{
    first_failure, passed_summary, run_checks, run_verification, truncate_tail, CheckOutcome,
    CheckStatus, ExecError, CHECK_TOOL_NAME, MAX_OUTPUT_BYTES,
};
pub use runner::{CheckRunner, ProcessCheckRunner, RawRun, MAX_CAPTURE_BYTES};
pub use status::{
    is_verifier, status_from_exec_error, status_from_outcomes, status_requires_container,
    ERROR_CHECKS_INVALID, ERROR_EXEC_FAILED, ERROR_REQUIRES_CONTAINER, ERROR_VERIFICATION_FAILED,
    REQUIRES_CONTAINER_MESSAGE, VERIFIER_TOOL,
};

#[cfg(test)]
pub(crate) mod test_support {
    use super::{CheckRunner, RawRun};
    use std::sync::Mutex;
    use std::time::Duration;

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
