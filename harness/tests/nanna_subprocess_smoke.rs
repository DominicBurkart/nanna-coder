//! Smoke test for the eval-runner subprocess shape.
//!
//! Gates the on-disk JSON contract between the `nanna agent --output-json`
//! binary and the eval-runner's `run_agent_subprocess`. If the binary's
//! report format ever drifts from `AgentRunReport`, this test will fail
//! loudly before a SWE-bench scorecard regression does.
//!
//! Requires an installed `nanna` binary (set `NANNA_HARNESS_BIN` or have
//! `nanna` on PATH) AND a reachable Ollama. Like the rest of
//! `eval_runner_tests`, this is `#[ignore]` so CI must opt in via
//! `--ignored`. When prerequisites are missing we skip with a clear
//! eprintln rather than panic.

use harness::agent::AgentRunReport;
use std::path::PathBuf;
use std::process::Command;

fn locate_nanna_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NANNA_HARNESS_BIN") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }
    which::which("nanna").ok()
}

fn ollama_reachable() -> bool {
    reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(800))
        .timeout(std::time::Duration::from_millis(800))
        .build()
        .ok()
        .and_then(|c| c.get("http://localhost:11434/api/tags").send().ok())
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn init_git_workspace(dir: &std::path::Path) {
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .unwrap();
    std::fs::write(dir.join("main.rs"), "fn main(){}").unwrap();
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .status()
        .unwrap();
    Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(dir)
        .status()
        .unwrap();
}

#[test]
#[ignore] // requires nanna binary + Ollama instance
fn nanna_agent_subprocess_emits_versioned_report() {
    let bin = match locate_nanna_binary() {
        Some(p) => p,
        None => {
            eprintln!(
                "nanna_agent_subprocess_emits_versioned_report: nanna binary not found \
                 (set NANNA_HARNESS_BIN or put `nanna` on PATH); skipping"
            );
            return;
        }
    };
    if !ollama_reachable() {
        eprintln!(
            "nanna_agent_subprocess_emits_versioned_report: Ollama not reachable on \
             :11434; skipping"
        );
        return;
    }

    let tmp = tempfile::TempDir::new().unwrap();
    init_git_workspace(tmp.path());
    let report_path = tmp.path().join("report.json");

    let output = Command::new(&bin)
        .args([
            "agent",
            "--prompt",
            "Just respond 'done' without making changes.",
            "--model",
            "qwen3:0.6b",
            "--max-iterations",
            "2",
            "--no-ensure-pod",
            "--work-dir",
        ])
        .arg(tmp.path())
        .arg("--output-json")
        .arg(&report_path)
        .output()
        .expect("failed to spawn nanna agent");

    assert!(
        output.status.success(),
        "nanna agent exited {:?}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        report_path.exists(),
        "exit-0 but no report JSON written at {}",
        report_path.display()
    );

    let report = AgentRunReport::read_from_path(&report_path).expect("report failed to parse");
    assert_eq!(
        report.schema_version,
        harness::agent::SCHEMA_VERSION,
        "subprocess report schema_version drifted from compiled-in expectation"
    );
    // We don't assert task_completed == true: the model may interpret the
    // prompt either way. We DO assert iterations > 0 — exit-0 with zero
    // iterations means the agent loop short-circuited, which would mask a
    // real subprocess regression.
    assert!(
        report.iterations >= 1,
        "iterations should be >= 1 on exit-0 success, got {}",
        report.iterations
    );
}

#[test]
fn nanna_subprocess_smoke_compiles() {
    // Ensures the test crate compiles even when the runtime gates skip the
    // ignored test. If `AgentRunReport`/`SCHEMA_VERSION` exports change
    // shape, the build fails here rather than at the next manual run.
    let _: u32 = harness::agent::SCHEMA_VERSION;
}
