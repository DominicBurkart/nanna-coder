//! Integration tests for the `--log-file` CLI surface introduced in PR1 of #219.
//!
//! The critical regression test here is the "no flag, no file" invariant: when
//! `--log-file` is not supplied, the harness must not touch the filesystem for
//! logging purposes. The model-dependent smoke test is `#[ignore]`d so these
//! tests run in any environment without an Ollama instance.

use assert_cmd::Command;
use tempfile::tempdir;

/// Regression test: when `--log-file` is absent, running the harness must not
/// create any files in the current working directory. Uses the `Tools`
/// subcommand which does not require a model backend.
#[test]
fn no_log_file_flag_produces_zero_file_writes() {
    let workdir = tempdir().expect("create tempdir");

    // Snapshot the directory contents before the run. The harness must not
    // create any new files in this directory when `--log-file` is absent.
    let before: Vec<_> = std::fs::read_dir(workdir.path())
        .expect("read tempdir")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert!(before.is_empty(), "tempdir should start empty");

    let mut cmd = Command::cargo_bin("harness").expect("locate harness binary");
    cmd.current_dir(workdir.path())
        .env_remove("NANNA_LOG_FILE")
        .env_remove("NANNA_LOG_FILE_LEVEL")
        .arg("tools");

    let output = cmd.output().expect("spawn harness");
    assert!(
        output.status.success(),
        "harness tools failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let after: Vec<_> = std::fs::read_dir(workdir.path())
        .expect("read tempdir")
        .filter_map(|e| e.ok().map(|e| e.file_name()))
        .collect();
    assert!(
        after.is_empty(),
        "harness must not write files when --log-file is absent; found: {:?}",
        after
    );
}

/// When `--log-file` is supplied, the harness creates the file (the
/// non-blocking appender opens it lazily on first write). Any line present
/// must be a valid JSON object. Uses the `Tools` subcommand so no Ollama is
/// required; the only events that may fire on this path are internal tracing
/// events from workspace setup code reached before we exit.
#[test]
fn log_file_flag_produces_jsonl_output_if_any() {
    let workdir = tempdir().expect("create tempdir");
    let log_path = workdir.path().join("nanna.log");

    let mut cmd = Command::cargo_bin("harness").expect("locate harness binary");
    cmd.current_dir(workdir.path())
        .env_remove("NANNA_LOG_FILE")
        .env_remove("NANNA_LOG_FILE_LEVEL")
        .arg("--log-file")
        .arg(&log_path)
        .arg("tools");

    let output = cmd.output().expect("spawn harness");
    assert!(
        output.status.success(),
        "harness failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    // If the file was created, every non-empty line must parse as a JSON
    // object. The `Tools` subcommand path is light on tracing emissions, so
    // the file may be empty or absent — the invariant we enforce is that if
    // anything was written, it is well-formed JSON lines.
    if log_path.exists() {
        let contents = std::fs::read_to_string(&log_path).expect("read log file");
        for line in contents.lines().filter(|l| !l.trim().is_empty()) {
            let parsed: serde_json::Value =
                serde_json::from_str(line).unwrap_or_else(|_| panic!("non-JSON line: {}", line));
            assert!(
                parsed.is_object(),
                "log line is not a JSON object: {}",
                line
            );
        }
    }
}

/// Smoke test that exercises a code path known to emit `session.start` and
/// `env.snapshot` events: `agent --prompt hi`. This requires Ollama at
/// runtime, so it is `#[ignore]`d by default and can be run manually via
/// `cargo test -p harness --test logging_integration -- --ignored`.
#[test]
#[ignore]
fn agent_run_writes_session_start_and_env_snapshot() {
    let workdir = tempdir().expect("create tempdir");
    let log_path = workdir.path().join("nanna.log");

    let mut cmd = Command::cargo_bin("harness").expect("locate harness binary");
    cmd.current_dir(workdir.path())
        .arg("--log-file")
        .arg(&log_path)
        .arg("agent")
        .arg("--prompt")
        .arg("hi");

    let output = cmd.output().expect("spawn harness");
    assert!(output.status.success(), "agent run failed");

    let contents = std::fs::read_to_string(&log_path).expect("read log file");
    let has_session_start = contents
        .lines()
        .any(|l| l.contains("\"session.start\"") && l.contains("\"user_prompt\":\"hi\""));
    let has_env_snapshot = contents.lines().any(|l| l.contains("\"env.snapshot\""));
    assert!(
        has_session_start,
        "missing session.start event: {}",
        contents
    );
    assert!(has_env_snapshot, "missing env.snapshot event: {}", contents);
}
