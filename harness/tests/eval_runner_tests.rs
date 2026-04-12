// NOTE: All tests in this file require a running Ollama instance and are
// therefore marked #[ignore] so they do not run in CI.
//
// run_eval now always constructs an OllamaProvider, so the tool-calling
// loop is always active. There is no entity-based fallback path.

use harness::eval::runner::{EvalRunnerConfig, run_eval};
use std::path::PathBuf;

#[tokio::test]
#[ignore]
async fn test_run_eval_hello_world() {
    let config = EvalRunnerConfig::default();
    let case_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/eval_cases/hello_world");
    let eval_case = harness::eval::case::EvalCase {
        description: "Write a hello-world program in Python.".to_string(),
        language: harness::eval::case::Language::Python,
        expected_symbols: vec![],
    };
    let result = run_eval(&eval_case, &case_dir, &config).await;
    assert!(result.is_ok(), "run_eval failed: {:?}", result);
    let run_result = result.unwrap();
    assert!(
        run_result.task_completed,
        "task not completed: {:?}",
        run_result.failure_message
    );
}
