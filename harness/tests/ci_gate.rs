#![cfg(unix)]

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn script() -> PathBuf {
    repo_root().join(".github/workflows/ci-gate.sh")
}

fn require_jq() {
    let ok = Command::new("jq")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(ok, "jq is required to test .github/workflows/ci-gate.sh");
}

struct Verdict {
    ok: bool,
    output: String,
}

fn run_gate(needs: &str, event: &str, git_ref: &str, jobs: Option<Value>) -> Verdict {
    require_jq();
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new("bash");
    cmd.arg(script())
        .env("NEEDS_JSON", needs)
        .env("GATE_EVENT", event)
        .env("GATE_REF", git_ref)
        .env("GATE_SELF_NAME", "All Checks Passed");
    if let Some(j) = jobs {
        let p = dir.path().join("jobs.json");
        std::fs::write(&p, j.to_string()).unwrap();
        cmd.env("JOBS_JSON_FILE", p);
    }
    let out = cmd.output().unwrap();
    Verdict {
        ok: out.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

const JOBS: [&str; 6] = [
    "test-matrix",
    "build-matrix",
    "build-containers",
    "security-scan",
    "cache-maintenance",
    "release",
];

fn pr_needs() -> Value {
    json!({
        "test-matrix": {"result": "success", "outputs": {}},
        "build-matrix": {"result": "success", "outputs": {}},
        "build-containers": {"result": "success", "outputs": {}},
        "security-scan": {"result": "skipped", "outputs": {}},
        "cache-maintenance": {"result": "skipped", "outputs": {}},
        "release": {"result": "skipped", "outputs": {}},
    })
}

fn with(mut needs: Value, job: &str, result: &str) -> Value {
    needs[job]["result"] = json!(result);
    needs
}

fn pr(needs: Value) -> Verdict {
    run_gate(
        &needs.to_string(),
        "pull_request",
        "refs/pull/1/merge",
        None,
    )
}

#[test]
fn all_success_or_allowed_skips_on_pull_request_passes() {
    let v = pr(pr_needs());
    assert!(v.ok, "{}", v.output);
}

#[test]
fn failure_leg_fails() {
    let v = pr(with(pr_needs(), "build-matrix", "failure"));
    assert!(!v.ok, "{}", v.output);
    assert!(v.output.contains("build-matrix"), "{}", v.output);
}

#[test]
fn cancelled_leg_fails() {
    let v = pr(with(pr_needs(), "test-matrix", "cancelled"));
    assert!(!v.ok, "{}", v.output);
    assert!(v.output.contains("test-matrix"), "{}", v.output);
}

#[test]
fn skipped_leg_not_in_allow_list_fails() {
    let v = pr(with(pr_needs(), "build-matrix", "skipped"));
    assert!(!v.ok, "{}", v.output);
    assert!(v.output.contains("build-matrix"), "{}", v.output);
}

#[test]
fn skipped_test_matrix_fails() {
    let v = pr(with(pr_needs(), "test-matrix", "skipped"));
    assert!(!v.ok, "{}", v.output);
}

#[test]
fn allowed_skip_passes_only_for_its_event() {
    let v = pr(pr_needs());
    assert!(v.ok, "{}", v.output);
    let main_push = run_gate(&pr_needs().to_string(), "push", "refs/heads/main", None);
    assert!(!main_push.ok, "{}", main_push.output);
    assert!(
        main_push.output.contains("security-scan"),
        "{}",
        main_push.output
    );
}

#[test]
fn push_to_main_passes_when_everything_that_should_run_ran() {
    let mut n = pr_needs();
    n["security-scan"]["result"] = json!("success");
    n["cache-maintenance"]["result"] = json!("success");
    let v = run_gate(&n.to_string(), "push", "refs/heads/main", None);
    assert!(v.ok, "{}", v.output);
}

#[test]
fn cache_maintenance_skipped_on_main_push_fails() {
    let mut n = pr_needs();
    n["security-scan"]["result"] = json!("success");
    let v = run_gate(&n.to_string(), "push", "refs/heads/main", None);
    assert!(!v.ok, "{}", v.output);
    assert!(v.output.contains("cache-maintenance"), "{}", v.output);
}

#[test]
fn cache_maintenance_skip_allowed_on_develop_push() {
    let mut n = pr_needs();
    n["security-scan"]["result"] = json!("success");
    let v = run_gate(&n.to_string(), "push", "refs/heads/develop", None);
    assert!(v.ok, "{}", v.output);
}

#[test]
fn empty_needs_fails() {
    let v = run_gate("{}", "pull_request", "refs/pull/1/merge", None);
    assert!(!v.ok, "{}", v.output);
}

#[test]
fn unknown_result_value_fails() {
    for bogus in ["", "succeeded", "SUCCESS", "neutral", "timed_out"] {
        let v = pr(with(pr_needs(), "test-matrix", bogus));
        assert!(!v.ok, "result {bogus:?} passed: {}", v.output);
    }
}

#[test]
fn malformed_needs_fails() {
    for bad in [
        "",
        "not json",
        "[]",
        "null",
        "{\"test-matrix\": \"success\"}",
        "{\"test-matrix\": {}}",
        "{\"test-matrix\": {\"result\": null}}",
        "{\"test-matrix\": {\"result\": 1}}",
        "{\"test-matrix\": {\"result\": {\"a\": 1}}}",
        "{\"test-matrix\": {\"result\": [\"success\"]}}",
        "{\"test-matrix\": {\"result\": true}}",
    ] {
        let v = run_gate(bad, "pull_request", "refs/pull/1/merge", None);
        assert!(!v.ok, "needs {bad:?} passed: {}", v.output);
    }
}

#[test]
fn missing_expected_job_in_needs_fails() {
    let mut n = pr_needs();
    n.as_object_mut().unwrap().remove("build-containers");
    let v = pr(n);
    assert!(!v.ok, "{}", v.output);
    assert!(v.output.contains("build-containers"), "{}", v.output);
}

#[test]
fn unknown_job_in_needs_with_success_passes_but_skipped_fails() {
    let mut n = pr_needs();
    n["brand-new"] = json!({"result": "skipped"});
    let v = pr(n);
    assert!(!v.ok, "{}", v.output);
}

#[test]
fn hostile_event_and_ref_are_data_not_code() {
    let hostile = "refs/heads/$(touch /var/tmp/m1-849-tmp/pwned);`id`\"'";
    let v = run_gate(&pr_needs().to_string(), "pull_request", hostile, None);
    assert!(v.ok, "{}", v.output);
    assert!(!std::path::Path::new("/var/tmp/m1-849-tmp/pwned").exists());
}

fn jobs_with(entries: &[(&str, &str, &str)]) -> Value {
    let jobs: Vec<Value> = entries
        .iter()
        .map(|(n, s, c)| {
            json!({"name": n, "status": s, "conclusion": if c.is_empty() { Value::Null } else { json!(c) }})
        })
        .collect();
    json!({ "jobs": jobs })
}

fn run_with_jobs(jobs: Value) -> Verdict {
    run_gate(
        &pr_needs().to_string(),
        "pull_request",
        "refs/pull/1/merge",
        Some(jobs),
    )
}

#[test]
fn per_leg_all_success_passes() {
    let v = run_with_jobs(jobs_with(&[
        ("unit (macos-latest)", "completed", "success"),
        ("unit (ubuntu-latest)", "completed", "success"),
        ("Security Scan", "completed", "skipped"),
        ("All Checks Passed", "in_progress", ""),
    ]));
    assert!(v.ok, "{}", v.output);
}

#[test]
fn per_leg_cancelled_matrix_leg_fails_even_if_needs_say_success() {
    let v = run_with_jobs(jobs_with(&[
        ("unit (macos-latest)", "completed", "cancelled"),
        ("unit (ubuntu-latest)", "completed", "success"),
        ("All Checks Passed", "in_progress", ""),
    ]));
    assert!(!v.ok, "{}", v.output);
    assert!(v.output.contains("unit (macos-latest)"), "{}", v.output);
}

#[test]
fn per_leg_failure_timed_out_and_incomplete_fail() {
    for (status, concl) in [
        ("completed", "failure"),
        ("completed", "timed_out"),
        ("completed", "action_required"),
        ("completed", "neutral"),
        ("in_progress", ""),
        ("queued", ""),
    ] {
        let v = run_with_jobs(jobs_with(&[
            ("unit (ubuntu-latest)", status, concl),
            ("All Checks Passed", "in_progress", ""),
        ]));
        assert!(!v.ok, "{status}/{concl} passed: {}", v.output);
    }
}

#[test]
fn per_leg_malformed_or_empty_jobs_fail() {
    for bad in [
        json!({"jobs": []}),
        json!({}),
        json!([]),
        json!({"jobs": [{"name": "x"}]}),
    ] {
        let v = run_with_jobs(bad.clone());
        assert!(!v.ok, "{bad} passed: {}", v.output);
    }
}

#[test]
fn per_leg_missing_jobs_file_fails() {
    require_jq();
    let out = Command::new("bash")
        .arg(script())
        .env("NEEDS_JSON", pr_needs().to_string())
        .env("GATE_EVENT", "pull_request")
        .env("GATE_REF", "refs/pull/1/merge")
        .env("GATE_SELF_NAME", "All Checks Passed")
        .env("JOBS_JSON_FILE", "/var/tmp/m1-849-tmp/does-not-exist.json")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn run_968_attempt_1_is_rejected() {
    let needs = json!({
        "test-matrix": {"result": "success"},
        "build-matrix": {"result": "skipped"},
        "build-containers": {"result": "skipped"},
        "security-scan": {"result": "skipped"},
        "cache-maintenance": {"result": "skipped"},
        "release": {"result": "skipped"},
    });
    let v = run_gate(
        &needs.to_string(),
        "pull_request",
        "refs/pull/968/merge",
        Some(jobs_with(&[
            ("unit (macos-latest)", "completed", "cancelled"),
            ("unit (ubuntu-latest)", "completed", "success"),
            ("All Checks Passed", "in_progress", ""),
        ])),
    );
    assert!(!v.ok, "{}", v.output);
}

#[test]
fn allow_list_names_only_real_jobs_and_never_the_core_legs() {
    let ci = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap();
    let list =
        std::fs::read_to_string(repo_root().join(".github/workflows/ci-gate-allowed-skips.txt"))
            .unwrap();
    for line in list.lines().filter(|l| !l.trim().is_empty()) {
        let job = line.split_whitespace().next().unwrap();
        assert!(JOBS.contains(&job), "unknown job in allow-list: {job}");
        assert!(
            !["test-matrix", "build-matrix", "build-containers"].contains(&job),
            "core leg may never be skippable: {job}"
        );
        assert!(ci.contains(&format!("\n  {job}:")), "{job} not in ci.yml");
    }
}

fn gate_job_text() -> String {
    let ci = std::fs::read_to_string(repo_root().join(".github/workflows/ci.yml")).unwrap();
    let start = ci.find("\n  all-checks:\n").expect("all-checks job");
    ci[start..].to_string()
}

#[test]
fn gate_job_always_runs_and_decision_step_is_unconditional() {
    let job = gate_job_text();
    assert!(job.contains("\n    if: always()\n"), "{job}");
    let step_start = job
        .find("- name: Decide gate verdict")
        .expect("decide step");
    let step = &job[step_start..];
    let step_end = step[1..].find("\n      - ").map_or(step.len(), |i| i + 1);
    let step = &step[..step_end];
    assert!(
        !step.contains("\n        if:"),
        "decision step is conditional: {step}"
    );
    assert!(step.contains("ci-gate.sh"), "{step}");
    assert!(step.contains("NEEDS_JSON: ${{ toJSON(needs) }}"), "{step}");
}

#[test]
fn gate_job_never_interpolates_expressions_into_shell() {
    let job = gate_job_text();
    for line in job.lines() {
        let t = line.trim_start();
        if t.starts_with("run:") || (!t.contains(": ") && t.contains("${{")) {
            assert!(!t.contains("${{"), "expression in shell: {line}");
        }
    }
}

#[test]
fn gate_job_can_read_actions() {
    let job = gate_job_text();
    assert!(job.contains("\n      actions: read\n"), "{job}");
}

#[test]
fn non_string_result_among_otherwise_successful_jobs_fails() {
    for bad in [
        json!({"a": 1}),
        json!(["success"]),
        json!(true),
        json!(1),
        Value::Null,
    ] {
        let mut n = pr_needs();
        n["test-matrix"]["result"] = bad.clone();
        let v = pr(n);
        assert!(!v.ok, "result {bad} passed: {}", v.output);
        assert!(v.output.contains("malformed"), "{}", v.output);
    }
}

#[test]
fn empty_needs_is_reported_as_empty_or_malformed() {
    let v = run_gate("{}", "pull_request", "refs/pull/1/merge", None);
    assert!(!v.ok);
    assert!(v.output.contains("empty or malformed"), "{}", v.output);
}

#[test]
fn per_leg_incomplete_job_fails_even_with_a_success_conclusion() {
    for (status, concl) in [
        ("in_progress", "success"),
        ("queued", "skipped"),
        ("waiting", "success"),
    ] {
        let v = run_with_jobs(jobs_with(&[
            ("unit (ubuntu-latest)", status, concl),
            ("All Checks Passed", "in_progress", ""),
        ]));
        assert!(!v.ok, "{status}/{concl} passed: {}", v.output);
        assert!(v.output.contains("not completed"), "{}", v.output);
    }
}

fn develop_push_needs() -> String {
    let mut n = pr_needs();
    n["security-scan"]["result"] = json!("success");
    n.to_string()
}

#[test]
fn ref_rule_is_an_exact_match_not_a_prefix() {
    for r in [
        "refs/heads/developer",
        "refs/heads/develop;x",
        "refs/heads/develop/x",
        "refs/heads/devel",
        "refs/heads/Develop",
        "refs/heads/develop ",
        "",
    ] {
        let v = run_gate(&develop_push_needs(), "push", r, None);
        assert!(!v.ok, "ref {r:?} passed: {}", v.output);
    }
    let ok = run_gate(&develop_push_needs(), "push", "refs/heads/develop", None);
    assert!(ok.ok, "{}", ok.output);
}

#[test]
fn missing_allow_list_file_fails_closed() {
    require_jq();
    let out = Command::new("bash")
        .arg(script())
        .env("NEEDS_JSON", pr_needs().to_string())
        .env("GATE_EVENT", "pull_request")
        .env("GATE_REF", "refs/pull/1/merge")
        .env(
            "ALLOWED_SKIPS_FILE",
            "/var/tmp/m1-849-tmp/no-such-allow-list.txt",
        )
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text =
        String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("security-scan"), "{text}");
}

#[test]
fn comment_style_line_in_allow_list_grants_nothing() {
    require_jq();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("allow.txt");
    std::fs::write(&f, "#security-scan pull_request *\n").unwrap();
    let out = Command::new("bash")
        .arg(script())
        .env("NEEDS_JSON", pr_needs().to_string())
        .env("GATE_EVENT", "pull_request")
        .env("GATE_REF", "refs/pull/1/merge")
        .env("ALLOWED_SKIPS_FILE", &f)
        .output()
        .unwrap();
    assert!(!out.status.success());
}
