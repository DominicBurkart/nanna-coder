//! Smoke tests: verify the MCP server binary starts, responds to initialize,
//! and exposes all expected tools over the stdio transport.
//!
//! Mock tier (no Ollama): binary startup, initialize response, and full
//! stdio transport round-trip including tools/list.
//!
//! Live tier (#[ignore], requires Ollama): task submitted via handlers
//! with a real OllamaProvider reaches a terminal state with a result.
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

// ── Live-tier constants ──────────────────────────────────────────────────────
const MAX_ATTEMPTS: usize = 3;
const LIVE_TASK_TIMEOUT: Duration = Duration::from_secs(300);
const LIVE_MODEL: &str = "qwen3:0.6b";

// ── Expected MCP tool names ──────────────────────────────────────────────────
const EXPECTED_TOOLS: &[&str] = &[
    "assign_task",
    "poll_task",
    "get_result",
    "list_tasks",
    "cancel_task",
    "onboard_repo",
];

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Send a single JSON-RPC message framed with Content-Length and read back
/// the next framed response from the server.
fn send_and_recv(
    stdin: &mut std::process::ChildStdin,
    stdout_reader: &mut dyn std::io::Read,
    msg: &str,
) -> String {
    // Write request
    let header = format!("Content-Length: {}\r\n\r\n", msg.len());
    stdin.write_all(header.as_bytes()).unwrap();
    stdin.write_all(msg.as_bytes()).unwrap();
    stdin.flush().unwrap();

    // Read response header until \r\n\r\n
    let mut header_bytes: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stdout_reader.read_exact(&mut byte).is_err() {
            break;
        }
        header_bytes.push(byte[0]);
        let n = header_bytes.len();
        if n >= 4
            && header_bytes[n - 4] == b'\r'
            && header_bytes[n - 3] == b'\n'
            && header_bytes[n - 2] == b'\r'
            && header_bytes[n - 1] == b'\n'
        {
            break;
        }
    }

    // Parse Content-Length from response header
    let header_str = String::from_utf8_lossy(&header_bytes);
    let content_length: usize = header_str
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    let mut body = vec![0u8; content_length];
    let _ = stdout_reader.read_exact(&mut body);
    String::from_utf8_lossy(&body).into_owned()
}

// ── Mock-tier tests ──────────────────────────────────────────────────────────

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

/// End-to-end stdio transport test.
///
/// Spawns `harness mcp-serve`, performs the full MCP handshake over stdio
/// using Content-Length framing, sends `initialize` followed by `tools/list`,
/// and asserts that all 6 expected tools are returned. This exercises
/// `NannaMcpServer::run_stdio()` end-to-end.
#[test]
fn test_mcp_stdio_tools_list() {
    use std::io::Read;

    let mut child = Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["mcp-serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start harness mcp-serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    let timeout = Duration::from_secs(30);

    // Run the whole exchange in a thread so we can impose a wall-clock timeout.
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<String>, String>>();
    std::thread::spawn(move || {
        // 1. initialize
        let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let init_resp = send_and_recv(&mut stdin, &mut stdout, init_msg);
        if !init_resp.contains("protocolVersion") && !init_resp.contains("serverInfo") {
            let _ = tx.send(Err(format!(
                "initialize response missing expected fields: {}",
                &init_resp[..init_resp.len().min(500)]
            )));
            return;
        }

        // 2. notifications/initialized — server returns no body for this, so
        //    just write it without expecting a response.
        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let notif_header = format!("Content-Length: {}\r\n\r\n", notif.len());
        stdin.write_all(notif_header.as_bytes()).unwrap();
        stdin.write_all(notif.as_bytes()).unwrap();
        stdin.flush().unwrap();

        // 3. tools/list
        let list_msg = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let list_resp = send_and_recv(&mut stdin, &mut stdout, list_msg);

        // Parse tool names out of the response
        let tool_names: Vec<String> = match serde_json::from_str::<serde_json::Value>(&list_resp) {
            Ok(v) => v["result"]["tools"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
                .collect(),
            Err(e) => {
                let _ = tx.send(Err(format!(
                    "Failed to parse tools/list response: {}\nRaw: {}",
                    e,
                    &list_resp[..list_resp.len().min(500)]
                )));
                return;
            }
        };

        let _ = tx.send(Ok(tool_names));
    });

    let result = match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Timed out ({timeout:?}) waiting for MCP tools/list response");
        }
    };

    let _ = child.kill();
    let stderr_output = child.stderr.take().map(|mut se| {
        let mut buf = String::new();
        let _ = se.read_to_string(&mut buf);
        buf
    });
    let _ = child.wait();

    if let Some(ref stderr) = stderr_output {
        if !stderr.is_empty() {
            eprintln!("[smoke_mcp] stderr from mcp-serve:\n{stderr}");
        }
    }

    let tool_names = result.unwrap_or_else(|e| panic!("stdio transport test failed: {e}"));

    for expected in EXPECTED_TOOLS {
        assert!(
            tool_names.iter().any(|n| n == expected),
            "tools/list missing expected tool '{expected}'. Got: {tool_names:?}"
        );
    }
    assert_eq!(
        tool_names.len(),
        EXPECTED_TOOLS.len(),
        "Expected exactly {} tools, got {}: {tool_names:?}",
        EXPECTED_TOOLS.len(),
        tool_names.len()
    );
}

/// Smoke test for `tools/call` with `list_tasks`.
///
/// Spawns `harness mcp-serve`, performs the full MCP handshake over stdio,
/// then sends a `tools/call` JSON-RPC request with `name: "list_tasks"` and
/// `arguments: {}`. Asserts the response contains a `"content"` array whose
/// first element has parseable JSON text. This validates the full stdio
/// transport for tool execution without needing Ollama.
#[test]
fn test_mcp_stdio_tools_call_list_tasks() {
    use std::io::Read;

    let mut child = Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["mcp-serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to start harness mcp-serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();

    let timeout = Duration::from_secs(30);

    let (tx, rx) = std::sync::mpsc::channel::<Result<serde_json::Value, String>>();
    std::thread::spawn(move || {
        // 1. initialize
        let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let init_resp = send_and_recv(&mut stdin, &mut stdout, init_msg);
        if !init_resp.contains("protocolVersion") && !init_resp.contains("serverInfo") {
            let _ = tx.send(Err(format!(
                "initialize response missing expected fields: {}",
                &init_resp[..init_resp.len().min(500)]
            )));
            return;
        }

        // 2. notifications/initialized (no response expected)
        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let notif_header = format!("Content-Length: {}\r\n\r\n", notif.len());
        stdin.write_all(notif_header.as_bytes()).unwrap();
        stdin.write_all(notif.as_bytes()).unwrap();
        stdin.flush().unwrap();

        // 3. tools/call list_tasks
        let call_msg = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_tasks","arguments":{}}}"#;
        let call_resp = send_and_recv(&mut stdin, &mut stdout, call_msg);

        match serde_json::from_str::<serde_json::Value>(&call_resp) {
            Ok(v) => {
                let _ = tx.send(Ok(v));
            }
            Err(e) => {
                let _ = tx.send(Err(format!(
                    "Failed to parse tools/call response: {}\nRaw: {}",
                    e,
                    &call_resp[..call_resp.len().min(500)]
                )));
            }
        }
    });

    let result = match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Timed out ({timeout:?}) waiting for tools/call list_tasks response");
        }
    };

    let _ = child.kill();
    let stderr_output = child.stderr.take().map(|mut se| {
        let mut buf = String::new();
        let _ = se.read_to_string(&mut buf);
        buf
    });
    let _ = child.wait();

    if let Some(ref stderr) = stderr_output {
        if !stderr.is_empty() {
            eprintln!("[smoke_mcp] stderr from mcp-serve:\n{stderr}");
        }
    }

    let resp = result.unwrap_or_else(|e| panic!("tools/call list_tasks failed: {e}"));

    // The MCP tools/call response must contain a "content" array in the result.
    let content = resp["result"]["content"]
        .as_array()
        .unwrap_or_else(|| {
            panic!(
                "tools/call list_tasks: expected result.content array, got: {}",
                serde_json::to_string_pretty(&resp).unwrap_or_default()
            )
        });

    assert!(
        !content.is_empty(),
        "tools/call list_tasks: content array is empty"
    );

    // The first content element should have a "text" field with parseable JSON.
    let text = content[0]["text"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "tools/call list_tasks: first content element missing 'text': {:?}",
                content[0]
            )
        });

    let parsed: serde_json::Value = serde_json::from_str(text).unwrap_or_else(|e| {
        panic!(
            "tools/call list_tasks: content text is not valid JSON: {}\nRaw: {}",
            e,
            &text[..text.len().min(500)]
        )
    });

    // Sanity: the parsed JSON should be an object (the list_tasks handler returns
    // a JSON object with task information).
    assert!(
        parsed.is_object() || parsed.is_array(),
        "tools/call list_tasks: expected JSON object or array, got: {}",
        &text[..text.len().min(200)]
    );
}

// ── Live-tier test ────────────────────────────────────────────────────────────

/// Live MCP task roundtrip smoke test.
///
/// Submits a trivial task via `handle_assign_task` with a real
/// `OllamaProvider` (qwen3:0.6b), polls with `handle_poll_task` until a
/// terminal state (`Completed` or `Failed`) is reached, then calls
/// `handle_get_result` and asserts a well-formed response.
///
/// Uses a retry loop (MAX_ATTEMPTS = 3) and treats timeouts as soft
/// failures that trigger a retry, following the pattern in
/// `dev_container_integration.rs`.
///
/// Requires Ollama to be running with qwen3:0.6b pulled.
/// Run with: cargo test test_mcp_live_task_roundtrip -- --ignored
#[tokio::test]
#[ignore]
async fn test_mcp_live_task_roundtrip() {
    let mut last_error: Option<String> = None;

    for attempt in 0..MAX_ATTEMPTS {
        eprintln!("[smoke_mcp live] attempt {}/{}", attempt + 1, MAX_ATTEMPTS);

        let result = tokio::time::timeout(
            LIVE_TASK_TIMEOUT,
            run_live_task_attempt(),
        )
        .await;

        match result {
            Ok(Ok(())) => {
                eprintln!("[smoke_mcp live] passed on attempt {}", attempt + 1);
                return;
            }
            Ok(Err(e)) => {
                eprintln!("[smoke_mcp live] attempt {} failed: {}", attempt + 1, e);
                last_error = Some(e);
            }
            Err(_) => {
                eprintln!(
                    "[smoke_mcp live] attempt {} timed out after {:?} (soft failure, retrying)",
                    attempt + 1,
                    LIVE_TASK_TIMEOUT
                );
                last_error = Some(format!("timed out after {:?}", LIVE_TASK_TIMEOUT));
            }
        }
    }

    panic!(
        "[smoke_mcp live] all {} attempts failed. Last error: {}",
        MAX_ATTEMPTS,
        last_error.unwrap_or_default()
    );
}

async fn run_live_task_attempt() -> Result<(), String> {
    use harness::mcp::handlers::{handle_assign_task, handle_get_result, handle_poll_task};
    use harness::task::TaskManager;
    use model::prelude::*;
    use std::sync::Arc;

    let ollama_config = OllamaConfig::default();
    let provider = OllamaProvider::new(ollama_config).map_err(|e| e.to_string())?;
    let provider: Arc<dyn ModelProvider> = Arc::new(provider);

    let task_manager = Arc::new(TaskManager::default());

    // Use /tmp as repo_path — the workspace creation will fail quickly
    // (no git repo), which is acceptable: we only need to prove the handlers
    // complete a full assign -> poll -> get_result round-trip and return
    // a well-formed terminal-state response.
    let assign_params = serde_json::json!({
        "description": "Print hello world",
        "repo_path": "/tmp",
        "model": LIVE_MODEL,
        "max_iterations": 3
    });

    let assign_result = handle_assign_task(
        &assign_params,
        &task_manager,
        &provider,
        LIVE_MODEL,
        3,
    )
    .await
    .map_err(|e| format!("assign_task failed: {e}"))?;

    let task_id = assign_result["task_id"]
        .as_str()
        .ok_or_else(|| "assign_task returned no task_id".to_string())?;

    let poll_params = serde_json::json!({ "task_id": task_id });

    // Poll until Completed or Failed.
    let terminal_statuses = ["Completed", "Failed"];
    let poll_interval = std::time::Duration::from_secs(2);
    let max_polls = 60usize;
    let mut reached_terminal = false;

    for _ in 0..max_polls {
        let poll_result = handle_poll_task(&poll_params, &task_manager)
            .await
            .map_err(|e| format!("poll_task failed: {e}"))?;

        let status = poll_result["status"].as_str().unwrap_or("Unknown");
        eprintln!("[smoke_mcp live] poll status: {status}");

        if terminal_statuses.contains(&status) {
            reached_terminal = true;
            break;
        }

        tokio::time::sleep(poll_interval).await;
    }

    if !reached_terminal {
        return Err(
            "task did not reach a terminal state within polling budget".to_string(),
        );
    }

    // get_result must return a well-formed response; for Completed tasks
    // also assert non-empty result_summary.
    let get_params = serde_json::json!({ "task_id": task_id });
    let get_result = handle_get_result(&get_params, &task_manager)
        .await
        .map_err(|e| format!("get_result failed: {e}"))?;

    let status = get_result["status"].as_str().unwrap_or("Unknown");
    eprintln!("[smoke_mcp live] terminal status: {status}");

    if status == "Completed" {
        let summary = get_result["result_summary"].as_str().unwrap_or("");
        if summary.is_empty() {
            return Err("Completed task has empty result_summary".to_string());
        }
    }

    Ok(())
}
