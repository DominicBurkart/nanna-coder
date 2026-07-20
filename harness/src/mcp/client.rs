//! A minimal MCP Tasks client.
//!
//! [`NannaMcpClient`] speaks the newline-delimited JSON-RPC 2.0 dialect of
//! [`super::NannaMcpServer`] over any async reader/writer pair — a child
//! process's stdio, or an in-process [`tokio::io::duplex`] channel. It
//! implements the requestor side of the MCP Tasks extension: augment a
//! `tools/call` with a `task` field, then poll `tasks/get` and fetch
//! `tasks/result`.

use super::jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// Fallback polling interval when the server does not advertise a `pollInterval`.
const DEFAULT_POLL_INTERVAL_MS: u64 = 2000;

#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("server closed the connection before responding")]
    UnexpectedEof,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i32, message: String },
}

pub struct NannaMcpClient<R, W> {
    reader: R,
    writer: W,
    next_id: i64,
}

impl<R, W> NannaMcpClient<R, W>
where
    R: AsyncBufReadExt + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            next_id: 0,
        }
    }

    fn alloc_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    /// Send a request and read responses until the one matching this id
    /// arrives, skipping notifications and unrelated responses.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpClientError> {
        let id = self.alloc_id();
        let req = JsonRpcRequest::call(serde_json::json!(id), method, params);
        let mut body = serde_json::to_vec(&req)?;
        body.push(b'\n');
        self.writer.write_all(&body).await?;
        self.writer.flush().await?;

        let want = Some(serde_json::json!(id));
        let mut line = String::new();
        line.clear();
        while self.reader.read_line(&mut line).await? != 0 {
            let trimmed = line.trim();
            // Blank keep-alive lines and responses addressed to other requests
            // (or notifications) are ignored by falling through to the next
            // read; only the response matching our id returns.
            if !trimmed.is_empty() {
                let resp: JsonRpcResponse = serde_json::from_str(trimmed)?;
                if resp.id == want {
                    return match resp.error {
                        Some(err) => Err(McpClientError::Rpc {
                            code: err.code,
                            message: err.message,
                        }),
                        None => resp.result.ok_or_else(|| {
                            McpClientError::Protocol("response had neither result nor error".into())
                        }),
                    };
                }
            }
            line.clear();
        }
        // The stream closed before our response arrived.
        Err(McpClientError::UnexpectedEof)
    }

    /// Send a notification (no response expected).
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), McpClientError> {
        let req = JsonRpcRequest::notification(method, params);
        let mut body = serde_json::to_vec(&req)?;
        body.push(b'\n');
        self.writer.write_all(&body).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Perform the MCP initialize handshake, declaring client-side task support.
    pub async fn initialize(&mut self) -> Result<Value, McpClientError> {
        let params = serde_json::json!({ "protocolVersion": "2025-11-25", "capabilities": { "tasks": { "list": {}, "cancel": {} } }, "clientInfo": { "name": "nanna-cli", "version": env!("CARGO_PKG_VERSION") } });
        let result = self.request("initialize", params).await?;
        self.notify("notifications/initialized", serde_json::json!({}))
            .await?;
        Ok(result)
    }

    /// Submit a task-augmented `assign_task` call and return the new task id.
    pub async fn submit_task(
        &mut self,
        arguments: Value,
        ttl_ms: Option<u64>,
    ) -> Result<String, McpClientError> {
        let task = match ttl_ms {
            Some(ttl) => serde_json::json!({ "ttl": ttl }),
            None => serde_json::json!({}),
        };
        // Kept on one line so coverage instrumentation attributes it correctly.
        #[rustfmt::skip]
        let args = serde_json::json!({ "name": "assign_task", "arguments": arguments, "task": task });
        let result = self.request("tools/call", args).await?;
        result["task"]["taskId"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| McpClientError::Protocol("CreateTaskResult missing task.taskId".into()))
    }

    /// Fetch current task status (`tasks/get`).
    pub async fn get(&mut self, task_id: &str) -> Result<Value, McpClientError> {
        self.request("tasks/get", serde_json::json!({ "taskId": task_id }))
            .await
    }

    /// Fetch the terminal result (`tasks/result`); blocks server-side until terminal.
    pub async fn result(&mut self, task_id: &str) -> Result<Value, McpClientError> {
        self.request("tasks/result", serde_json::json!({ "taskId": task_id }))
            .await
    }

    /// List all tasks (`tasks/list`).
    pub async fn list(&mut self) -> Result<Value, McpClientError> {
        self.request("tasks/list", serde_json::json!({})).await
    }

    /// Cancel a task (`tasks/cancel`).
    pub async fn cancel(&mut self, task_id: &str) -> Result<Value, McpClientError> {
        self.request("tasks/cancel", serde_json::json!({ "taskId": task_id }))
            .await
    }

    /// Poll `tasks/get` until the task reaches a terminal status (respecting the
    /// advertised `pollInterval`), then return the `tasks/result` payload.
    pub async fn wait_result(&mut self, task_id: &str) -> Result<Value, McpClientError> {
        loop {
            let task = self.get(task_id).await?;
            let status = task["status"].as_str().unwrap_or("");
            if matches!(status, "completed" | "failed" | "cancelled") {
                return self.result(task_id).await;
            }
            let poll_ms = task["pollInterval"]
                .as_u64()
                .unwrap_or(DEFAULT_POLL_INTERVAL_MS);
            tokio::time::sleep(Duration::from_millis(poll_ms)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::NannaMcpServer;
    use crate::task::TaskManager;
    use async_trait::async_trait;
    use model::provider::{ModelProvider, ModelResult};
    use model::types::{ChatRequest, ChatResponse, ModelInfo};
    use std::sync::Arc;

    struct NoopProvider;

    #[async_trait]
    impl ModelProvider for NoopProvider {
        async fn chat(&self, _: ChatRequest) -> ModelResult<ChatResponse> {
            unimplemented!()
        }
        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }
        fn provider_name(&self) -> &'static str {
            "noop"
        }
    }

    /// Spawn a server over one half of a duplex and return a client on the other.
    #[allow(clippy::type_complexity)]
    fn connected_client() -> NannaMcpClient<
        tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    > {
        // Zero-permit manager: submitted tasks stay queued and never call the
        // (unimplemented) provider.
        let server = Arc::new(NannaMcpServer::new(
            Arc::new(TaskManager::new(0)),
            Arc::new(NoopProvider),
            "qwen3:0.6b".to_string(),
            100,
        ));
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_side);
        tokio::spawn(async move {
            let _ = server
                .serve(tokio::io::BufReader::new(server_read), server_write)
                .await;
        });
        let (client_read, client_write) = tokio::io::split(client_side);
        NannaMcpClient::new(tokio::io::BufReader::new(client_read), client_write)
    }

    #[tokio::test]
    async fn test_client_initialize_reports_task_capability() {
        let mut client = connected_client();
        let init = client.initialize().await.unwrap();
        assert_eq!(init["protocolVersion"], "2025-11-25");
        assert!(init["capabilities"]["tasks"]["requests"]["tools"]["call"].is_object());
    }

    #[tokio::test]
    async fn test_client_submit_get_list_cancel_result_roundtrip() {
        let mut client = connected_client();
        client.initialize().await.unwrap();

        let task_id = client
            .submit_task(
                serde_json::json!({ "description": "d", "repo_path": "/tmp" }),
                Some(5000),
            )
            .await
            .unwrap();
        assert!(!task_id.is_empty());

        let got = client.get(&task_id).await.unwrap();
        assert_eq!(got["status"], "working");
        assert_eq!(got["ttl"], 5000);

        let listed = client.list().await.unwrap();
        assert_eq!(listed["tasks"].as_array().unwrap().len(), 1);

        let cancelled = client.cancel(&task_id).await.unwrap();
        assert_eq!(cancelled["status"], "cancelled");

        // wait_result returns immediately now that the task is terminal.
        let result = client.wait_result(&task_id).await.unwrap();
        assert_eq!(result["isError"], true);
    }

    #[tokio::test]
    async fn test_client_unknown_task_is_rpc_error() {
        let mut client = connected_client();
        client.initialize().await.unwrap();
        let err = client.get("nope").await.unwrap_err();
        match err {
            McpClientError::Rpc { code, .. } => assert_eq!(code, -32602),
            other => panic!("expected rpc error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_client_eof_before_response_is_unexpected_eof() {
        // Reader is immediately at EOF: the request writes fine but no response
        // ever arrives.
        let mut client = NannaMcpClient::new(
            tokio::io::BufReader::new(tokio::io::empty()),
            tokio::io::sink(),
        );
        let err = client.get("x").await.unwrap_err();
        assert!(matches!(err, McpClientError::UnexpectedEof));
    }

    /// Build a client whose reader replays canned response bytes (and whose
    /// writes are discarded) — for exercising response-parsing branches.
    fn canned_client(
        bytes: &'static [u8],
    ) -> NannaMcpClient<tokio::io::BufReader<&'static [u8]>, tokio::io::Sink> {
        NannaMcpClient::new(tokio::io::BufReader::new(bytes), tokio::io::sink())
    }

    #[tokio::test]
    async fn test_client_skips_unrelated_message_then_returns_match() {
        // A stray notification (no id) precedes the real id=1 response; the
        // client skips it and returns the matching result.
        let mut client = canned_client(
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/x\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"working\"}}\n",
        );
        let r = client.get("t").await.unwrap();
        assert_eq!(r["status"], "working");
    }

    #[tokio::test]
    async fn test_client_skips_blank_keepalive_line() {
        // A blank line precedes the real id=1 response and must be skipped.
        let mut client = canned_client(
            b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"working\"}}\n",
        );
        let r = client.get("t").await.unwrap();
        assert_eq!(r["status"], "working");
    }

    #[tokio::test]
    async fn test_client_response_without_result_or_error_is_protocol_error() {
        let mut client = canned_client(b"{\"jsonrpc\":\"2.0\",\"id\":1}\n");
        let err = client.get("t").await.unwrap_err();
        assert!(matches!(err, McpClientError::Protocol(_)));
    }

    #[tokio::test]
    async fn test_client_submit_missing_task_id_is_protocol_error() {
        // Also exercises the no-ttl (None) submit path.
        let mut client =
            canned_client(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"task\":{}}}\n");
        let err = client
            .submit_task(serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, McpClientError::Protocol(_)));
    }

    #[tokio::test]
    async fn test_client_wait_result_polls_until_terminal() {
        use crate::task::TaskId;
        // Zero permits: the task stays `working`, so wait_result takes the
        // poll-then-sleep path at least once before we cancel it out of band.
        let manager = Arc::new(TaskManager::new(0));
        let server = Arc::new(NannaMcpServer::new(
            Arc::clone(&manager),
            Arc::new(NoopProvider),
            "m".to_string(),
            10,
        ));
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server_side);
        tokio::spawn(async move {
            let _ = server.serve(tokio::io::BufReader::new(sr), sw).await;
        });
        let (cr, cw) = tokio::io::split(client_side);
        let mut client = NannaMcpClient::new(tokio::io::BufReader::new(cr), cw);
        client.initialize().await.unwrap();
        let task_id = client
            .submit_task(
                serde_json::json!({ "description": "d", "repo_path": "/tmp" }),
                None,
            )
            .await
            .unwrap();

        let m = Arc::clone(&manager);
        let tid = TaskId(task_id.clone());
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = m.cancel(&tid).await;
        });

        let result = client.wait_result(&task_id).await.unwrap();
        assert_eq!(result["isError"], true);
    }
}
