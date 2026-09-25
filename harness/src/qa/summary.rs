//! Totals of the QA a task ran, attached to its result so `get_result`
//! shows the evidence and where it was written.

use super::browser::BrowserReport;
use super::manifest::ManifestReport;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

/// Counts of QA runs, checks and steps plus the artefacts they produced.
///
/// ```
/// use harness::qa::summary::QaSummary;
///
/// let summary = QaSummary::default();
/// assert!(summary.is_empty());
/// assert_eq!(summary.to_json()["artifacts"], serde_json::json!([]));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QaSummary {
    pub endpoint_runs: usize,
    pub endpoint_checks_passed: usize,
    pub endpoint_checks_failed: usize,
    pub browser_runs: usize,
    pub browser_steps_passed: usize,
    pub browser_steps_failed: usize,
    pub console_errors: usize,
    /// Host paths of every report and screenshot, in the order written.
    pub artifacts: Vec<String>,
}

impl QaSummary {
    pub fn is_empty(&self) -> bool {
        self.endpoint_runs == 0 && self.browser_runs == 0
    }

    pub fn record_endpoints(&mut self, report: &ManifestReport, artifact: &Path) {
        self.endpoint_runs += 1;
        self.endpoint_checks_passed += report.passed;
        self.endpoint_checks_failed += report.failed;
        self.artifacts.push(artifact.display().to_string());
    }

    pub fn record_browser(&mut self, report: &BrowserReport, artifact: &Path) {
        self.browser_runs += 1;
        let passed = report.steps.iter().filter(|s| s.passed).count();
        self.browser_steps_passed += passed;
        self.browser_steps_failed += report.steps.len() - passed;
        self.console_errors += report.console_errors.len();
        self.artifacts.push(artifact.display().to_string());
        for shot in &report.screenshots {
            self.artifacts.push(shot.display().to_string());
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("QaSummary serialises")
    }
}

/// A [`QaSummary`] shared between the QA tools of a task and its result.
#[derive(Debug, Default)]
pub struct QaLedger {
    inner: Mutex<QaSummary>,
}

impl QaLedger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_endpoints(&self, report: &ManifestReport, artifact: &Path) {
        self.lock().record_endpoints(report, artifact);
    }

    pub fn record_browser(&self, report: &BrowserReport, artifact: &Path) {
        self.lock().record_browser(report, artifact);
    }

    /// A copy of the totals so far.
    pub fn snapshot(&self) -> QaSummary {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QaSummary> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qa::browser::{Step, StepResult};
    use crate::qa::cdp::ConsoleError;
    use crate::qa::manifest::{Check, CheckResult};
    use std::path::PathBuf;

    fn endpoint_report() -> ManifestReport {
        let check = Check::new("/");
        let mut results = Vec::new();
        for (status, passed) in [(200, true), (500, false), (200, true)] {
            results.push(CheckResult {
                path: check.path.clone(),
                url: "u".to_string(),
                expected: 200,
                status: Some(status),
                contains: None,
                passed,
                snippet: String::new(),
                error: None,
            });
        }
        ManifestReport {
            base_url: "http://app".to_string(),
            checks: results,
            passed: 2,
            failed: 1,
        }
    }

    fn browser_report() -> BrowserReport {
        BrowserReport {
            frontend_url: "http://app".to_string(),
            steps: vec![
                StepResult {
                    step: Step::Goto {
                        path: "/".to_string(),
                    },
                    passed: true,
                    detail: String::new(),
                },
                StepResult {
                    step: Step::Screenshot {
                        name: "s".to_string(),
                    },
                    passed: false,
                    detail: String::new(),
                },
            ],
            passed: false,
            screenshots: vec![PathBuf::from("/a/browser-1/s.png")],
            console_errors: vec![ConsoleError {
                source: "console".to_string(),
                text: "e".to_string(),
                url: None,
            }],
        }
    }

    #[test]
    fn ledger_accumulates_runs_and_artifacts() {
        let ledger = QaLedger::new();
        assert!(ledger.snapshot().is_empty());
        ledger.record_endpoints(&endpoint_report(), Path::new("/a/endpoints-1.json"));
        ledger.record_endpoints(&endpoint_report(), Path::new("/a/endpoints-2.json"));
        ledger.record_browser(&browser_report(), Path::new("/a/browser-1/report.json"));
        let summary = ledger.snapshot();
        assert!(!summary.is_empty());
        assert_eq!(summary.endpoint_runs, 2);
        assert_eq!(summary.endpoint_checks_passed, 4);
        assert_eq!(summary.endpoint_checks_failed, 2);
        assert_eq!(summary.browser_runs, 1);
        assert_eq!(summary.browser_steps_passed, 1);
        assert_eq!(summary.browser_steps_failed, 1);
        assert_eq!(summary.console_errors, 1);
        assert_eq!(
            summary.artifacts,
            vec![
                "/a/endpoints-1.json",
                "/a/endpoints-2.json",
                "/a/browser-1/report.json",
                "/a/browser-1/s.png"
            ]
        );
        let json = summary.to_json();
        assert_eq!(json["endpoint_runs"], 2);
        assert_eq!(json["artifacts"][3], "/a/browser-1/s.png");
        let back: QaSummary = serde_json::from_value(json).unwrap();
        assert_eq!(back, summary);
        assert!(format!("{ledger:?}").contains("endpoint_runs: 2"));
    }

    #[test]
    fn summary_is_empty_only_without_runs() {
        let mut summary = QaSummary::default();
        assert!(summary.is_empty());
        summary.record_browser(
            &BrowserReport {
                frontend_url: String::new(),
                steps: vec![],
                passed: true,
                screenshots: vec![],
                console_errors: vec![],
            },
            Path::new("/r.json"),
        );
        assert!(!summary.is_empty());
        assert_eq!(summary.browser_steps_passed, 0);
        assert_eq!(summary.artifacts, vec!["/r.json"]);
    }
}
