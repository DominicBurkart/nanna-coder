mod config;
mod outcome;
mod runner;

pub use config::{
    Check, VerifyConfig, VerifyConfigError, DEFAULT_TIMEOUT_SECS, MAX_CHECKS, MAX_COMMAND_BYTES,
    MAX_NAME_LEN, MAX_TIMEOUT_SECS, MAX_VERIFY_BYTES, VERIFY_FILE,
};
pub use outcome::{
    first_failure, passed_summary, run_checks, run_verification, truncate_tail, CheckOutcome,
    CheckStatus, ExecError, CHECK_TOOL_NAME, MAX_OUTPUT_BYTES,
};
pub use runner::{CheckRunner, ProcessCheckRunner, RawRun, MAX_CAPTURE_BYTES};
