//! Test and lint execution abstractions.
//!
//! Provides a pluggable [`TestExecutor`] trait along with a concrete
//! [`CargoTestExecutor`] that shells out to `cargo`. Execution and parsing are
//! intentionally split: this module is responsible for spawning processes and
//! collecting their stdout/stderr, while the pure-function parsers in
//! [`super::parse`] turn that output into entity values.
//!
//! # Test runner: `cargo nextest` (stable)
//!
//! The first iteration of this module invoked
//! `cargo test -- -Z unstable-options --format=json`, which requires a
//! **nightly** toolchain and silently fails on stable. To keep the parser on
//! a stable-compatible path we now invoke
//! `cargo nextest run --message-format libtest-json`, whose output is
//! byte-compatible with libtest's JSON format — the parser API is unchanged.
//!
//! `cargo-nextest` must be installed on the executor host. Nanna-coder's CI
//! already provisions it (`taiki-e/install-action` on non-Nix runners; it is
//! part of the Nix dev-shell otherwise). When the binary is missing,
//! [`CargoTestExecutor::run_tests`] returns
//! [`TestError::NextestUnavailable`] with an installation hint rather than a
//! generic I/O error so callers can surface an actionable message.

use super::parse::{parse_cargo_test_messages, parse_clippy_messages};
use super::types::{LintResultEntity, TestError, TestRunEntity};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;

/// Probe for `cargo nextest` on `PATH` by invoking `cargo nextest --version`.
///
/// Returns `Ok(())` if the subcommand is available; otherwise
/// [`TestError::NextestUnavailable`] with an installation hint.
async fn ensure_nextest_available() -> Result<(), TestError> {
    let probe = Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .await;
    match probe {
        Ok(out) if out.status.success() => Ok(()),
        _ => Err(TestError::NextestUnavailable),
    }
}

/// Best-effort `git rev-parse HEAD` against `workspace_root`.
///
/// Returns `None` when the workspace is not a git repo or the call fails —
/// populating `commit_hash` is not load-bearing for correctness, so failures
/// are swallowed rather than surfaced to the caller.
fn try_read_head_sha(workspace_root: &Path) -> Option<String> {
    crate::entities::git::operations::read_head_commit(workspace_root)
        .ok()
        .map(|c| c.sha)
}

/// Build a [`TestRunEntity`] from the raw stdout bytes of a
/// `cargo nextest run --message-format libtest-json` invocation.
///
/// The output format is a superset of `cargo test --format=json` — every
/// event kind the parser cares about (`{"type":"suite"|"test", ...}`) is
/// emitted by both runners with the same field names.
///
/// Split out from [`CargoTestExecutor::run_tests`] so the byte-to-entity
/// transform can be exercised by unit tests without spawning `cargo`.
fn build_test_run_from_stdout(
    stdout: Vec<u8>,
    duration: Duration,
) -> Result<TestRunEntity, TestError> {
    let text = String::from_utf8(stdout)
        .map_err(|e| TestError::Parse(format!("invalid utf-8 in cargo stdout: {e}")))?;
    let results = parse_cargo_test_messages(&text)?;

    let mut run = TestRunEntity::new("nextest".to_string());
    run.results = results;
    run.duration = duration;
    Ok(run)
}

/// Build a [`LintResultEntity`] from the raw stdout bytes of a
/// `cargo clippy --message-format=json` invocation.
fn build_lint_result_from_stdout(stdout: Vec<u8>) -> Result<LintResultEntity, TestError> {
    let text = String::from_utf8(stdout)
        .map_err(|e| TestError::Parse(format!("invalid utf-8 in cargo stdout: {e}")))?;
    let results = parse_clippy_messages(&text)?;

    let mut entity = LintResultEntity::new();
    entity.results = results;
    Ok(entity)
}

/// Trait for running tests and lints and returning domain entities.
#[async_trait]
pub trait TestExecutor: Send + Sync {
    /// Execute the test suite and return a populated [`TestRunEntity`].
    async fn run_tests(&self) -> Result<TestRunEntity, TestError>;

    /// Execute the lint suite and return a populated [`LintResultEntity`].
    async fn run_lints(&self) -> Result<LintResultEntity, TestError>;
}

/// `cargo`-backed [`TestExecutor`] that invokes `cargo nextest run` (tests)
/// and `cargo clippy` (lints) with JSON output and parses the result.
///
/// `run_tests` requires `cargo-nextest` on `PATH`; see the module-level
/// documentation for the rationale and how CI provisions it.
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
        ensure_nextest_available().await?;

        let started = Instant::now();
        // `--message-format libtest-json` is stable as of cargo-nextest 0.9.64
        // (behind the `NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1` env-flag on older
        // versions) and emits the same shape as libtest's JSON output, so the
        // parser is unchanged.
        let stdout = self
            .run_cargo(&["nextest", "run", "--message-format", "libtest-json"])
            .await?;
        let mut run = build_test_run_from_stdout(stdout, started.elapsed())?;
        run.commit_hash = try_read_head_sha(&self.workspace_root);
        Ok(run)
    }

    async fn run_lints(&self) -> Result<LintResultEntity, TestError> {
        let stdout = self.run_cargo(&["clippy", "--message-format=json"]).await?;
        let mut entity = build_lint_result_from_stdout(stdout)?;
        entity.commit_hash = try_read_head_sha(&self.workspace_root);
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
        // Possible failure paths:
        //   - `cargo nextest` is not installed         -> NextestUnavailable
        //   - `cargo nextest` is installed but the CWD does not exist -> Io
        //   - cargo returns non-zero (e.g. workspace not found)       -> CommandFailed
        assert!(
            matches!(
                err,
                TestError::Io(_) | TestError::CommandFailed { .. } | TestError::NextestUnavailable
            ),
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

    // --------- helper coverage: stdout -> entity transforms ---------

    #[test]
    fn test_build_test_run_from_stdout_happy_path() {
        let stdout = concat!(
            r#"{"type":"suite","event":"started","test_count":2}"#,
            "\n",
            r#"{"type":"test","event":"ok","name":"a::b","exec_time":0.01}"#,
            "\n",
            r#"{"type":"test","event":"failed","name":"a::c","exec_time":0.02,"stdout":"boom"}"#,
            "\n",
            r#"{"type":"suite","event":"ok","passed":1,"failed":1}"#,
        )
        .as_bytes()
        .to_vec();

        let run = build_test_run_from_stdout(stdout, Duration::from_millis(42)).expect("build ok");
        assert_eq!(run.executor, "nextest");
        assert_eq!(run.duration, Duration::from_millis(42));
        assert_eq!(run.results.len(), 2);
        assert_eq!(run.passed(), 1);
        assert_eq!(run.failed(), 1);
    }

    #[test]
    fn test_build_test_run_from_stdout_empty() {
        let run = build_test_run_from_stdout(Vec::new(), Duration::from_secs(0))
            .expect("empty stdout still builds");
        assert!(run.results.is_empty());
    }

    #[test]
    fn test_build_test_run_from_stdout_invalid_utf8() {
        let stdout = vec![0xff, 0xfe, 0xfd];
        let err = build_test_run_from_stdout(stdout, Duration::from_secs(0))
            .expect_err("invalid utf-8 must fail");
        assert!(matches!(err, TestError::Parse(msg) if msg.contains("invalid utf-8")));
    }

    #[test]
    fn test_build_test_run_from_stdout_propagates_parse_error() {
        let stdout = b"not-json\n".to_vec();
        let err = build_test_run_from_stdout(stdout, Duration::from_secs(0))
            .expect_err("malformed json must fail");
        assert!(matches!(err, TestError::Parse(_)));
    }

    #[test]
    fn test_build_lint_result_from_stdout_happy_path() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","package_id":"x 0.1.0","target":{"name":"x"}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"message":"unused","level":"warning","code":{"code":"clippy::needless_return"},"spans":[{"file_name":"src/lib.rs","line_start":4,"column_start":2,"is_primary":true}]}}"#,
        )
        .as_bytes()
        .to_vec();

        let entity = build_lint_result_from_stdout(stdout).expect("build ok");
        assert_eq!(entity.results.len(), 1);
        assert_eq!(entity.results[0].rule, "clippy::needless_return");
    }

    #[test]
    fn test_build_lint_result_from_stdout_empty() {
        let entity = build_lint_result_from_stdout(Vec::new()).expect("empty stdout still builds");
        assert!(entity.results.is_empty());
        assert!(entity.worst_severity().is_none());
    }

    #[test]
    fn test_build_lint_result_from_stdout_invalid_utf8() {
        let stdout = vec![0xff, 0xfe];
        let err = build_lint_result_from_stdout(stdout).expect_err("invalid utf-8 must fail");
        assert!(matches!(err, TestError::Parse(msg) if msg.contains("invalid utf-8")));
    }

    #[test]
    fn test_build_lint_result_from_stdout_propagates_parse_error() {
        let stdout = b"not-json\n".to_vec();
        let err = build_lint_result_from_stdout(stdout).expect_err("malformed json must fail");
        assert!(matches!(err, TestError::Parse(_)));
    }
}
