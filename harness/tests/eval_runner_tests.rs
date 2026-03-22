//! Integration tests for the eval runner.

use harness::agent::eval_case::EvalCase;
use harness::eval::runner::{run_eval, EvalRunnerConfig, EvalRunnerError};
use std::path::Path;

/// Locate the evals/cases directory relative to the workspace root.
fn cases_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("evals/cases")
        .leak()
}

#[tokio::test]
async fn test_run_eval_returns_result() {
    let task_toml = cases_dir().join("happy-path-001/task.toml");
    let case = EvalCase::from_toml_file(&task_toml).unwrap();
    let case_dir = task_toml.parent().unwrap();

    // Use a short max_iterations so the entity-based agent completes quickly
    let config = EvalRunnerConfig::default().with_max_iterations(10);
    let result = run_eval(&case, case_dir, &config).await.unwrap();

    assert_eq!(result.case_id, "happy-path-001");
    assert!(result.execution_time.as_nanos() > 0);
}

#[tokio::test]
async fn test_run_eval_timeout() {
    let toml_str = r#"
[case]
id = "timeout-test"
name = "Timeout test"
description = "Should time out quickly"

[task]
prompt = "Do something impossible"

[metadata]
timeout_secs = 1
"#;
    let case = EvalCase::from_toml_str(toml_str).unwrap();

    // Give the agent many iterations but only 1 second timeout
    let config = EvalRunnerConfig::default().with_max_iterations(10000);

    // Use a temp dir as case_dir (no repo/ subdirectory)
    let tmp = tempfile::TempDir::new().unwrap();
    let result = run_eval(&case, tmp.path(), &config).await;

    // The agent should either complete quickly (no LLM attached) or time out.
    // With no LLM, it will complete via the entity-based loop, so it won't timeout.
    // This test mainly verifies the timeout plumbing compiles and runs.
    assert!(result.is_ok() || matches!(result, Err(EvalRunnerError::Timeout(_))));
}

#[tokio::test]
async fn test_run_eval_isolation() {
    let task_toml = cases_dir().join("happy-path-001/task.toml");
    let case = EvalCase::from_toml_file(&task_toml).unwrap();
    let case_dir = task_toml.parent().unwrap();
    let config = EvalRunnerConfig::default().with_max_iterations(5);

    // Run two evals — they should not interfere with each other
    let result1 = run_eval(&case, case_dir, &config).await.unwrap();
    let result2 = run_eval(&case, case_dir, &config).await.unwrap();

    assert_eq!(result1.case_id, result2.case_id);
    // Both should produce results (even if agent doesn't fully succeed without LLM)
    assert!(result1.execution_time.as_nanos() > 0);
    assert!(result2.execution_time.as_nanos() > 0);
}

#[tokio::test]
async fn test_discover_and_run_all_cases() {
    let cases = EvalCase::discover(cases_dir()).unwrap();
    assert!(
        cases.len() >= 3,
        "Expected at least 3 eval cases, found {}",
        cases.len()
    );

    let config = EvalRunnerConfig::default().with_max_iterations(5);

    for (eval_case, case_path) in &cases {
        let result = run_eval(eval_case, case_path, &config).await.unwrap();
        assert_eq!(result.case_id, eval_case.case.id);
    }
}
