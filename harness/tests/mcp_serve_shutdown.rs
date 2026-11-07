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

struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline && child.try_wait().unwrap().is_none() {
        std::thread::sleep(Duration::from_millis(50));
    }
    child.try_wait().unwrap()
}

fn serve_then_signal(signal: i32) {
    let dir = tempfile::tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/identities/global");
    copy_dir(&fixtures, &dir.path().join("config/agents"));
    let mut child = Reaper(
        Command::new(env!("CARGO_BIN_EXE_nanna"))
            .arg("mcp-serve")
            .env("NANNA_QUEUE_PATH", dir.path().join("queue.jsonl"))
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
        let read = replies.read_line(&mut line).unwrap_or(0);
        let _ = sender.send(read);
    });
    let bytes = receiver.recv_timeout(Duration::from_secs(30)).unwrap_or(0);
    assert!(bytes > 0, "mcp-serve never answered on stdout");
    assert_eq!(unsafe { libc::kill(child.0.id() as i32, signal) }, 0);
    let status = wait_for_exit(&mut child.0, Duration::from_secs(10));
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
