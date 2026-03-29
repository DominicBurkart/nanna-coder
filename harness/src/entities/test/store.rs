//! Commit-correlated test result storage
//!
//! Provides an in-memory store that indexes test / analysis results by git
//! commit hash, enabling temporal queries such as trend analysis and
//! regression detection.

use super::types::*;
use std::collections::HashMap;

/// In-memory store for commit-correlated test results
#[derive(Debug, Default)]
pub struct TestCommitStore {
    results: HashMap<String, CommitTestResults>,
    /// Ordered list of commit hashes (insertion order)
    commit_order: Vec<String>,
}

/// Summary of trends across multiple commits
#[derive(Debug, Clone)]
pub struct TrendAnalysis {
    /// Number of commits analysed
    pub commit_count: usize,

    /// Test pass-rate per commit (commit_hash, pass_rate 0.0..1.0)
    pub pass_rates: Vec<(String, f64)>,

    /// Line-coverage per commit (commit_hash, coverage %)
    pub coverage_trend: Vec<(String, f64)>,

    /// Total failing tests in the most recent commit (if available)
    pub latest_failures: usize,

    /// Whether tests are currently all green
    pub currently_green: bool,
}

impl TestCommitStore {
    /// Create a new empty store
    pub fn new() -> Self {
        Self::default()
    }

    /// Store results for a commit, replacing any previous results for the same hash
    pub fn store_results(&mut self, results: CommitTestResults) {
        let hash = results.commit_hash.clone();
        if !self.results.contains_key(&hash) {
            self.commit_order.push(hash.clone());
        }
        self.results.insert(hash, results);
    }

    /// Look up results for a specific commit
    pub fn query_by_commit(&self, commit_hash: &str) -> Option<&CommitTestResults> {
        self.results.get(commit_hash)
    }

    /// Number of commits stored
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Whether the store is empty
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Analyse trends across the given commits (or all commits if slice is empty)
    pub fn query_trend(&self, commits: &[String]) -> TrendAnalysis {
        let ordered: Vec<&String> = if commits.is_empty() {
            self.commit_order.iter().collect()
        } else {
            commits.iter().collect()
        };

        let mut pass_rates = Vec::new();
        let mut coverage_trend = Vec::new();
        let mut latest_failures = 0;
        let mut currently_green = true;

        for hash in &ordered {
            if let Some(ctr) = self.results.get(hash.as_str()) {
                if let Some(ref tests) = ctr.tests {
                    let total = tests.total();
                    let rate = if total > 0 {
                        tests.passed as f64 / total as f64
                    } else {
                        1.0
                    };
                    pass_rates.push((hash.to_string(), rate));
                    latest_failures = tests.failed;
                }

                if let Some(ref cov) = ctr.coverage {
                    coverage_trend.push((hash.to_string(), cov.line_coverage_pct));
                }

                currently_green = ctr.all_green();
            }
        }

        TrendAnalysis {
            commit_count: ordered.len(),
            pass_rates,
            coverage_trend,
            latest_failures,
            currently_green,
        }
    }

    /// Return commit hashes that have test failures
    pub fn commits_with_failures(&self) -> Vec<&str> {
        self.commit_order
            .iter()
            .filter_map(|hash| {
                let ctr = self.results.get(hash)?;
                let tests = ctr.tests.as_ref()?;
                if tests.failed > 0 {
                    Some(hash.as_str())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Return commit hashes that have critical vulnerabilities
    pub fn commits_with_critical_vulns(&self) -> Vec<&str> {
        self.commit_order
            .iter()
            .filter_map(|hash| {
                let ctr = self.results.get(hash)?;
                let audit = ctr.audit.as_ref()?;
                if audit.has_critical() {
                    Some(hash.as_str())
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn make_passing_tests(commit: &str) -> CommitTestResults {
        let mut tests = TestResultEntity::new(commit.to_string());
        tests.add_result(TestResult {
            name: "test_ok".to_string(),
            status: TestStatus::Passed,
            duration: Duration::from_millis(5),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            category: TestCategory::Unit,
        });
        let mut ctr = CommitTestResults::new(commit.to_string());
        ctr.tests = Some(tests);
        ctr
    }

    fn make_failing_tests(commit: &str) -> CommitTestResults {
        let mut tests = TestResultEntity::new(commit.to_string());
        tests.add_result(TestResult {
            name: "test_fail".to_string(),
            status: TestStatus::Failed {
                reason: "oops".to_string(),
            },
            duration: Duration::from_millis(5),
            output: String::new(),
            file: PathBuf::from("src/lib.rs"),
            line: 1,
            category: TestCategory::Unit,
        });
        let mut ctr = CommitTestResults::new(commit.to_string());
        ctr.tests = Some(tests);
        ctr
    }

    #[test]
    fn test_store_and_query() {
        let mut store = TestCommitStore::new();
        assert!(store.is_empty());

        store.store_results(make_passing_tests("aaa"));
        assert_eq!(store.len(), 1);

        let result = store.query_by_commit("aaa");
        assert!(result.is_some());
        assert!(result.unwrap().all_green());

        assert!(store.query_by_commit("nonexistent").is_none());
    }

    #[test]
    fn test_store_replaces_existing() {
        let mut store = TestCommitStore::new();
        store.store_results(make_failing_tests("aaa"));
        assert!(!store.query_by_commit("aaa").unwrap().all_green());

        // Replace with passing
        store.store_results(make_passing_tests("aaa"));
        assert!(store.query_by_commit("aaa").unwrap().all_green());
        assert_eq!(store.len(), 1); // no duplicate
    }

    #[test]
    fn test_trend_analysis() {
        let mut store = TestCommitStore::new();
        store.store_results(make_passing_tests("commit1"));
        store.store_results(make_failing_tests("commit2"));
        store.store_results(make_passing_tests("commit3"));

        let trend = store.query_trend(&[]);
        assert_eq!(trend.commit_count, 3);
        assert_eq!(trend.pass_rates.len(), 3);
        assert_eq!(trend.latest_failures, 0);
        assert!(trend.currently_green);

        // Check that commit2 had 0% pass rate
        let commit2_rate = trend
            .pass_rates
            .iter()
            .find(|(h, _)| h == "commit2")
            .unwrap()
            .1;
        assert_eq!(commit2_rate, 0.0);
    }

    #[test]
    fn test_trend_analysis_specific_commits() {
        let mut store = TestCommitStore::new();
        store.store_results(make_passing_tests("a"));
        store.store_results(make_failing_tests("b"));
        store.store_results(make_passing_tests("c"));

        let trend = store.query_trend(&["a".to_string(), "c".to_string()]);
        assert_eq!(trend.commit_count, 2);
        assert_eq!(trend.pass_rates.len(), 2);
    }

    #[test]
    fn test_commits_with_failures() {
        let mut store = TestCommitStore::new();
        store.store_results(make_passing_tests("a"));
        store.store_results(make_failing_tests("b"));
        store.store_results(make_passing_tests("c"));

        let failures = store.commits_with_failures();
        assert_eq!(failures, vec!["b"]);
    }

    #[test]
    fn test_commits_with_critical_vulns() {
        let mut store = TestCommitStore::new();

        let mut ctr = CommitTestResults::new("safe".to_string());
        let audit = SecurityAuditEntity::new("safe".to_string());
        ctr.audit = Some(audit);
        store.store_results(ctr);

        let mut ctr2 = CommitTestResults::new("vuln".to_string());
        let mut audit2 = SecurityAuditEntity::new("vuln".to_string());
        audit2.add_vulnerability(Vulnerability {
            package: "bad".to_string(),
            version: "0.1.0".to_string(),
            vulnerability_id: "CVE-0000".to_string(),
            severity: VulnerabilitySeverity::Critical,
            fixed_in: None,
            description: "critical".to_string(),
        });
        ctr2.audit = Some(audit2);
        store.store_results(ctr2);

        let critical = store.commits_with_critical_vulns();
        assert_eq!(critical, vec!["vuln"]);
    }

    #[test]
    fn test_coverage_trend() {
        let mut store = TestCommitStore::new();

        for (hash, pct) in [("c1", 70.0), ("c2", 75.0), ("c3", 80.0)] {
            let mut ctr = CommitTestResults::new(hash.to_string());
            let mut cov = CoverageEntity::new(hash.to_string());
            cov.total_lines = 100;
            cov.covered_lines = pct as usize;
            cov.line_coverage_pct = pct;
            ctr.coverage = Some(cov);
            store.store_results(ctr);
        }

        let trend = store.query_trend(&[]);
        assert_eq!(trend.coverage_trend.len(), 3);
        assert_eq!(trend.coverage_trend[0].1, 70.0);
        assert_eq!(trend.coverage_trend[2].1, 80.0);
    }
}
