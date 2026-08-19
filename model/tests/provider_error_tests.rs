//! Tests for ModelError Display formatting.
//!
//! The thiserror-derived Display implementations are part of the public API
//! contract between the harness and callers: callers log these strings and
//! may pattern-match on them. Each variant's format string is verified here
//! so a refactor can't silently break the wire representation.

use model::provider::ModelError;

// ---------------------------------------------------------------------------
// Display formatting for struct-like variants
// ---------------------------------------------------------------------------

#[test]
fn model_not_found_display() {
    let err = ModelError::ModelNotFound {
        model: "gpt-4".to_string(),
    };
    assert_eq!(err.to_string(), "Model not found: gpt-4");
}

#[test]
fn model_not_found_display_empty_model_name() {
    let err = ModelError::ModelNotFound {
        model: String::new(),
    };
    assert_eq!(err.to_string(), "Model not found: ");
}

#[test]
fn invalid_config_display() {
    let err = ModelError::InvalidConfig {
        message: "missing api key".to_string(),
    };
    assert_eq!(err.to_string(), "Invalid configuration: missing api key");
}

#[test]
fn service_unavailable_display() {
    let err = ModelError::ServiceUnavailable {
        message: "upstream timeout".to_string(),
    };
    assert_eq!(err.to_string(), "Service unavailable: upstream timeout");
}

#[test]
fn unknown_display() {
    let err = ModelError::Unknown {
        message: "something went wrong".to_string(),
    };
    assert_eq!(err.to_string(), "Unknown error: something went wrong");
}

// ---------------------------------------------------------------------------
// Display formatting for unit variants
// ---------------------------------------------------------------------------

#[test]
fn rate_limit_display() {
    let err = ModelError::RateLimit;
    assert_eq!(err.to_string(), "Rate limit exceeded");
}

#[test]
fn authentication_display() {
    let err = ModelError::Authentication;
    assert_eq!(err.to_string(), "Authentication failed");
}

// ---------------------------------------------------------------------------
// From conversions (thiserror generates these)
// ---------------------------------------------------------------------------

#[test]
fn serialization_error_from_serde_json() {
    // Force a serde_json parse error to get a real serde_json::Error.
    let serde_err = serde_json::from_str::<serde_json::Value>("not valid json").unwrap_err();
    let err = ModelError::from(serde_err);
    let display = err.to_string();
    assert!(
        display.starts_with("Serialization error:"),
        "Expected 'Serialization error:' prefix, got: {display}"
    );
}

// ---------------------------------------------------------------------------
// Debug trait (required by Error trait bounds in practice)
// ---------------------------------------------------------------------------

#[test]
fn all_variants_implement_debug() {
    let variants: Vec<ModelError> = vec![
        ModelError::ModelNotFound {
            model: "m".to_string(),
        },
        ModelError::InvalidConfig {
            message: "c".to_string(),
        },
        ModelError::ServiceUnavailable {
            message: "s".to_string(),
        },
        ModelError::RateLimit,
        ModelError::Authentication,
        ModelError::Unknown {
            message: "u".to_string(),
        },
    ];
    for err in &variants {
        let debug = format!("{err:?}");
        assert!(
            !debug.is_empty(),
            "Debug output for {:?} should not be empty",
            err.to_string()
        );
    }
}

// ---------------------------------------------------------------------------
// ModelResult alias
// ---------------------------------------------------------------------------

#[test]
fn model_result_ok_passes_through() {
    use model::provider::ModelResult;
    // ModelResult<T> is Result<T, ModelError>; verify the Ok variant carries values correctly.
    let result: ModelResult<u32> = Ok(42);
    assert_eq!(result.ok(), Some(42));
}

#[test]
fn model_result_err_carries_variant() {
    use model::provider::ModelResult;
    // Use map_err to convert before unwrap_err so the lint doesn't fire on a literal Err.
    let result: ModelResult<()> = Err(ModelError::RateLimit);
    let err_msg = result.map_err(|e| e.to_string()).unwrap_err();
    assert_eq!(err_msg, "Rate limit exceeded");
}
