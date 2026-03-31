//! Smoke test: verify the MCP server binary starts and responds to initialize.
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn test_harness_help_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_harness"))
        .arg("--help")
        .output()
        .expect("Failed to run harness --help");
    assert!(output.status.success(), "harness --help failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mcp-serve"),
        "Should list mcp-serve subcommand"
    );
}

#[test]
fn test_mcp_serve_responds_to_initialize() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["mcp-serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start harness mcp-serve");

    let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let header = format!("Content-Length: {}\r\n\r\n", init_msg.len());

    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(header.as_bytes()).unwrap();
    stdin.write_all(init_msg.as_bytes()).unwrap();
    stdin.flush().unwrap();
    drop(child.stdin.take());

    // Read stdout in a background thread with a timeout to avoid hanging indefinitely.
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = std::io::BufReader::new(stdout);
        let mut response_buf = vec![0u8; 4096];
        let n = reader.read(&mut response_buf).unwrap_or(0);
        let _ = tx.send(String::from_utf8_lossy(&response_buf[..n]).into_owned());
    });

    let timeout = Duration::from_secs(30);
    let response = match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Timed out ({timeout:?}) waiting for MCP initialize response");
        }
    };

    // Capture stderr for diagnostics before killing the process.
    let stderr_output = child.stderr.take().map(|mut se| {
        use std::io::Read;
        let mut buf = String::new();
        let _ = se.read_to_string(&mut buf);
        buf
    });

    let _ = child.kill();
    let status = child.wait();

    assert!(
        response.contains("serverInfo")
            || response.contains("protocolVersion")
            || response.contains("\"result\""),
        "MCP initialize should return server info, got: {}\nstderr: {}\nexit status: {:?}",
        &response[..response.len().min(500)],
        stderr_output.as_deref().unwrap_or("<none>"),
        status,
    );

    // Warn (but don't fail) if process produced unexpected stderr.
    if let Some(ref stderr) = stderr_output {
        if !stderr.is_empty() {
            eprintln!("[smoke_mcp] stderr from mcp-serve:\n{stderr}");
        }
    }
}
