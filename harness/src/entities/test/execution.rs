//! Test and lint execution abstractions.
//!
//! Provides a pluggable [`TestExecutor`] trait along with a concrete
//! [`CargoTestExecutor`] that shells out to `cargo`. Execution and parsing are
//! intentionally split: this module is responsible for spawning processes and
//! collecting their stdout/stderr, while the pure-function parsers in
//! [`super::parse`] turn that output into entity values.

use super::parse::{parse_cargo_test_messages, parse_clippy_messages};
use super::types::{LintResultEntity, TestError, TestRunEntity};
use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Instant;
use tokio::process::Command;

/// Trait for running tests and lints and returning domain entities.
#[async_trait]
pub trait TestExecutor: Send + Sync {
    /// Execute the test suite and return a populated [`TestRunEntity`].
    async fn run_tests(&self) -> Result<TestRunEntity, TestError>;

    /// Execute the lint suite and return a populated [`LintResultEntity`].
    async fn run_lints(&self) -> Result<LintResultEntity, TestError>;
}

/// `cargo`-backed [`TestExecutor`] that invokes `cargo test` and
/// `cargo clippy` with `--message-format=json` and parses the output.
pub struct CargoTestExecutor {
    /// Workspace root passed to `cargo -C` / `current_dir`.
    pub workspace_root: PathBuf,
}

impl CargoTestExecutor {
    /// Create a new executor rooted at the given workspace path.
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }

    async fn run_cargo(&self, args: &[&str]) -> Result<Vec<u8>, TestError> {
        let output = Command::new("cargo")
            .args(args)
            .current_dir(&self.workspace_root)
            .output()
            .await?;

        if !output.status.success() {
            return Err(TestError::CommandFailed {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        Ok(output.stdout)
    }
}

#[async_trait]
impl TestExecutor for CargoTestExecutor {
    async fn run_tests(&self) -> Result<TestRunEntity, TestError> {
        let started = Instant::now();
        let stdout = self
            .run_cargo(&[
                "test",
                "--",
                "-Z",
                "unstable-options",
                "--format=json",
                "--report-time",
            ])
            .await?;
        let text = String::from_utf8(stdout)
            .map_err(|e| TestError::Parse(format!("invalid utf-8 in cargo stdout: {e}")))?;

        let results = parse_cargo_test_messages(&text)?;

        let mut run = TestRunEntity::new("cargo".to_string());
        run.results = results;
        run.duration = started.elapsed();
        Ok(run)
    }

    async fn run_lints(&self) -> Result<LintResultEntity, TestError> {
        let stdout = self.run_cargo(&["clippy", "--message-format=json"]).await?;
        let text = String::from_utf8(stdout)
            .map_err(|e| TestError::Parse(format!("invalid utf-8 in cargo stdout: {e}")))?;

        let results = parse_clippy_messages(&text)?;

        let mut entity = LintResultEntity::new();
        entity.results = results;
        Ok(entity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cargo_test_executor_new() {
        let exec = CargoTestExecutor::new(PathBuf::from("/tmp/nonexistent-workspace-abc"));
        assert_eq!(
            exec.workspace_root,
            PathBuf::from("/tmp/nonexistent-workspace-abc")
        );
    }

    #[tokio::test]
    async fn test_cargo_test_executor_run_tests_missing_workspace() {
        let exec = CargoTestExecutor::new(PathBuf::from(
            "/tmp/definitely-does-not-exist-nanna-issue-24",
        ));
        let err = exec.run_tests().await.expect_err("should fail");
        // Either the CWD doesn't exist (Io) or cargo returns non-zero (CommandFailed).
        assert!(
            matches!(err, TestError::Io(_) | TestError::CommandFailed { .. }),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_cargo_test_executor_run_lints_missing_workspace() {
        let exec = CargoTestExecutor::new(PathBuf::from(
            "/tmp/definitely-does-not-exist-nanna-issue-24-lints",
        ));
        let err = exec.run_lints().await.expect_err("should fail");
        assert!(
            matches!(err, TestError::Io(_) | TestError::CommandFailed { .. }),
            "unexpected error: {err:?}"
        );
    }
}
