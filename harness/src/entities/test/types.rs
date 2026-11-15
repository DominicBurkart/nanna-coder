//! Test entity types
//!
//! This module defines the testing & analysis entities introduced for issue #24.
//! It preserves the original placeholder [`TestEntity`] (still referenced by
//! `harness::agent::eval`) and adds richer types that capture real test-run and
//! lint-result information.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

/// Test result entity (placeholder)
///
/// Retained for backwards compatibility with [`crate::agent::eval`] which
/// constructs this type directly. New code should prefer [`TestRunEntity`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #24
}

#[async_trait]
impl Entity for TestEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

impl TestEntity {
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
        }
    }
}

impl Default for TestEntity {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Test run entities (issue #24 first slice)
// ---------------------------------------------------------------------------

/// Status of an individual test case.
///
/// `Ord` is derived with variant declaration order chosen so that more severe
/// outcomes compare greater than less severe ones. The intent is:
///
/// `Skipped < Passed < Timeout < Failed`
///
/// This lets callers sort a `Vec<TestStatus>` and find the worst outcome via
/// `Iterator::max`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestStatus {
    /// Test was skipped (e.g. `#[ignore]`).
    Skipped,
    /// Test passed.
    Passed,
    /// Test timed out.
    Timeout,
    /// Test failed with the given reason.
    Failed { reason: String },
}

/// A single test result.
///
/// # Schema divergence from issue #24
///
/// Issue #24's proposed spec is
/// `{ name, status, duration: Duration, output: String, file: PathBuf, line: usize }`.
/// This first slice intentionally diverges:
///
/// - `output` is not modeled directly — failure output is carried inside
///   [`TestStatus::Failed::reason`], which covers the non-empty case. A
///   standalone `output` field for passing tests can be added in a follow-up
///   without breaking this schema.
/// - `file` and `line` are omitted because `cargo test --message-format=json`
///   (and `cargo nextest run --message-format libtest-json`) do **not** emit
///   source-location metadata on a per-test basis. Reconstructing them
///   requires a separate symbol-table lookup, which is out of scope for the
///   first slice.
/// - `duration_ms: Option<u64>` stands in for `Duration` because the
///   underlying `exec_time` field is reported in seconds as an `f64` and is
///   absent for skipped tests. Moving to `Option<Duration>` is tracked as a
///   follow-up.
///
/// Follow-ups that revisit this schema are tracked against issue #24.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestResult {
    /// Fully qualified test name, e.g. `crate::module::test_name`.
    pub name: String,
    /// Outcome of the test.
    pub status: TestStatus,
    /// Wall-clock duration in milliseconds. `None` when the runner did not
    /// report a duration (e.g. skipped tests). See the type-level doc comment
    /// for why this is `Option<u64>` rather than `Duration`.
    pub duration_ms: Option<u64>,
}

/// An entity representing a complete test run (e.g. `cargo test` invocation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRunEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Optional git commit the test run was executed against.
    pub commit_hash: Option<String>,

    /// Individual test results collected during the run.
    pub results: Vec<TestResult>,

    /// Total wall-clock duration of the run.
    pub duration: Duration,

    /// Executor identifier (e.g. `"cargo"`, `"nextest"`).
    pub executor: String,
}

impl TestRunEntity {
    /// Create a new test run entity.
    pub fn new(executor: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
            commit_hash: None,
            results: Vec::new(),
            duration: Duration::from_secs(0),
            executor,
        }
    }

    /// Number of tests with status [`TestStatus::Passed`].
    pub fn passed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.status, TestStatus::Passed))
            .count()
    }

    /// Number of tests with status [`TestStatus::Failed`] or
    /// [`TestStatus::Timeout`].
    pub fn failed(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.status, TestStatus::Failed { .. } | TestStatus::Timeout))
            .count()
    }
}

#[async_trait]
impl Entity for TestRunEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Lint / static analysis entities
// ---------------------------------------------------------------------------

/// Severity of a lint diagnostic.
///
/// Derive order is `Info < Warning < Error` so that `Vec<Severity>::iter().max()`
/// returns the worst severity encountered.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// Lint tooling identifier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "tool", content = "name", rename_all = "snake_case")]
pub enum LintTool {
    Clippy,
    Rustfmt,
    Custom(String),
}

/// Location of a lint diagnostic inside a source file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LintLocation {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

/// A single lint diagnostic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LintResult {
    pub tool: LintTool,
    pub location: LintLocation,
    /// Lint rule name (e.g. `clippy::needless_return`).
    pub rule: String,
    /// Human-readable diagnostic message.
    pub message: String,
    pub severity: Severity,
}

/// An entity representing a complete lint run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintResultEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Optional git commit the lint run was executed against.
    pub commit_hash: Option<String>,

    /// Diagnostics produced by the run.
    pub results: Vec<LintResult>,
}

impl LintResultEntity {
    /// Create a new lint result entity.
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Lint),
            commit_hash: None,
            results: Vec::new(),
        }
    }

    /// Return the worst severity in the run, if any diagnostics were produced.
    pub fn worst_severity(&self) -> Option<Severity> {
        self.results.iter().map(|r| r.severity).max()
    }
}

impl Default for LintResultEntity {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Entity for LintResultEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by test/lint entity helpers.
#[derive(Debug, Error)]
pub enum TestError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("command failed (exit {code:?}): {stderr}")]
    CommandFailed { code: Option<i32>, stderr: String },

    #[error(
        "cargo-nextest is not installed. Install it with \
         `cargo install cargo-nextest --locked` \
         or run inside the project's Nix dev-shell."
    )]
    NextestUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_entity_creation() {
        let e = TestEntity::new();
        assert_eq!(e.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn test_test_entity_default_equivalent_to_new() {
        let a = TestEntity::default();
        assert_eq!(a.metadata().entity_type, EntityType::Test);
    }
}

#[cfg(test)]
mod new_types_tests {
    use super::*;

    #[test]
    fn test_test_status_serde_roundtrip() {
        let statuses = vec![
            TestStatus::Passed,
            TestStatus::Skipped,
            TestStatus::Timeout,
            TestStatus::Failed {
                reason: "panicked at 'assertion failed'".to_string(),
            },
        ];
        for s in &statuses {
            let json = serde_json::to_string(s).expect("serialize");
            let back: TestStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(s, &back);
        }
    }

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        let worst = [Severity::Info, Severity::Error, Severity::Warning]
            .iter()
            .copied()
            .max()
            .unwrap();
        assert_eq!(worst, Severity::Error);
    }

    #[test]
    fn test_test_status_ordering() {
        // Skipped < Passed < Timeout < Failed
        assert!(TestStatus::Skipped < TestStatus::Passed);
        assert!(TestStatus::Passed < TestStatus::Timeout);
        assert!(TestStatus::Timeout < TestStatus::Failed { reason: "x".into() });
    }

    #[tokio::test]
    async fn test_test_run_entity_implements_entity() {
        let mut run = TestRunEntity::new("cargo".into());
        run.commit_hash = Some("deadbeef".into());
        run.results.push(TestResult {
            name: "foo::bar".into(),
            status: TestStatus::Passed,
            duration_ms: Some(10),
        });
        run.results.push(TestResult {
            name: "foo::baz".into(),
            status: TestStatus::Failed {
                reason: "boom".into(),
            },
            duration_ms: Some(20),
        });
        run.duration = Duration::from_millis(30);

        assert_eq!(run.entity_type(), EntityType::Test);
        assert_eq!(run.passed(), 1);
        assert_eq!(run.failed(), 1);

        let json = run.to_json().expect("to_json");
        assert!(json.contains("\"executor\":\"cargo\""));
        assert!(json.contains("\"commit_hash\":\"deadbeef\""));
    }

    #[tokio::test]
    async fn test_lint_result_entity_implements_entity() {
        let mut lint = LintResultEntity::new();
        lint.results.push(LintResult {
            tool: LintTool::Clippy,
            location: LintLocation {
                file: PathBuf::from("src/lib.rs"),
                line: 42,
                column: 7,
            },
            rule: "clippy::needless_return".into(),
            message: "unneeded return".into(),
            severity: Severity::Warning,
        });
        lint.results.push(LintResult {
            tool: LintTool::Custom("shellcheck".into()),
            location: LintLocation {
                file: PathBuf::from("scripts/foo.sh"),
                line: 1,
                column: 1,
            },
            rule: "SC2086".into(),
            message: "quote this".into(),
            severity: Severity::Error,
        });

        assert_eq!(lint.entity_type(), EntityType::Lint);
        assert_eq!(lint.worst_severity(), Some(Severity::Error));

        let json = lint.to_json().expect("to_json");
        assert!(json.contains("\"rule\":\"clippy::needless_return\""));
    }

    #[test]
    fn test_test_error_display() {
        let io_err = TestError::Io(io::Error::new(io::ErrorKind::NotFound, "missing"));
        let parse_err = TestError::Parse("bad json".into());
        let cmd_err = TestError::CommandFailed {
            code: Some(101),
            stderr: "boom".into(),
        };

        assert!(format!("{}", io_err).contains("io error"));
        assert!(format!("{}", parse_err).contains("parse error"));
        assert!(format!("{}", cmd_err).contains("command failed"));
        assert!(format!("{}", cmd_err).contains("101"));
    }
}
