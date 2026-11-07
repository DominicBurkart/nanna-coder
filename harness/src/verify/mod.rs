mod config;

pub use config::{
    Check, VerifyConfig, VerifyConfigError, DEFAULT_TIMEOUT_SECS, MAX_CHECKS, MAX_COMMAND_BYTES,
    MAX_NAME_LEN, MAX_TIMEOUT_SECS, MAX_VERIFY_BYTES, VERIFY_FILE,
};
