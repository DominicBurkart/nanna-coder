//! Testing & Analysis Entities
//!
//! This module implements test results and static analysis entities for
//! tracking code quality metrics and test outcomes correlated with git state.
//!
//! # Entity types
//!
//! - [`TestResultEntity`] — unit / integration / E2E test outcomes
//! - [`LintResultEntity`] — clippy, rustfmt, and custom linter findings
//! - [`CoverageEntity`] — line, branch, and function coverage metrics
//! - [`SecurityAuditEntity`] — `cargo audit` / `cargo deny` results
//! - [`BenchmarkEntity`] — performance benchmark results
//! - [`CommitTestResults`] — aggregate of all results for a single commit
//!
//! # Storage
//!
//! [`TestCommitStore`] provides an in-memory store indexed by commit hash,
//! supporting trend analysis and regression detection.
//!
//! See issue #24 and ARCHITECTURE.md for details.

pub mod store;
pub mod types;

pub use store::*;
pub use types::*;
