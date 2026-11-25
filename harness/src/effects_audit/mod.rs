mod rules;

use crate::entities::context::types::ToolCallRecord;
use crate::tool_contract::{BuildState, FinishClaim};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDelta {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files: Vec<FileDelta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Info,
    Flag,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub severity: FindingSeverity,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    #[default]
    Pass,
    Flag,
    Block,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectsAudit {
    pub verdict: Verdict,
    pub findings: Vec<Finding>,
}

impl EffectsAudit {
    pub fn blocking(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.severity == FindingSeverity::Block)
    }

    pub fn is_blocked(&self) -> bool {
        self.verdict == Verdict::Block
    }

    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "verdict": self.verdict,
            "codes": self.findings.iter().map(|f| f.code.as_str()).collect::<Vec<_>>(),
            "blocking": self.blocking().map(|f| f.code.as_str()).collect::<Vec<_>>(),
        })
    }
}

pub struct EffectsInput<'a> {
    pub task_text: &'a str,
    pub claim: Option<FinishClaim>,
    pub ledger: &'a [ToolCallRecord],
    pub diff: DiffSummary,
    pub baseline: BuildState,
    pub final_state: BuildState,
    pub write_globs: &'a [String],
    pub write_capable: bool,
}

pub fn audit_effects(input: &EffectsInput) -> EffectsAudit {
    let findings = rules::evaluate(input);
    let verdict = match findings.iter().map(|f| f.severity).max() {
        Some(FindingSeverity::Block) => Verdict::Block,
        Some(FindingSeverity::Flag) => Verdict::Flag,
        _ => Verdict::Pass,
    };
    EffectsAudit { verdict, findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_contract::{Diagnostic, ErrorCode, Severity, ToolOutcome};

    fn call(ok: bool) -> ToolCallRecord {
        ToolCallRecord {
            tool_name: "replace".to_string(),
            arguments: serde_json::json!({}),
            call_id: "c".to_string(),
            result: String::new(),
            effect: None,
            seq: 0,
            outcome: Some(if ok {
                ToolOutcome::success(serde_json::json!({}))
            } else {
                ToolOutcome::failure(ErrorCode::NoMatch, "no match")
            }),
        }
    }

    fn delta(path: &str, before: u64, after: u64) -> FileDelta {
        FileDelta {
            path: path.to_string(),
            added: 1,
            removed: 1,
            bytes_before: before,
            bytes_after: after,
        }
    }

    fn claim(summary: &str, files: &[&str]) -> Option<FinishClaim> {
        Some(FinishClaim {
            summary: summary.to_string(),
            files_changed: files.iter().map(|f| f.to_string()).collect(),
        })
    }

    fn failing() -> BuildState {
        BuildState::Fail {
            errors: vec![Diagnostic {
                file: "src/a.rs".to_string(),
                line: 1,
                col: 1,
                severity: Severity::Error,
                code: None,
                message: "boom".to_string(),
            }],
        }
    }

    fn base<'a>(ledger: &'a [ToolCallRecord], globs: &'a [String]) -> EffectsInput<'a> {
        EffectsInput {
            task_text: "fix it",
            claim: None,
            ledger,
            diff: DiffSummary::default(),
            baseline: BuildState::Pass,
            final_state: BuildState::Pass,
            write_globs: globs,
            write_capable: true,
        }
    }

    fn codes(audit: &EffectsAudit) -> Vec<&str> {
        audit.findings.iter().map(|f| f.code.as_str()).collect()
    }

    fn one_edit() -> DiffSummary {
        DiffSummary {
            files: vec![delta("src/a.rs", 100, 101)],
        }
    }

    #[test]
    fn a_clean_two_file_one_line_edit_passes() {
        let globs = vec!["src/**".to_string()];
        let ledger = vec![call(true), call(true), call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("fixed two typos", &["src/a.rs", "src/b.rs"]);
        input.diff = DiffSummary {
            files: vec![delta("src/a.rs", 1000, 1002), delta("src/b.rs", 500, 501)],
        };
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Pass, "{:?}", audit.findings);
        assert!(audit.findings.is_empty());
    }

    #[test]
    fn a_plan_only_summary_with_zero_changes_is_blocked_as_claimed_unverified() {
        let globs = vec!["src/**".to_string()];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim(
            "Plan: open src/main.rs, add `#![unsafe]` at the top, then rebuild.",
            &[],
        );
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert!(codes(&audit).contains(&"claimed_unverified"));
    }

    #[test]
    fn claimed_files_with_an_empty_diff_are_blocked() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited it", &["src/a.rs"]);
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert!(codes(&audit).contains(&"claimed_unverified"));
    }

    #[test]
    fn a_write_capable_run_with_no_claim_and_no_diff_is_blocked_as_no_changes() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let input = base(&ledger, &globs);
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert_eq!(codes(&audit), vec!["no_changes"]);
    }

    #[test]
    fn a_read_only_run_with_no_diff_passes() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.write_capable = false;
        assert_eq!(audit_effects(&input).verdict, Verdict::Pass);
    }

    #[test]
    fn an_unclaimed_change_is_flagged() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited a", &["src/a.rs"]);
        input.diff = DiffSummary {
            files: vec![delta("src/a.rs", 100, 101), delta("./src/b.rs", 100, 101)],
        };
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Flag);
        assert_eq!(codes(&audit), vec!["unclaimed_change"]);
    }

    #[test]
    fn a_claimed_file_missing_from_the_diff_is_flagged() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited a and b", &["src/a.rs", "src/b.rs"]);
        input.diff = one_edit();
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Flag);
        assert_eq!(codes(&audit), vec!["claimed_file_missing"]);
    }

    #[test]
    fn a_write_outside_the_globs_is_blocked() {
        let globs = vec!["src/**".to_string()];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited", &["src/a.rs", "Cargo.toml"]);
        input.diff = DiffSummary {
            files: vec![delta("src/a.rs", 100, 101), delta("Cargo.toml", 100, 101)],
        };
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert_eq!(codes(&audit), vec!["out_of_scope_write"]);
        assert!(audit.findings[0].detail.contains("Cargo.toml"));
    }

    #[test]
    fn replacing_a_473_line_file_with_27_bytes_is_a_destructive_rewrite() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("rewrote", &["src/lib.rs"]);
        input.diff = DiffSummary {
            files: vec![FileDelta {
                path: "src/lib.rs".to_string(),
                added: 1,
                removed: 473,
                bytes_before: 14_000,
                bytes_after: 27,
            }],
        };
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert_eq!(codes(&audit), vec!["destructive_rewrite"]);
    }

    #[test]
    fn a_shrink_of_exactly_eighty_percent_is_not_destructive() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("trimmed", &["src/lib.rs"]);
        input.diff = DiffSummary {
            files: vec![delta("src/lib.rs", 1000, 200)],
        };
        assert_eq!(audit_effects(&input).verdict, Verdict::Pass);
    }

    #[test]
    fn a_build_that_goes_from_pass_to_fail_is_blocked() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited", &["src/a.rs"]);
        input.diff = one_edit();
        input.final_state = failing();
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert_eq!(codes(&audit), vec!["build_regressed"]);
    }

    #[test]
    fn a_build_that_stays_broken_is_flagged_not_blocked() {
        let globs = vec![];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited", &["src/a.rs"]);
        input.diff = one_edit();
        input.baseline = failing();
        input.final_state = failing();
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Flag);
        assert_eq!(codes(&audit), vec!["build_unfixed"]);
    }

    #[test]
    fn more_than_half_failed_calls_is_flagged_from_four_calls() {
        let globs = vec![];
        let ledger = vec![call(false), call(false), call(false), call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited", &["src/a.rs"]);
        input.diff = one_edit();
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Flag);
        assert_eq!(codes(&audit), vec!["high_failure_ratio"]);
    }

    #[test]
    fn failure_ratio_needs_four_calls_and_more_than_half() {
        let globs = vec![];
        let few = vec![call(false), call(false), call(false)];
        let mut input = base(&few, &globs);
        input.claim = claim("edited", &["src/a.rs"]);
        input.diff = one_edit();
        assert_eq!(audit_effects(&input).verdict, Verdict::Pass);
        let half = vec![call(false), call(false), call(true), call(true)];
        let mut input = base(&half, &globs);
        input.claim = claim("edited", &["src/a.rs"]);
        input.diff = one_edit();
        assert_eq!(audit_effects(&input).verdict, Verdict::Pass);
    }

    #[test]
    fn already_satisfied_passes_only_with_a_clean_ledger_and_empty_diff() {
        let globs = vec![];
        let ledger = vec![call(true), call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim(
            "The requirement is already satisfied; nothing to change.",
            &[],
        );
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Pass);
        assert_eq!(codes(&audit), vec!["already_satisfied"]);

        let dirty = vec![call(true), call(false)];
        let mut input = base(&dirty, &globs);
        input.claim = claim(
            "The requirement is already satisfied; nothing to change.",
            &[],
        );
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert!(!codes(&audit).contains(&"already_satisfied"));

        let mut input = base(&ledger, &globs);
        input.claim = claim("done", &[]);
        assert_eq!(audit_effects(&input).verdict, Verdict::Block);
    }

    #[test]
    fn the_verdict_is_the_worst_finding() {
        let globs = vec!["src/**".to_string()];
        let ledger = vec![call(true)];
        let mut input = base(&ledger, &globs);
        input.claim = claim("edited", &["src/a.rs"]);
        input.diff = DiffSummary {
            files: vec![delta("src/a.rs", 100, 101), delta("docs/x.md", 10, 11)],
        };
        let audit = audit_effects(&input);
        assert_eq!(audit.verdict, Verdict::Block);
        assert!(codes(&audit).contains(&"unclaimed_change"));
        assert!(codes(&audit).contains(&"out_of_scope_write"));
    }

    #[test]
    fn the_summary_lists_codes_and_blocking_codes() {
        let audit = EffectsAudit {
            verdict: Verdict::Block,
            findings: vec![
                Finding {
                    code: "unclaimed_change".into(),
                    severity: FindingSeverity::Flag,
                    detail: String::new(),
                },
                Finding {
                    code: "no_changes".into(),
                    severity: FindingSeverity::Block,
                    detail: String::new(),
                },
            ],
        };
        let summary = audit.summary();
        assert_eq!(summary["verdict"], "block");
        assert_eq!(summary["blocking"], serde_json::json!(["no_changes"]));
        assert_eq!(summary["codes"].as_array().unwrap().len(), 2);
    }
}
