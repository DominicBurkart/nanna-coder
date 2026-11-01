use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream.read(&mut chunk).unwrap_or(0);
        if read == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..read]);
        let text = String::from_utf8_lossy(&data).to_string();
        if let Some(split) = text.find("\r\n\r\n") {
            let length = text[..split]
                .to_ascii_lowercase()
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if data.len() >= split + 4 + length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&data).to_string()
}

fn serve(verdict_reply: &'static str) -> String {
    serve_counting(verdict_reply, true).0
}

fn serve_counting(verdict_reply: &'static str, digest: bool) -> (String, Arc<AtomicUsize>) {
    let chats = Arc::new(AtomicUsize::new(0));
    let counter = chats.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let request = read_request(&mut stream);
            let body = if request.starts_with("GET /api/tags") {
                if digest {
                    r#"{"models":[{"name":"stub:1","modified_at":"2024-01-01T00:00:00Z","size":1,"digest":"sha256:stub"}]}"#.to_string()
                } else {
                    r#"{"models":[{"name":"stub:1","modified_at":"2024-01-01T00:00:00Z","size":1}]}"#.to_string()
                }
            } else {
                counter.fetch_add(1, Ordering::SeqCst);
                serde_json::json!({
                    "model": "stub:1",
                    "message": {"role": "assistant", "content": verdict_reply},
                    "done": true
                })
                .to_string()
            };
            let reply = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    (format!("http://{addr}"), chats)
}

fn leaking_cases(dir: &Path) {
    let source = std::fs::read_to_string(
        repo_root().join("evals/cases/auditor_spawn/allow-workspace-fit.toml"),
    )
    .unwrap()
    .replace("allow-workspace-fit", "leak")
    .replace("verdict = \"allow\"", "verdict = \"block\"");
    std::fs::write(dir.join("leak.toml"), source).unwrap();
}

fn nanna(dir: &Path, args: &[&str], model: Option<&str>) -> Output {
    let catalog = repo_root().join("evals/cases/auditor_spawn/catalog");
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanna"));
    command
        .args(["auditor", "eval", "--cases"])
        .arg(dir)
        .arg("--catalog")
        .arg(catalog)
        .args(args)
        .env_remove("NANNA_EVAL_MODEL")
        .env_remove("MODEL");
    if !args.contains(&"--runs") {
        command.args(["--runs", "1"]);
    }
    if let Some(model) = model {
        command.env("NANNA_EVAL_MODEL", model);
    }
    command.output().unwrap()
}

#[test]
fn a_model_that_allows_a_harmful_case_exits_one() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    let base = serve(r#"{"verdict":"allow"}"#);
    let output = nanna(dir.path(), &["--base-url", &base], Some("stub:1"));
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("MODEL_FALSE_ALLOW"), "{stdout}");
}

#[test]
fn a_model_that_blocks_the_harmful_case_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    let base =
        serve(r#"{"verdict":"block","reasons":[{"code":"scope_creep","detail":"too wide"}]}"#);
    let output = nanna(dir.path(), &["--base-url", &base], Some("stub:1"));
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn an_unset_model_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    let output = nanna(dir.path(), &[], None);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("no auditor model configured"));
}

#[test]
fn an_unreachable_model_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    let output = nanna(
        dir.path(),
        &["--base-url", "http://127.0.0.1:1"],
        Some("stub:1"),
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

#[test]
fn an_empty_cases_directory_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    let base = serve(r#"{"verdict":"allow"}"#);
    let output = nanna(dir.path(), &["--base-url", &base], Some("stub:1"));
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}

fn other_case(dir: &Path) {
    let source = std::fs::read_to_string(
        repo_root().join("evals/cases/auditor_spawn/allow-workspace-fit.toml"),
    )
    .unwrap()
    .replace("allow-workspace-fit", "other");
    std::fs::write(dir.join("other.toml"), source).unwrap();
}

#[test]
fn the_runs_flag_sets_how_many_model_only_calls_are_made_per_case() {
    for runs in ["1", "3"] {
        let dir = tempfile::tempdir().unwrap();
        leaking_cases(dir.path());
        let (base, chats) = serve_counting(r#"{"verdict":"allow"}"#, true);
        let output = nanna(
            dir.path(),
            &["--base-url", &base, "--runs", runs],
            Some("stub:1"),
        );
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let runs: usize = runs.parse().unwrap();
        assert_eq!(chats.load(Ordering::SeqCst), runs + 1);
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("runs: {runs}")));
    }
}

#[test]
fn max_cases_scores_a_labelled_subset_that_is_not_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    other_case(dir.path());
    let base =
        serve(r#"{"verdict":"block","reasons":[{"code":"scope_creep","detail":"too wide"}]}"#);
    let output = nanna(
        dir.path(),
        &["--base-url", &base, "--max-cases", "1"],
        Some("stub:1"),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(stdout.contains("subset: 1 of 2 cases"), "{stdout}");
    assert!(
        stdout.contains("result: PASS (subset of 1, NOT the gate)"),
        "{stdout}"
    );
}

#[test]
fn the_max_false_allow_flag_moves_the_exit_code_threshold() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    let base = serve(r#"{"verdict":"allow"}"#);
    let tolerated = nanna(
        dir.path(),
        &["--base-url", &base, "--max-false-allow", "1"],
        Some("stub:1"),
    );
    assert_eq!(tolerated.status.code(), Some(0), "{tolerated:?}");
    let strict = nanna(
        dir.path(),
        &["--base-url", &base, "--max-false-allow", "0"],
        Some("stub:1"),
    );
    assert_eq!(strict.status.code(), Some(1), "{strict:?}");
}

#[test]
fn a_model_without_a_resolvable_digest_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    let (base, chats) = serve_counting(
        r#"{"verdict":"block","reasons":[{"code":"scope_creep","detail":"x"}]}"#,
        false,
    );
    let output = nanna(dir.path(), &["--base-url", &base], Some("stub:1"));
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("digest"));
    assert_eq!(chats.load(Ordering::SeqCst), 0);
}

#[test]
fn neither_output_stream_carries_prompt_or_task_text() {
    let dir = tempfile::tempdir().unwrap();
    leaking_cases(dir.path());
    let base = serve(r#"{"verdict":"allow"}"#);
    let output = nanna(
        dir.path(),
        &["--base-url", &base, "--runs", "3"],
        Some("stub:1"),
    );
    let streams = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(streams.contains("[model_only] 1/3"), "{streams}");
    for secret in [
        "regression test for the empty-password path",
        "Fix the login bug and ship it",
        "github.com/example/repo",
    ] {
        assert!(!streams.contains(secret), "{secret} leaked:\n{streams}");
    }
}
