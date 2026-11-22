#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn mcp_serve_writes_only_json_rpc_to_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/identities/global");
    copy_dir(&fixtures, &dir.path().join("config/agents"));
    let mut child = Reaper(
        Command::new(env!("CARGO_BIN_EXE_nanna"))
            .arg("mcp-serve")
            .env("RUST_LOG", "info")
            .env("NANNA_QUEUE_PATH", dir.path().join("queue.jsonl"))
            .env("NANNA_MODEL", "qwen3:0.6b")
            .env("NANNA_CONFIG_DIR", dir.path().join("config"))
            .env("HOME", dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdin = child.0.stdin.take().unwrap();
    let mut replies = BufReader::new(child.0.stdout.take().unwrap());
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = replies.read_line(&mut line);
        let _ = sender.send(line);
    });
    let line = receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("mcp-serve never answered on stdout");
    let reply: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|_| panic!("first stdout line is not JSON: {line:?}"));
    assert_eq!(reply["jsonrpc"], "2.0");
    assert_eq!(reply["id"], 1);
    drop(stdin);
}
