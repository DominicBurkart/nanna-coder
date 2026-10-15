use harness::auditor::eval::{AuditorScorecardRow, RULE_AUDITOR_LABEL};
use std::process::Command;

#[test]
fn record_scorecard_appends_a_circular_labelled_row() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("auditor_spawn.jsonl");
    let output = Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args([
            "auditor",
            "record-scorecard",
            "--commit",
            "abc123",
            "--branch",
            "topic",
            "--out",
        ])
        .arg(&out)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(RULE_AUDITOR_LABEL), "{stdout}");
    let text = std::fs::read_to_string(&out).unwrap();
    let rows: Vec<AuditorScorecardRow> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].commit, "abc123");
    assert_eq!(rows[0].branch.as_deref(), Some("topic"));
    assert_eq!(rows[0].label, RULE_AUDITOR_LABEL);
}

#[test]
fn record_scorecard_fails_loudly_when_the_output_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args(["auditor", "record-scorecard", "--commit", "abc123", "--out"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("IsADirectory"));
}
