//! Test & analysis entity types
//!
//! Defines all testing and static analysis entities for tracking test results,
//! lint outcomes, code coverage, security audits, and performance benchmarks
//! correlated with git state. See issue #24 and ARCHITECTURE.md for details.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Test Results
// ---------------------------------------------------------------------------

/// Status of a single test execution
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestStatus {
    /// Test passed
    Passed,
    /// Test failed with a reason
    Failed { reason: String },
    /// Test was skipped
    Skipped,
    /// Test timed out
    Timeout,
}

/// Category of test
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestCategory {
    /// Unit test
    Unit,
    /// Integration test
    Integration,
    /// End-to-end test
    EndToEnd,
    /// Property-based / fuzz test
    Property,
    /// Custom category
    Custom(String),
}

/// A single test result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    /// Fully qualified test name
    pub name: String,

    /// Outcome
    pub status: TestStatus,

    /// How long the test took
    #[serde(with = "duration_millis")]
    pub duration: Duration,

    /// Captured stdout/stderr output
    pub output: String,

    /// Source file containing the test
    pub file: PathBuf,

    /// Line number of the test function
    pub line: usize,

    /// Test category
    pub category: TestCategory,
}

/// Entity wrapping a collection of test results for a single run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResultEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Git commit hash this run was executed against
    pub commit_hash: String,

    /// Individual test outcomes
    pub results: Vec<TestResult>,

    /// Total wall-clock duration of the run
    #[serde(with = "duration_millis")]
    pub total_duration: Duration,

    /// Number of tests that passed
    pub passed: usize,

    /// Number of tests that failed
    pub failed: usize,

    /// Number of tests that were skipped
    pub skipped: usize,
}

impl TestResultEntity {
    /// Create a new test result entity for the given commit
    pub fn new(commit_hash: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
            commit_hash,
            results: Vec::new(),
            total_duration: Duration::ZERO,
            passed: 0,
            failed: 0,
            skipped: 0,
        }
    }

    /// Add a test result and update counters
    pub fn add_result(&mut self, result: TestResult) {
        match &result.status {
            TestStatus::Passed => self.passed += 1,
            TestStatus::Failed { .. } => self.failed += 1,
            TestStatus::Skipped => self.skipped += 1,
            TestStatus::Timeout => self.failed += 1,
        }
        self.results.push(result);
    }

    /// Whether every test passed
    pub fn all_passed(&self) -> bool {
        self.failed == 0
    }

    /// Total number of tests
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped
    }

    /// Return only the failed results
    pub fn failures(&self) -> Vec<&TestResult> {
        self.results
            .iter()
            .filter(|r| matches!(r.status, TestStatus::Failed { .. } | TestStatus::Timeout))
            .collect()
    }
}

#[async_trait]
impl Entity for TestResultEntity {
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
// Lint Results
// ---------------------------------------------------------------------------

/// Severity of a lint finding
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    /// Informational hint
    Hint,
    /// Warning
    Warning,
    /// Error
    Error,
}

/// The tool that produced a lint finding
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LintTool {
    /// Rust clippy
    Clippy,
    /// Rust formatter
    Rustfmt,
    /// A custom / third-party linter
    Custom(String),
}

/// Source location of a lint finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintLocation {
    /// File path
    pub file: PathBuf,
    /// Line number (1-based)
    pub line: usize,
    /// Column number (1-based)
    pub column: usize,
}

/// A single lint finding
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintFinding {
    /// Tool that reported this
    pub tool: LintTool,

    /// Source location
    pub location: LintLocation,

    /// Rule / lint identifier (e.g. `clippy::needless_return`)
    pub rule: String,

    /// Human-readable message
    pub message: String,

    /// Severity level
    pub severity: Severity,

    /// Machine-applicable suggested fix (if available)
    pub suggested_fix: Option<String>,
}

/// Entity wrapping lint results for a single run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LintResultEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Git commit hash this lint run was executed against
    pub commit_hash: String,

    /// Individual findings
    pub findings: Vec<LintFinding>,

    /// Number of errors
    pub error_count: usize,

    /// Number of warnings
    pub warning_count: usize,

    /// Number of hints
    pub hint_count: usize,
}

impl LintResultEntity {
    /// Create a new lint result entity for the given commit
    pub fn new(commit_hash: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
            commit_hash,
            findings: Vec::new(),
            error_count: 0,
            warning_count: 0,
            hint_count: 0,
        }
    }

    /// Add a lint finding and update counters
    pub fn add_finding(&mut self, finding: LintFinding) {
        match finding.severity {
            Severity::Error => self.error_count += 1,
            Severity::Warning => self.warning_count += 1,
            Severity::Hint => self.hint_count += 1,
        }
        self.findings.push(finding);
    }

    /// Whether there are any errors
    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    /// Total number of findings
    pub fn total(&self) -> usize {
        self.error_count + self.warning_count + self.hint_count
    }

    /// Return findings for a specific file
    pub fn findings_for_file(&self, path: &PathBuf) -> Vec<&LintFinding> {
        self.findings
            .iter()
            .filter(|f| &f.location.file == path)
            .collect()
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
// Code Coverage
// ---------------------------------------------------------------------------

/// Per-file coverage data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileCoverage {
    /// File path
    pub file: PathBuf,

    /// Total lines in the file
    pub total_lines: usize,

    /// Lines covered by tests
    pub covered_lines: usize,

    /// Branch coverage percentage (0.0 – 100.0)
    pub branch_coverage: Option<f64>,

    /// Function coverage percentage (0.0 – 100.0)
    pub function_coverage: Option<f64>,
}

impl FileCoverage {
    /// Line coverage as a percentage
    pub fn line_coverage_pct(&self) -> f64 {
        if self.total_lines == 0 {
            return 0.0;
        }
        (self.covered_lines as f64 / self.total_lines as f64) * 100.0
    }
}

/// Entity wrapping code coverage results
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Git commit hash
    pub commit_hash: String,

    /// Overall total lines
    pub total_lines: usize,

    /// Overall covered lines
    pub covered_lines: usize,

    /// Overall line-coverage percentage
    pub line_coverage_pct: f64,

    /// Overall branch-coverage percentage (if available)
    pub branch_coverage_pct: Option<f64>,

    /// Overall function-coverage percentage (if available)
    pub function_coverage_pct: Option<f64>,

    /// Per-file breakdown
    pub file_coverage: HashMap<PathBuf, FileCoverage>,
}

impl CoverageEntity {
    /// Create a new coverage entity for the given commit
    pub fn new(commit_hash: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
            commit_hash,
            total_lines: 0,
            covered_lines: 0,
            line_coverage_pct: 0.0,
            branch_coverage_pct: None,
            function_coverage_pct: None,
            file_coverage: HashMap::new(),
        }
    }

    /// Add per-file coverage and recompute aggregates
    pub fn add_file_coverage(&mut self, fc: FileCoverage) {
        self.total_lines += fc.total_lines;
        self.covered_lines += fc.covered_lines;
        self.recompute_pct();
        self.file_coverage.insert(fc.file.clone(), fc);
    }

    /// Recompute the aggregate line-coverage percentage
    fn recompute_pct(&mut self) {
        self.line_coverage_pct = if self.total_lines == 0 {
            0.0
        } else {
            (self.covered_lines as f64 / self.total_lines as f64) * 100.0
        };
    }
}

#[async_trait]
impl Entity for CoverageEntity {
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
// Security Audit
// ---------------------------------------------------------------------------

/// Severity of a vulnerability
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum VulnerabilitySeverity {
    /// Low severity
    Low,
    /// Medium severity
    Medium,
    /// High severity
    High,
    /// Critical severity
    Critical,
}

/// A single dependency vulnerability
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vulnerability {
    /// Affected package name
    pub package: String,

    /// Affected version
    pub version: String,

    /// Advisory / CVE identifier
    pub vulnerability_id: String,

    /// Severity
    pub severity: VulnerabilitySeverity,

    /// Version that fixes the issue (if known)
    pub fixed_in: Option<String>,

    /// Human-readable description
    pub description: String,
}

/// A license compliance issue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseIssue {
    /// Package name
    pub package: String,

    /// Detected license identifier
    pub license: String,

    /// Why this license is problematic
    pub reason: String,
}

/// Entity wrapping a security audit run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityAuditEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Git commit hash
    pub commit_hash: String,

    /// Discovered vulnerabilities
    pub vulnerabilities: Vec<Vulnerability>,

    /// License compliance issues
    pub license_issues: Vec<LicenseIssue>,

    /// Number of critical vulnerabilities
    pub critical_count: usize,

    /// Number of high-severity vulnerabilities
    pub high_count: usize,
}

impl SecurityAuditEntity {
    /// Create a new security audit entity
    pub fn new(commit_hash: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
            commit_hash,
            vulnerabilities: Vec::new(),
            license_issues: Vec::new(),
            critical_count: 0,
            high_count: 0,
        }
    }

    /// Add a vulnerability and update counters
    pub fn add_vulnerability(&mut self, vuln: Vulnerability) {
        match vuln.severity {
            VulnerabilitySeverity::Critical => self.critical_count += 1,
            VulnerabilitySeverity::High => self.high_count += 1,
            _ => {}
        }
        self.vulnerabilities.push(vuln);
    }

    /// Add a license issue
    pub fn add_license_issue(&mut self, issue: LicenseIssue) {
        self.license_issues.push(issue);
    }

    /// Whether there are any critical vulnerabilities
    pub fn has_critical(&self) -> bool {
        self.critical_count > 0
    }
}

#[async_trait]
impl Entity for SecurityAuditEntity {
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
// Performance Benchmarks
// ---------------------------------------------------------------------------

/// A single benchmark result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    /// Benchmark name
    pub name: String,

    /// Measured value (e.g. nanoseconds per iteration)
    pub value: f64,

    /// Unit of the measured value
    pub unit: String,

    /// Standard deviation (if available)
    pub stddev: Option<f64>,

    /// Number of iterations
    pub iterations: u64,
}

/// Entity wrapping benchmark results for a single run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Git commit hash
    pub commit_hash: String,

    /// Individual benchmark results
    pub results: Vec<BenchmarkResult>,
}

impl BenchmarkEntity {
    /// Create a new benchmark entity
    pub fn new(commit_hash: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
            commit_hash,
            results: Vec::new(),
        }
    }

    /// Add a benchmark result
    pub fn add_result(&mut self, result: BenchmarkResult) {
        self.results.push(result);
    }

    /// Look up a benchmark by name
    pub fn find(&self, name: &str) -> Option<&BenchmarkResult> {
        self.results.iter().find(|r| r.name == name)
    }
}

#[async_trait]
impl Entity for BenchmarkEntity {
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
// Commit Test Results (aggregate for a commit)
// ---------------------------------------------------------------------------

/// Aggregate of all test / analysis results for a single commit
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitTestResults {
    /// Git commit hash
    pub commit_hash: String,

    /// Test results (if run)
    pub tests: Option<TestResultEntity>,

    /// Lint results (if run)
    pub lints: Option<LintResultEntity>,

    /// Coverage results (if run)
    pub coverage: Option<CoverageEntity>,

    /// Security audit results (if run)
    pub audit: Option<SecurityAuditEntity>,

    /// Benchmark results (if run)
    pub benchmarks: Option<BenchmarkEntity>,

    /// Timestamp of the aggregation
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl CommitTestResults {
    /// Create a new aggregate for the given commit
    pub fn new(commit_hash: String) -> Self {
        Self {
            commit_hash,
            tests: None,
            lints: None,
            coverage: None,
            audit: None,
            benchmarks: None,
            timestamp: chrono::Utc::now(),
        }
    }

    /// Whether all recorded checks passed (no failures, no errors, no critical vulns)
    pub fn all_green(&self) -> bool {
        let tests_ok = self.tests.as_ref().is_none_or(|t| t.all_passed());
        let lints_ok = self.lints.as_ref().is_none_or(|l| !l.has_errors());
        let audit_ok = self.audit.as_ref().is_none_or(|a| !a.has_critical());
        tests_ok && lints_ok && audit_ok
    }
}

// ---------------------------------------------------------------------------
// Duration serde helper (serialize as milliseconds)
// ---------------------------------------------------------------------------

mod duration_millis {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    #[derive(Serialize, Deserialize)]
    struct Millis(u64);

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        Millis(d.as_millis() as u64).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let Millis(ms) = Millis::deserialize(d)?;
        Ok(Duration::from_millis(ms))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- TestResultEntity -------------------------------------------------

    #[test]
    fn test_result_entity_creation() {
        let entity = TestResultEntity::new("abc123".to_string());
        assert_eq!(entity.commit_hash, "abc123");
        assert_eq!(entity.passed, 0);
        assert_eq!(entity.failed, 0);
        assert_eq!(entity.skipped, 0);
        assert!(entity.results.is_empty());
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn test_result_entity_add_results() {
        let mut entity = TestResultEntity::new("abc123".to_string());

        entity.add_result(TestResult {
            name: "test_foo".to_string(),
            status: TestStatus::Passed,
            duration: Duration::from_millis(10),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 42,
            category: TestCategory::Unit,
        });

        entity.add_result(TestResult {
            name: "test_bar".to_string(),
            status: TestStatus::Failed {
                reason: "assertion failed".to_string(),
            },
            duration: Duration::from_millis(5),
            output: "thread panicked".to_string(),
            file: PathBuf::from("src/lib.rs"),
            line: 55,
            category: TestCategory::Unit,
        });

        entity.add_result(TestResult {
            name: "test_baz".to_string(),
            status: TestStatus::Skipped,
            duration: Duration::ZERO,
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 70,
            category: TestCategory::Integration,
        });

        entity.add_result(TestResult {
            name: "test_timeout".to_string(),
            status: TestStatus::Timeout,
            duration: Duration::from_secs(30),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 80,
            category: TestCategory::EndToEnd,
        });

        assert_eq!(entity.passed, 1);
        assert_eq!(entity.failed, 2); // Failed + Timeout
        assert_eq!(entity.skipped, 1);
        assert_eq!(entity.total(), 4);
        assert!(!entity.all_passed());
        assert_eq!(entity.failures().len(), 2);
    }

    #[test]
    fn test_result_entity_all_passed() {
        let mut entity = TestResultEntity::new("abc123".to_string());
        entity.add_result(TestResult {
            name: "test_ok".to_string(),
            status: TestStatus::Passed,
            duration: Duration::from_millis(1),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            category: TestCategory::Unit,
        });
        assert!(entity.all_passed());
    }

    #[test]
    fn test_result_entity_serialization_roundtrip() {
        let mut entity = TestResultEntity::new("abc123".to_string());
        entity.add_result(TestResult {
            name: "test_ok".to_string(),
            status: TestStatus::Passed,
            duration: Duration::from_millis(42),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            category: TestCategory::Unit,
        });

        let json = entity.to_json().unwrap();
        let deserialized: TestResultEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.commit_hash, "abc123");
        assert_eq!(deserialized.passed, 1);
        assert_eq!(deserialized.results[0].name, "test_ok");
    }

    // -- LintResultEntity -------------------------------------------------

    #[test]
    fn test_lint_result_entity_creation() {
        let entity = LintResultEntity::new("def456".to_string());
        assert_eq!(entity.commit_hash, "def456");
        assert_eq!(entity.error_count, 0);
        assert_eq!(entity.warning_count, 0);
        assert_eq!(entity.hint_count, 0);
        assert!(!entity.has_errors());
    }

    #[test]
    fn test_lint_result_entity_add_findings() {
        let mut entity = LintResultEntity::new("def456".to_string());

        entity.add_finding(LintFinding {
            tool: LintTool::Clippy,
            location: LintLocation {
                file: PathBuf::from("src/main.rs"),
                line: 10,
                column: 5,
            },
            rule: "clippy::needless_return".to_string(),
            message: "unneeded return".to_string(),
            severity: Severity::Warning,
            suggested_fix: Some("remove return".to_string()),
        });

        entity.add_finding(LintFinding {
            tool: LintTool::Rustfmt,
            location: LintLocation {
                file: PathBuf::from("src/main.rs"),
                line: 20,
                column: 1,
            },
            rule: "formatting".to_string(),
            message: "incorrect formatting".to_string(),
            severity: Severity::Error,
            suggested_fix: None,
        });

        entity.add_finding(LintFinding {
            tool: LintTool::Custom("mycheck".to_string()),
            location: LintLocation {
                file: PathBuf::from("src/utils.rs"),
                line: 5,
                column: 1,
            },
            rule: "custom-rule".to_string(),
            message: "consider refactoring".to_string(),
            severity: Severity::Hint,
            suggested_fix: None,
        });

        assert_eq!(entity.error_count, 1);
        assert_eq!(entity.warning_count, 1);
        assert_eq!(entity.hint_count, 1);
        assert_eq!(entity.total(), 3);
        assert!(entity.has_errors());

        let main_findings = entity.findings_for_file(&PathBuf::from("src/main.rs"));
        assert_eq!(main_findings.len(), 2);
    }

    #[test]
    fn test_lint_result_entity_serialization_roundtrip() {
        let mut entity = LintResultEntity::new("def456".to_string());
        entity.add_finding(LintFinding {
            tool: LintTool::Clippy,
            location: LintLocation {
                file: PathBuf::from("src/lib.rs"),
                line: 1,
                column: 1,
            },
            rule: "clippy::test".to_string(),
            message: "test".to_string(),
            severity: Severity::Warning,
            suggested_fix: None,
        });

        let json = entity.to_json().unwrap();
        let deserialized: LintResultEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.commit_hash, "def456");
        assert_eq!(deserialized.warning_count, 1);
    }

    // -- CoverageEntity ---------------------------------------------------

    #[test]
    fn test_coverage_entity_creation() {
        let entity = CoverageEntity::new("cov123".to_string());
        assert_eq!(entity.commit_hash, "cov123");
        assert_eq!(entity.total_lines, 0);
        assert_eq!(entity.covered_lines, 0);
        assert_eq!(entity.line_coverage_pct, 0.0);
    }

    #[test]
    fn test_coverage_entity_add_file() {
        let mut entity = CoverageEntity::new("cov123".to_string());

        entity.add_file_coverage(FileCoverage {
            file: PathBuf::from("src/lib.rs"),
            total_lines: 100,
            covered_lines: 80,
            branch_coverage: Some(75.0),
            function_coverage: Some(90.0),
        });

        entity.add_file_coverage(FileCoverage {
            file: PathBuf::from("src/main.rs"),
            total_lines: 50,
            covered_lines: 50,
            branch_coverage: None,
            function_coverage: None,
        });

        assert_eq!(entity.total_lines, 150);
        assert_eq!(entity.covered_lines, 130);
        // 130/150 ≈ 86.67%
        assert!((entity.line_coverage_pct - 86.666).abs() < 0.1);
        assert_eq!(entity.file_coverage.len(), 2);
    }

    #[test]
    fn test_file_coverage_pct_zero_lines() {
        let fc = FileCoverage {
            file: PathBuf::from("empty.rs"),
            total_lines: 0,
            covered_lines: 0,
            branch_coverage: None,
            function_coverage: None,
        };
        assert_eq!(fc.line_coverage_pct(), 0.0);
    }

    #[test]
    fn test_coverage_entity_serialization_roundtrip() {
        let mut entity = CoverageEntity::new("cov123".to_string());
        entity.add_file_coverage(FileCoverage {
            file: PathBuf::from("src/lib.rs"),
            total_lines: 100,
            covered_lines: 80,
            branch_coverage: None,
            function_coverage: None,
        });

        let json = entity.to_json().unwrap();
        let deserialized: CoverageEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_lines, 100);
        assert_eq!(deserialized.covered_lines, 80);
    }

    // -- SecurityAuditEntity ----------------------------------------------

    #[test]
    fn test_security_audit_entity_creation() {
        let entity = SecurityAuditEntity::new("sec123".to_string());
        assert_eq!(entity.commit_hash, "sec123");
        assert!(entity.vulnerabilities.is_empty());
        assert!(entity.license_issues.is_empty());
        assert!(!entity.has_critical());
    }

    #[test]
    fn test_security_audit_add_vulnerability() {
        let mut entity = SecurityAuditEntity::new("sec123".to_string());

        entity.add_vulnerability(Vulnerability {
            package: "some-crate".to_string(),
            version: "0.1.0".to_string(),
            vulnerability_id: "RUSTSEC-2024-0001".to_string(),
            severity: VulnerabilitySeverity::Critical,
            fixed_in: Some("0.2.0".to_string()),
            description: "Remote code execution".to_string(),
        });

        entity.add_vulnerability(Vulnerability {
            package: "other-crate".to_string(),
            version: "1.0.0".to_string(),
            vulnerability_id: "RUSTSEC-2024-0002".to_string(),
            severity: VulnerabilitySeverity::High,
            fixed_in: None,
            description: "Denial of service".to_string(),
        });

        assert_eq!(entity.critical_count, 1);
        assert_eq!(entity.high_count, 1);
        assert!(entity.has_critical());
        assert_eq!(entity.vulnerabilities.len(), 2);
    }

    #[test]
    fn test_security_audit_add_license_issue() {
        let mut entity = SecurityAuditEntity::new("sec123".to_string());

        entity.add_license_issue(LicenseIssue {
            package: "gpl-crate".to_string(),
            license: "GPL-3.0".to_string(),
            reason: "incompatible with MIT".to_string(),
        });

        assert_eq!(entity.license_issues.len(), 1);
    }

    #[test]
    fn test_security_audit_serialization_roundtrip() {
        let mut entity = SecurityAuditEntity::new("sec123".to_string());
        entity.add_vulnerability(Vulnerability {
            package: "pkg".to_string(),
            version: "1.0.0".to_string(),
            vulnerability_id: "CVE-2024-0001".to_string(),
            severity: VulnerabilitySeverity::Medium,
            fixed_in: None,
            description: "test".to_string(),
        });

        let json = entity.to_json().unwrap();
        let deserialized: SecurityAuditEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.vulnerabilities.len(), 1);
    }

    // -- BenchmarkEntity --------------------------------------------------

    #[test]
    fn test_benchmark_entity_creation() {
        let entity = BenchmarkEntity::new("bench123".to_string());
        assert_eq!(entity.commit_hash, "bench123");
        assert!(entity.results.is_empty());
    }

    #[test]
    fn test_benchmark_entity_add_and_find() {
        let mut entity = BenchmarkEntity::new("bench123".to_string());

        entity.add_result(BenchmarkResult {
            name: "sort_1000".to_string(),
            value: 1234.5,
            unit: "ns/iter".to_string(),
            stddev: Some(56.7),
            iterations: 10000,
        });

        entity.add_result(BenchmarkResult {
            name: "parse_json".to_string(),
            value: 5678.9,
            unit: "ns/iter".to_string(),
            stddev: None,
            iterations: 5000,
        });

        assert_eq!(entity.results.len(), 2);
        let found = entity.find("sort_1000");
        assert!(found.is_some());
        assert_eq!(found.unwrap().value, 1234.5);
        assert!(entity.find("nonexistent").is_none());
    }

    #[test]
    fn test_benchmark_entity_serialization_roundtrip() {
        let mut entity = BenchmarkEntity::new("bench123".to_string());
        entity.add_result(BenchmarkResult {
            name: "test_bench".to_string(),
            value: 100.0,
            unit: "ns".to_string(),
            stddev: Some(5.0),
            iterations: 1000,
        });

        let json = entity.to_json().unwrap();
        let deserialized: BenchmarkEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.results[0].name, "test_bench");
    }

    // -- CommitTestResults ------------------------------------------------

    #[test]
    fn test_commit_test_results_all_green_empty() {
        let ctr = CommitTestResults::new("abc123".to_string());
        assert!(ctr.all_green());
    }

    #[test]
    fn test_commit_test_results_all_green_passing() {
        let mut ctr = CommitTestResults::new("abc123".to_string());

        let mut tests = TestResultEntity::new("abc123".to_string());
        tests.add_result(TestResult {
            name: "test_ok".to_string(),
            status: TestStatus::Passed,
            duration: Duration::from_millis(1),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            category: TestCategory::Unit,
        });
        ctr.tests = Some(tests);

        let lints = LintResultEntity::new("abc123".to_string());
        ctr.lints = Some(lints);

        let audit = SecurityAuditEntity::new("abc123".to_string());
        ctr.audit = Some(audit);

        assert!(ctr.all_green());
    }

    #[test]
    fn test_commit_test_results_not_green_failing_test() {
        let mut ctr = CommitTestResults::new("abc123".to_string());

        let mut tests = TestResultEntity::new("abc123".to_string());
        tests.add_result(TestResult {
            name: "test_fail".to_string(),
            status: TestStatus::Failed {
                reason: "oops".to_string(),
            },
            duration: Duration::from_millis(1),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            category: TestCategory::Unit,
        });
        ctr.tests = Some(tests);

        assert!(!ctr.all_green());
    }

    #[test]
    fn test_commit_test_results_not_green_lint_error() {
        let mut ctr = CommitTestResults::new("abc123".to_string());

        let mut lints = LintResultEntity::new("abc123".to_string());
        lints.add_finding(LintFinding {
            tool: LintTool::Clippy,
            location: LintLocation {
                file: PathBuf::from("src/lib.rs"),
                line: 1,
                column: 1,
            },
            rule: "test".to_string(),
            message: "error".to_string(),
            severity: Severity::Error,
            suggested_fix: None,
        });
        ctr.lints = Some(lints);

        assert!(!ctr.all_green());
    }

    #[test]
    fn test_commit_test_results_not_green_critical_vuln() {
        let mut ctr = CommitTestResults::new("abc123".to_string());

        let mut audit = SecurityAuditEntity::new("abc123".to_string());
        audit.add_vulnerability(Vulnerability {
            package: "bad".to_string(),
            version: "0.1.0".to_string(),
            vulnerability_id: "CVE-0000".to_string(),
            severity: VulnerabilitySeverity::Critical,
            fixed_in: None,
            description: "critical".to_string(),
        });
        ctr.audit = Some(audit);

        assert!(!ctr.all_green());
    }

    // -- Entity trait on all types -----------------------------------------

    #[test]
    fn test_all_entities_implement_entity_trait() {
        let test_entity = TestResultEntity::new("a".to_string());
        let lint_entity = LintResultEntity::new("b".to_string());
        let cov_entity = CoverageEntity::new("c".to_string());
        let sec_entity = SecurityAuditEntity::new("d".to_string());
        let bench_entity = BenchmarkEntity::new("e".to_string());

        assert!(test_entity.to_json().is_ok());
        assert!(lint_entity.to_json().is_ok());
        assert!(cov_entity.to_json().is_ok());
        assert!(sec_entity.to_json().is_ok());
        assert!(bench_entity.to_json().is_ok());

        assert_eq!(test_entity.entity_type(), EntityType::Test);
        assert_eq!(lint_entity.entity_type(), EntityType::Test);
        assert_eq!(cov_entity.entity_type(), EntityType::Test);
        assert_eq!(sec_entity.entity_type(), EntityType::Test);
        assert_eq!(bench_entity.entity_type(), EntityType::Test);
    }

    // -- Severity ordering ------------------------------------------------

    #[test]
    fn test_severity_ordering() {
        assert!(Severity::Hint < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
    }

    #[test]
    fn test_vulnerability_severity_ordering() {
        assert!(VulnerabilitySeverity::Low < VulnerabilitySeverity::Medium);
        assert!(VulnerabilitySeverity::Medium < VulnerabilitySeverity::High);
        assert!(VulnerabilitySeverity::High < VulnerabilitySeverity::Critical);
    }
}
