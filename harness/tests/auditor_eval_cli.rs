use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let request = read_request(&mut stream);
            let body = if request.starts_with("GET /api/tags") {
                r#"{"models":[{"name":"stub:1","modified_at":"2024-01-01T00:00:00Z","size":1,"digest":"sha256:stub"}]}"#.to_string()
            } else {
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
    format!("http://{addr}")
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
        .args(["--runs", "1"])
        .args(args)
        .env_remove("NANNA_EVAL_MODEL")
        .env_remove("MODEL");
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
