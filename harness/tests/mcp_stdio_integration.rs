//! Integration test for `harness mcp-serve` stdio framing (issue #201).
//!
//! Spawns the compiled `harness` binary as a child process with piped
//! stdin/stdout, sends real JSON-RPC over the pipe, and asserts:
//!   1. `initialize` returns `protocolVersion: "2024-11-05"` and
//!      `capabilities.tools` as an object;
//!   2. `tools/list` returns exactly the 6 expected tool names;
//!   3. `notifications/initialized` produces no response bytes;
//!   4. stdout contains no `Content-Length:` header anywhere;
//!   5. the child exits cleanly (status 0) within a short timeout after
//!      stdin closes.
//!
//! The test uses `NANNA_MCP_MOCK_PROVIDER=1` to bypass Ollama. It does not
//! require a model backend.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

const RPC_TIMEOUT: Duration = Duration::from_secs(10);

fn harness_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harness"))
}

async fn send(
    stdin: &mut tokio::process::ChildStdin,
    payload: &serde_json::Value,
) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(payload)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await
}

async fn recv_line<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<String> {
    let mut line = String::new();
    let read = timeout(RPC_TIMEOUT, reader.read_line(&mut line))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "recv_line timed out"))??;
    if read == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "child closed stdout before response",
        ));
    }
    Ok(line)
}

#[tokio::test]
async fn mcp_stdio_end_to_end_initialize_and_tools_list() {
    let mut child = Command::new(harness_bin())
        .args(["mcp-serve", "--model", "mock:test"])
        .env("NANNA_MCP_MOCK_PROVIDER", "1")
        .env("RUST_LOG", "warn")
        // Prevent accidental contact with a developer's local Ollama.
        .env_remove("OLLAMA_HOST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn harness mcp-serve");

    let mut stdin = child.stdin.take().expect("take stdin");
    let stdout = child.stdout.take().expect("take stdout");
    let stderr = child.stderr.take().expect("take stderr");
    let mut reader = BufReader::new(stdout);

    // Drain stderr in the background so a chatty logger can't fill the pipe
    // and deadlock the child on its stdout writes.
    let stderr_handle = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = BufReader::new(stderr).read_to_end(&mut buf).await;
        String::from_utf8_lossy(&buf).into_owned()
    });

    // 1) initialize
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "mcp-stdio-integration-test", "version": "0" }
            }
        }),
    )
    .await
    .expect("write initialize");

    let init_line = recv_line(&mut reader)
        .await
        .expect("read initialize response");
    let init: serde_json::Value =
        serde_json::from_str(init_line.trim()).expect("parse initialize response");
    assert_eq!(init["jsonrpc"], "2.0", "initialize: jsonrpc");
    assert_eq!(init["id"], 1, "initialize: id");
    assert_eq!(
        init["result"]["protocolVersion"], "2024-11-05",
        "initialize: protocolVersion"
    );
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "initialize: capabilities.tools must be an object, got {:?}",
        init["result"]["capabilities"]["tools"]
    );
    assert_eq!(
        init["result"]["serverInfo"]["name"], "nanna",
        "initialize: serverInfo.name"
    );
    // Server must NOT echo client capabilities back — regression guard.
    assert!(
        init["result"]["clientCapabilities"].is_null(),
        "initialize response leaked client capabilities"
    );

    // 2) notifications/initialized — no response expected
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    )
    .await
    .expect("write notifications/initialized");

    // 3) tools/list
    send(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    )
    .await
    .expect("write tools/list");

    let tools_line = recv_line(&mut reader)
        .await
        .expect("read tools/list response");
    let tools_resp: serde_json::Value =
        serde_json::from_str(tools_line.trim()).expect("parse tools/list response");
    assert_eq!(tools_resp["id"], 2, "tools/list: id");
    let tools = tools_resp["result"]["tools"]
        .as_array()
        .expect("tools/list: result.tools must be an array");
    assert_eq!(tools.len(), 6, "tools/list: expected exactly 6 tools");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    let expected = [
        "assign_task",
        "cancel_task",
        "get_result",
        "list_tasks",
        "onboard_repo",
        "poll_task",
    ];
    assert_eq!(names, expected, "tools/list: unexpected tool names");

    // 4) Close stdin; drain any remaining stdout; then assert clean exit.
    drop(stdin);

    let mut remaining = String::new();
    let _ = timeout(RPC_TIMEOUT, reader.read_to_string(&mut remaining))
        .await
        .expect("drain remaining stdout");

    let status = timeout(RPC_TIMEOUT, child.wait())
        .await
        .expect("child wait timed out")
        .expect("child wait failed");

    let stderr_contents = stderr_handle.await.unwrap_or_default();
    assert!(
        status.success(),
        "child exited non-zero: {status:?}\nstderr:\n{stderr_contents}"
    );

    // 5) Framing invariants — reconstruct full stdout from the lines we read
    // plus anything left over after stdin close.
    let full_stdout = format!("{init_line}{tools_line}{remaining}");
    assert!(
        !full_stdout.to_ascii_lowercase().contains("content-length"),
        "stdout leaked a Content-Length header: {full_stdout:?}"
    );
    // Exactly two non-empty lines produced across the session (one per id'd
    // request). Notifications produce nothing.
    let non_empty: Vec<&str> = full_stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        non_empty.len(),
        2,
        "expected 2 response lines, got {}:\n{non_empty:#?}",
        non_empty.len()
    );
    for line in &non_empty {
        serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
            panic!("non-JSON line on stdout: {line:?} err={e}");
        });
    }
}
