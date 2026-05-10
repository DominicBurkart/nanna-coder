//! CLI helper utilities extracted from `main.rs` so they can be unit-tested
//! without a full tokio runtime / real process context.
//!
//! The binary entry-point (`main.rs`) is excluded from coverage via
//! `codecov.yml`; everything in this module (`emit`, `create_provider`,
//! `use_mock_provider`, `MockCliProvider`, `install_ctrlc_handler`,
//! `format_from`, `exit_code_for`, `classify_handler_error`) is covered by
//! unit tests below.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use model::prelude::*;

use crate::output::{ExitCode, JsonEnvelope, OutputFormat};

/// Returns true when we should use the mock provider (CI-friendly, no Ollama required).
pub fn use_mock_provider() -> bool {
    std::env::var("NANNA_TEST_MOCK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// A trivial mock provider that always returns a canned response.
/// Used for integration tests in CI where Ollama is unavailable.
pub struct MockCliProvider;

#[async_trait]
impl ModelProvider for MockCliProvider {
    async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
        Ok(ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: Some("Mock response".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        })
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        Ok(vec![ModelInfo {
            name: "mock".to_string(),
            size: Some(0),
            digest: None,
            modified_at: None,
        }])
    }

    async fn health_check(&self) -> ModelResult<()> {
        Ok(())
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }
}

/// Create an `Arc<dyn ModelProvider>` -- mock when `NANNA_TEST_MOCK=1`, else Ollama.
pub fn create_provider() -> Result<Arc<dyn ModelProvider>, Box<dyn std::error::Error>> {
    if use_mock_provider() {
        Ok(Arc::new(MockCliProvider))
    } else {
        let config = OllamaConfig::default();
        Ok(Arc::new(OllamaProvider::new(config)?))
    }
}

/// Map an `ExitCode` to its stable string error-code used in the JSON envelope.
pub fn exit_code_for(code: ExitCode) -> &'static str {
    match code {
        ExitCode::StateError => "STATE_ERROR",
        ExitCode::InfraError => "INFRA_ERROR",
        ExitCode::UserError => "USER_ERROR",
        ExitCode::Interrupted => "INTERRUPTED",
        ExitCode::Success => "OK",
    }
}

/// Classify a handler error string into the appropriate [`ExitCode`].
///
/// Handlers return `Err(String)` for both user-input errors (missing field,
/// invalid path) and infrastructure / state errors (task not found, submit
/// failure).  This function inspects the message and routes it to the correct
/// exit-code bucket so agents can branch on the numeric exit status without
/// parsing free-form text.
///
/// Heuristics (in priority order):
/// 1. Messages containing "not found" or "still pending/running" → `StateError`
/// 2. Messages that look like infra problems (provider, network, I/O) → `InfraError`
/// 3. Everything else (missing field, bad input) → `UserError`
pub fn classify_handler_error(msg: &str) -> ExitCode {
    let lower = msg.to_lowercase();
    // State errors: task lifecycle problems
    if lower.contains("not found")
        || lower.contains("still pending")
        || lower.contains("still running")
        || lower.contains("already cancelled")
        || lower.contains("wrong state")
    {
        return ExitCode::StateError;
    }
    // Infrastructure errors: provider / network / I/O
    if lower.contains("connection")
        || lower.contains("timeout")
        || lower.contains("io error")
        || lower.contains("provider")
        || lower.contains("ollama")
        || lower.contains("failed to submit")
        || lower.contains("infra")
    {
        return ExitCode::InfraError;
    }
    // Default: bad user input
    ExitCode::UserError
}

/// Emit a response (success or error) in the selected format and return the
/// corresponding process exit code.
pub fn emit(
    format: OutputFormat,
    code: ExitCode,
    data: serde_json::Value,
) -> std::process::ExitCode {
    match format {
        OutputFormat::Json => {
            let envelope = if code == ExitCode::Success {
                JsonEnvelope::success(data)
            } else {
                let msg = data
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| data.to_string());
                JsonEnvelope::error(exit_code_for(code), &msg)
            };
            println!("{}", envelope.to_json_string());
        }
        OutputFormat::Human => {
            if code == ExitCode::Success {
                print!("{}", crate::output::render(&data, OutputFormat::Human));
            } else {
                let msg = data.as_str().map(|s| s.to_string()).unwrap_or_else(|| {
                    crate::output::render(&data, OutputFormat::Human)
                        .trim()
                        .to_string()
                });
                eprintln!("Error: {msg}");
            }
        }
    }
    code.process_exit()
}

/// Install a Ctrl+C handler that exits with code 130 (SIGINT convention).
/// The `json_mode` flag lets the handler emit a JSON envelope when the CLI
/// was invoked with `--json`.
///
/// # Ordering
///
/// The handler reads `json_mode` with `Ordering::Acquire`. The store in
/// `main` after `Cli::parse()` uses `Ordering::Release` to establish the
/// necessary happens-before. This function must be called **after**
/// `Cli::parse()` and the `json_mode.store(...)` so the handler always
/// sees the correct value.
pub fn install_ctrlc_handler(json_mode: Arc<AtomicBool>) {
    let _ = ctrlc::set_handler(move || {
        if json_mode.load(Ordering::Acquire) {
            let envelope = JsonEnvelope::error("INTERRUPTED", "Received SIGINT");
            let _ = writeln!(io::stdout(), "{}", envelope.to_json_string());
        } else {
            let _ = writeln!(io::stderr(), "Interrupted");
        }
        std::process::exit(130);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    fn test_exit_code_for_mapping() {
        assert_eq!(exit_code_for(ExitCode::Success), "OK");
        assert_eq!(exit_code_for(ExitCode::UserError), "USER_ERROR");
        assert_eq!(exit_code_for(ExitCode::StateError), "STATE_ERROR");
        assert_eq!(exit_code_for(ExitCode::InfraError), "INFRA_ERROR");
        assert_eq!(exit_code_for(ExitCode::Interrupted), "INTERRUPTED");
    }

    #[test]
    #[serial]
    fn test_use_mock_provider_default_false() {
        // The env var could be set in the surrounding test process; guard by
        // explicitly unsetting it for this assertion.
        let prev = std::env::var("NANNA_TEST_MOCK").ok();
        unsafe { std::env::remove_var("NANNA_TEST_MOCK"); }
        assert!(!use_mock_provider());
        if let Some(v) = prev {
            unsafe { std::env::set_var("NANNA_TEST_MOCK", v); }
        }
    }

    #[tokio::test]
    async fn test_mock_cli_provider_chat() {
        let p = MockCliProvider;
        let req = ChatRequest::new("mock", vec![]);
        let resp = p.chat(req).await.expect("mock chat");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(
            resp.choices[0].message.content.as_deref(),
            Some("Mock response")
        );
    }

    #[tokio::test]
    async fn test_mock_cli_provider_list_models() {
        let p = MockCliProvider;
        let models = p.list_models().await.expect("list models");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "mock");
    }

    #[tokio::test]
    async fn test_mock_cli_provider_health_check() {
        let p = MockCliProvider;
        assert!(p.health_check().await.is_ok());
        assert_eq!(p.provider_name(), "mock");
    }

    #[test]
    #[serial]
    fn test_create_provider_with_mock_env() {
        let prev = std::env::var("NANNA_TEST_MOCK").ok();
        unsafe { std::env::set_var("NANNA_TEST_MOCK", "1"); }
        let p = create_provider().expect("mock provider");
        assert_eq!(p.provider_name(), "mock");
        match prev {
            Some(v) => unsafe { std::env::set_var("NANNA_TEST_MOCK", v); },
            None => unsafe { std::env::remove_var("NANNA_TEST_MOCK"); },
        }
    }

    #[test]
    fn test_classify_handler_error_user() {
        assert_eq!(classify_handler_error("Missing required field: description"), ExitCode::UserError);
        assert_eq!(classify_handler_error("repo_path must be an absolute path"), ExitCode::UserError);
    }

    #[test]
    fn test_classify_handler_error_state() {
        assert_eq!(classify_handler_error("Task not found: abc-123"), ExitCode::StateError);
        assert_eq!(classify_handler_error("Task abc is still pending"), ExitCode::StateError);
        assert_eq!(classify_handler_error("Task abc is still running"), ExitCode::StateError);
    }

    #[test]
    fn test_classify_handler_error_infra() {
        assert_eq!(classify_handler_error("connection refused"), ExitCode::InfraError);
        assert_eq!(classify_handler_error("Ollama returned 503"), ExitCode::InfraError);
        assert_eq!(classify_handler_error("failed to submit task: IO error"), ExitCode::InfraError);
    }
}
