#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn serve_then_signal(signal: i32) {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/identities/global");
    copy_dir(&fixtures, &dir.path().join("config/agents"));
    let mut child = Command::new(env!("CARGO_BIN_EXE_nanna"))
        .arg("mcp-serve")
        .env("NANNA_QUEUE_PATH", dir.path().join("queue.jsonl"))
        .env("NANNA_CONFIG_DIR", dir.path().join("config"))
        .env("HOME", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut replies = BufReader::new(child.stdout.take().unwrap());
    writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = replies.read_line(&mut line).unwrap_or(0);
        let _ = sender.send(read);
    });
    let answered = receiver.recv_timeout(Duration::from_secs(30));
    if answered.map_or(true, |bytes| bytes == 0) {
        let _ = child.kill();
        panic!("mcp-serve never answered on stdout");
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, signal) }, 0);
    let status = wait_for_exit(&mut child, Duration::from_secs(10));
    let _ = child.kill();
    let status = status.expect("mcp-serve is still running with stdin open after the signal");
    assert!(status.success(), "{status:?}");
    drop(stdin);
}

#[test]
fn mcp_serve_exits_on_sigterm_while_stdin_stays_open() {
    serve_then_signal(libc::SIGTERM);
}

#[test]
fn mcp_serve_exits_on_sigint_while_stdin_stays_open() {
    serve_then_signal(libc::SIGINT);
}
