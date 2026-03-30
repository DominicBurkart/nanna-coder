//! Smoke test: verify the MCP server binary starts and responds to initialize.
use std::io::{Read, Write};
use std::process::{Command, Stdio};

#[test]
fn test_harness_help_exits_zero() {
    let output = Command::new(env!("CARGO_BIN_EXE_harness"))
        .arg("--help")
        .output()
        .expect("Failed to run harness --help");
    assert!(output.status.success(), "harness --help failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mcp-serve"), "Should list mcp-serve subcommand");
}

#[test]
fn test_mcp_serve_responds_to_initialize() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["mcp-serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("Failed to start harness mcp-serve");

    let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let header = format!("Content-Length: {}\r\n\r\n", init_msg.len());

    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(header.as_bytes()).unwrap();
    stdin.write_all(init_msg.as_bytes()).unwrap();
    stdin.flush().unwrap();
    drop(child.stdin.take());

    let stdout = child.stdout.take().unwrap();
    let mut reader = std::io::BufReader::new(stdout);
    let mut response_buf = vec![0u8; 4096];
    let n = reader.read(&mut response_buf).unwrap_or(0);
    let response = String::from_utf8_lossy(&response_buf[..n]);

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        response.contains("serverInfo") || response.contains("protocolVersion"),
        "MCP initialize should return server info, got: {}",
        &response[..response.len().min(500)]
    );
}
