//! Integration tests for the eval runner.
//!
//! These tests require an Ollama instance to run the agent end-to-end.
//! Run with: `cargo test --test eval_runner_tests -- --ignored`

use harness::agent::eval_case::EvalCase;
use harness::eval::runner::{run_eval, EvalRunnerConfig, EvalRunnerError};
use std::path::PathBuf;

/// Locate the evals/cases directory relative to the workspace root.
fn cases_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("evals/cases")
}

#[tokio::test]
#[ignore] // requires Ollama instance
async fn test_run_eval_returns_result() {
    let cases_dir = cases_dir();
    let task_toml = cases_dir.join("happy-path-001/task.toml");
    let case = EvalCase::from_toml_file(&task_toml).unwrap();
    let case_dir = task_toml.parent().unwrap();

    let config = EvalRunnerConfig::default().with_max_iterations(10);
    let result = match run_eval(&case, case_dir, &config).await {
        Ok(r) => r,
        Err(EvalRunnerError::ModelProvider(msg)) => {
            eprintln!("test_run_eval_returns_result: model provider unavailable ({msg}); skipping");
            return;
        }
        Err(e) => panic!("run_eval failed unexpectedly: {e:?}"),
    };

    assert_eq!(result.case_id, "happy-path-001");
    assert!(result.execution_time.as_nanos() > 0);
}

#[tokio::test]
#[ignore] // requires Ollama instance
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

    // Give the agent many iterations but only 1 second timeout. With a real
    // Ollama provider wired into run_eval, the agent is expected to spend
    // at least a second in LLM calls for an open-ended prompt, which should
    // trip the 1s timeout. We assert specifically on the timeout path so the
    // test fails loudly if the timeout plumbing regresses — the previous
    // `result.is_ok() || Err(Timeout)` assertion was trivially satisfied
    // while a silent-fallback no-op agent existed.
    let config = EvalRunnerConfig::default().with_max_iterations(10000);

    // Use a temp dir as case_dir (no repo/ subdirectory)
    let tmp = tempfile::TempDir::new().unwrap();
    let result = run_eval(&case, tmp.path(), &config).await;

    match result {
        Err(EvalRunnerError::Timeout(_)) => {}
        Err(EvalRunnerError::ModelProvider(msg)) => {
            // Ollama reachability is a prerequisite of this #[ignore] test;
            // if provider init fails (e.g. daemon gone mid-suite), surface it
            // rather than panic so the failure mode is distinct from a real
            // timeout-plumbing regression.
            eprintln!(
                "test_run_eval_timeout: model provider unavailable ({msg}); \
                 skipping timeout assertion"
            );
        }
        other => panic!("expected Err(Timeout) (or ModelProvider unavailable), got {other:?}"),
    }
}

#[tokio::test]
#[ignore] // requires Ollama instance
async fn test_run_eval_isolation() {
    let cases_dir = cases_dir();
    let task_toml = cases_dir.join("happy-path-001/task.toml");
    let case = EvalCase::from_toml_file(&task_toml).unwrap();
    let case_dir = task_toml.parent().unwrap();
    let config = EvalRunnerConfig::default().with_max_iterations(5);

    let result1 = match run_eval(&case, case_dir, &config).await {
        Ok(r) => r,
        Err(EvalRunnerError::ModelProvider(msg)) => {
            eprintln!("test_run_eval_isolation: model provider unavailable ({msg}); skipping");
            return;
        }
        Err(e) => panic!("run_eval failed unexpectedly: {e:?}"),
    };
    let result2 = run_eval(&case, case_dir, &config)
        .await
        .expect("provider was reachable on first call; expected reachable here too");

    assert_eq!(result1.case_id, result2.case_id);
    assert!(result1.execution_time.as_nanos() > 0);
    assert!(result2.execution_time.as_nanos() > 0);
}

#[tokio::test]
#[ignore] // requires Ollama instance
async fn test_discover_and_run_all_cases() {
    let cases_dir = cases_dir();
    let cases = EvalCase::discover(&cases_dir).unwrap();
    assert!(
        cases.len() >= 3,
        "Expected at least 3 eval cases, found {}",
        cases.len()
    );

    let config = EvalRunnerConfig::default().with_max_iterations(5);

    for (eval_case, case_path) in &cases {
        match run_eval(eval_case, case_path, &config).await {
            Ok(result) => assert_eq!(result.case_id, eval_case.case.id),
            Err(EvalRunnerError::ModelProvider(msg)) => {
                eprintln!(
                    "test_discover_and_run_all_cases: model provider unavailable ({msg}); skipping"
                );
                return;
            }
            Err(e) => panic!(
                "run_eval failed unexpectedly for {}: {e:?}",
                eval_case.case.id
            ),
        }
    }
}
