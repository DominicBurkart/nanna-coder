//! Unit tests for `ModelError` Display format strings.
//!
//! `ModelError` derives its `Display` impl via `thiserror` with custom
//! `#[error("...")]` literals. These tests pin each variant's human-readable
//! message so that a future edit to the format string is visible in the diff
//! and not silently broken. The messages surface in logs, API error responses,
//! and user-facing CLI output, so correctness matters.

use model::provider::ModelError;

/// `ModelNotFound` should name the missing model.
#[test]
fn model_not_found_message_contains_model_name() {
    let err = ModelError::ModelNotFound {
        model: "qwen3:72b".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("qwen3:72b"),
        "ModelNotFound message must contain the model name: {msg}"
    );
    assert!(
        msg.contains("not found") || msg.to_lowercase().contains("not found"),
        "ModelNotFound message must say 'not found': {msg}"
    );
}

/// `InvalidConfig` should surface the descriptive message.
#[test]
fn invalid_config_message_contains_description() {
    let err = ModelError::InvalidConfig {
        message: "base_url is empty".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("base_url is empty"),
        "InvalidConfig message must include the detail: {msg}"
    );
}

/// `ServiceUnavailable` should surface the descriptive message.
#[test]
fn service_unavailable_message_contains_description() {
    let err = ModelError::ServiceUnavailable {
        message: "Ollama is not running".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("Ollama is not running"),
        "ServiceUnavailable message must include the detail: {msg}"
    );
}

/// `RateLimit` has a fixed message with no interpolated fields.
#[test]
fn rate_limit_message_is_non_empty() {
    let msg = ModelError::RateLimit.to_string();
    assert!(
        !msg.is_empty(),
        "RateLimit must produce a non-empty Display message"
    );
    // The thiserror literal is "Rate limit exceeded".
    assert!(
        msg.to_lowercase().contains("rate limit"),
        "RateLimit message must mention 'rate limit': {msg}"
    );
}

/// `Authentication` has a fixed message with no interpolated fields.
#[test]
fn authentication_message_is_non_empty() {
    let msg = ModelError::Authentication.to_string();
    assert!(
        !msg.is_empty(),
        "Authentication must produce a non-empty Display message"
    );
    assert!(
        msg.to_lowercase().contains("auth"),
        "Authentication message must mention auth: {msg}"
    );
}

/// `Unknown` should surface the descriptive message.
#[test]
fn unknown_error_message_contains_description() {
    let err = ModelError::Unknown {
        message: "something went sideways".to_string(),
    };
    let msg = err.to_string();
    assert!(
        msg.contains("something went sideways"),
        "Unknown error message must include the detail: {msg}"
    );
}
