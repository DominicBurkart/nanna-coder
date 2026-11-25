use super::{EffectsInput, Finding, FindingSeverity};
use crate::tool_contract::BuildState;
use glob::Pattern;
use std::collections::BTreeSet;

const MIN_CALLS_FOR_RATIO: usize = 4;
const MIN_BYTES_FOR_REWRITE: u64 = 100;

const SATISFIED_MARKERS: [&str; 5] = [
    "already satisfied",
    "already_satisfied",
    "already implemented",
    "no changes needed",
    "no change needed",
];

fn finding(code: &str, severity: FindingSeverity, detail: String) -> Finding {
    Finding {
        code: code.to_string(),
        severity,
        detail,
    }
}

pub(super) fn normalize(path: &str) -> String {
    let mut path = path.trim();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    path.to_string()
}

fn says_already_satisfied(summary: &str) -> bool {
    let lower = summary.to_lowercase();
    SATISFIED_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

fn failed_calls(input: &EffectsInput) -> usize {
    input
        .ledger
        .iter()
        .filter(|call| call.outcome.as_ref().is_some_and(|outcome| !outcome.ok))
        .count()
}

fn presence(input: &EffectsInput, out: &mut Vec<Finding>) {
    if !input.diff.files.is_empty() {
        return;
    }
    match &input.claim {
        Some(claim) => {
            let satisfied = claim.files_changed.is_empty()
                && says_already_satisfied(&claim.summary)
                && failed_calls(input) == 0;
            if satisfied {
                out.push(finding(
                    "already_satisfied",
                    FindingSeverity::Info,
                    "the agent reported the requirement already holds and delivered no changes"
                        .to_string(),
                ));
            } else if input.write_capable || !claim.files_changed.is_empty() {
                out.push(finding(
                    "claimed_unverified",
                    FindingSeverity::Block,
                    format!(
                        "the agent claimed completion ({} file(s) listed) but the workspace has no changes",
                        claim.files_changed.len()
                    ),
                ));
            }
        }
        None if input.write_capable => out.push(finding(
            "no_changes",
            FindingSeverity::Block,
            "a write-capable run ended with no changes and no finish claim".to_string(),
        )),
        None => {}
    }
}

fn claim_agreement(input: &EffectsInput, out: &mut Vec<Finding>) {
    let Some(claim) = &input.claim else {
        return;
    };
    if input.diff.files.is_empty() {
        return;
    }
    let claimed: BTreeSet<String> = claim.files_changed.iter().map(|p| normalize(p)).collect();
    let delivered: BTreeSet<String> = input
        .diff
        .files
        .iter()
        .map(|f| normalize(&f.path))
        .collect();
    let unclaimed: Vec<&String> = delivered.difference(&claimed).collect();
    if !unclaimed.is_empty() {
        out.push(finding(
            "unclaimed_change",
            FindingSeverity::Flag,
            format!(
                "changed but not claimed: {}",
                unclaimed
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    let missing: Vec<&String> = claimed.difference(&delivered).collect();
    if !missing.is_empty() {
        out.push(finding(
            "claimed_file_missing",
            FindingSeverity::Flag,
            format!(
                "claimed but not in the diff: {}",
                missing
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
}

fn scope(input: &EffectsInput, out: &mut Vec<Finding>) {
    if input.write_globs.is_empty() {
        return;
    }
    let patterns: Vec<Pattern> = input
        .write_globs
        .iter()
        .filter_map(|glob| Pattern::new(glob).ok())
        .collect();
    let outside: Vec<String> = input
        .diff
        .files
        .iter()
        .map(|f| normalize(&f.path))
        .filter(|path| !patterns.iter().any(|pattern| pattern.matches(path)))
        .collect();
    if !outside.is_empty() {
        out.push(finding(
            "out_of_scope_write",
            FindingSeverity::Block,
            format!(
                "changed outside the allowed write globs: {}",
                outside.join(", ")
            ),
        ));
    }
}

fn rewrites(input: &EffectsInput, out: &mut Vec<Finding>) {
    for file in &input.diff.files {
        let shrank_too_far =
            file.bytes_before >= MIN_BYTES_FOR_REWRITE && file.bytes_after * 5 < file.bytes_before;
        if shrank_too_far {
            out.push(finding(
                "destructive_rewrite",
                FindingSeverity::Block,
                format!(
                    "{} shrank from {} to {} bytes",
                    normalize(&file.path),
                    file.bytes_before,
                    file.bytes_after
                ),
            ));
        }
    }
}

fn build(input: &EffectsInput, out: &mut Vec<Finding>) {
    match (&input.baseline, &input.final_state) {
        (BuildState::Pass, BuildState::Fail { errors }) => out.push(finding(
            "build_regressed",
            FindingSeverity::Block,
            format!(
                "the build passed before the run and fails after it ({} error(s))",
                errors.len()
            ),
        )),
        (BuildState::Fail { .. }, BuildState::Fail { errors }) => out.push(finding(
            "build_unfixed",
            FindingSeverity::Flag,
            format!(
                "the build was failing before the run and still fails ({} error(s))",
                errors.len()
            ),
        )),
        _ => {}
    }
}

fn failure_ratio(input: &EffectsInput, out: &mut Vec<Finding>) {
    let total = input.ledger.len();
    let failed = failed_calls(input);
    if total >= MIN_CALLS_FOR_RATIO && failed * 2 > total {
        out.push(finding(
            "high_failure_ratio",
            FindingSeverity::Flag,
            format!("{failed} of {total} tool calls failed"),
        ));
    }
}

pub(super) fn evaluate(input: &EffectsInput) -> Vec<Finding> {
    let mut findings = Vec::new();
    presence(input, &mut findings);
    claim_agreement(input, &mut findings);
    scope(input, &mut findings);
    rewrites(input, &mut findings);
    build(input, &mut findings);
    failure_ratio(input, &mut findings);
    findings
}
