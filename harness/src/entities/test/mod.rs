//! Testing & Analysis Entities
//!
//! This module implements test results and static analysis entities for
//! tracking code quality metrics and test outcomes.
//!
//! See issue #24 and ARCHITECTURE.md for details.

// Placeholder for test entity implementation
// Full implementation tracked in issue #24

pub mod correlation;
pub mod execution;
pub mod parse;
pub mod types;

// Explicit re-exports keep the public surface intentional. As more entity
// kinds land under issue #24 (coverage, audit, trend, vulnerabilities) this
// list should grow deliberately rather than via blanket globs, which risk
// name collisions (e.g. a future `ExecutionError` shadowing `TestError`).
pub use correlation::correlate_with_commit;
pub use execution::{CargoTestExecutor, TestExecutor};
pub use parse::{parse_cargo_test_messages, parse_clippy_messages};
pub use types::{
    LintLocation, LintResult, LintResultEntity, LintTool, Severity, TestEntity, TestError,
    TestResult, TestRunEntity, TestStatus,
};
