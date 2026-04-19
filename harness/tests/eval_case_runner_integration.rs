//! Integration test for `harness::eval::run_eval_case` against a live model.
//!
//! This runs `happy-path-001` (add a `greet` function) against whatever model
//! the caller selects via `NANNA_EVAL_MODEL` (default: `gemma4:e4b`). It is
//! `#[ignore]`-gated because it needs a container runtime and a reachable
//! Ollama server — that gating path runs in CI via `cargo nextest
//! --run-ignored ignored-only` on the `integration-container` matrix leg
//! (blocked by the probe bug tracked in issue #235, which is being fixed in
//! parallel).

use harness::agent::eval_case::EvalCase;
use harness::eval::{run_eval_case, EvalRunConfig};
use model::provider::ModelProvider;
use model::{OllamaConfig, OllamaProvider};
use std::path::PathBuf;
use std::sync::Arc;

fn eval_model() -> String {
    std::env::var("NANNA_EVAL_MODEL").unwrap_or_else(|_| "gemma4:e4b".to_string())
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("CARGO_MANIFEST_DIR has a parent")
        .to_path_buf()
}

#[tokio::test]
#[ignore]
async fn run_happy_path_001_against_live_model() {
    let case_dir = workspace_root().join("evals/cases/happy-path-001");
    let task_toml = case_dir.join("task.toml");
    let source_repo = case_dir.join("repo");

    assert!(
        task_toml.is_file(),
        "fixture task.toml missing at {:?}",
        task_toml
    );
    assert!(
        source_repo.is_dir(),
        "fixture repo missing at {:?}",
        source_repo
    );

    let case = EvalCase::from_toml_file(&task_toml).expect("failed to load happy-path-001");

    let workspace = tempfile::tempdir().expect("tempdir");

    let ollama_config = OllamaConfig::default();
    let provider = OllamaProvider::new(ollama_config).expect("failed to create OllamaProvider");
    let provider: Arc<dyn ModelProvider> = Arc::new(provider);

    let run_config = EvalRunConfig::new(eval_model()).with_verbose(true);

    let result = run_eval_case(&case, &source_repo, workspace.path(), &run_config, provider).await;

    assert!(
        result.passed,
        "eval case {} failed: {:?} (build_passed={:?}, files_changed={:?}, missing_symbols={:?})",
        result.case_id,
        result.failure_reason,
        result.build_passed,
        result.files_changed,
        result.missing_symbols
    );
}
