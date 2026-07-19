pub mod client;
pub mod handlers;
pub mod jsonrpc;

use crate::task::{TaskId, TaskManager};
use jsonrpc::{JsonRpcRequest, JsonRpcResponse};
use model::provider::ModelProvider;
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

pub struct NannaMcpServer {
    task_manager: Arc<TaskManager>,
    provider: Arc<dyn ModelProvider>,
    default_model: String,
    default_max_iterations: usize,
}

impl NannaMcpServer {
    pub fn new(
        task_manager: Arc<TaskManager>,
        provider: Arc<dyn ModelProvider>,
        default_model: String,
        default_max_iterations: usize,
    ) -> Self {
        Self {
            task_manager,
            provider,
            default_model,
            default_max_iterations,
        }
    }

    /// Serve the MCP protocol over a reader/writer pair.
    ///
    /// Each request is handled on its own spawned task and responses are
    /// serialized through an mpsc channel to a single writer task. This lets a
    /// blocking `tasks/result` (which waits for the task to reach a terminal
    /// state) run without stalling concurrent `tasks/get` / `tasks/cancel`
    /// requests from the same client.
    pub async fn serve<R, W>(
        self: Arc<Self>,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        let writer_task = tokio::spawn(async move {
            // Drain and write responses in order. Write errors mean the client
            // has gone away; keep draining until all senders drop so the loop
            // still terminates cleanly (the failed writes are simply ignored).
            while let Some(bytes) = rx.recv().await {
                let _ = writer.write_all(&bytes).await;
                let _ = writer.flush().await;
            }
        });

        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                break;
            }
            let this = Arc::clone(&self);
            let tx = tx.clone();
            let owned = line.clone();
            tokio::spawn(async move {
                if let Ok(Some(response_bytes)) = this.process_line(&owned).await {
                    let _ = tx.send(response_bytes);
                }
            });
        }

        drop(tx);
        let _ = writer_task.await;
        Ok(())
    }

    async fn process_line(
        &self,
        line: &str,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(req) => self.handle_request(req).await,
            Err(e) => JsonRpcResponse::error(None, -32700, format!("Parse error: {}", e)),
        };

        if response.id.is_none() && response.error.is_none() {
            return Ok(None);
        }

        let mut body = serde_json::to_vec(&response)?;
        body.push(b'\n');
        Ok(Some(body))
    }

    async fn handle_request(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        if req.jsonrpc != "2.0" {
            return JsonRpcResponse::error(req.id, -32600, "Invalid JSON-RPC version".to_string());
        }

        let params = req.params.unwrap_or(Value::Object(Default::default()));

        match req.method.as_str() {
            "initialize" => JsonRpcResponse::success(
                req.id,
                serde_json::json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": {},
                        "tasks": {
                            "list": {},
                            "cancel": {},
                            "requests": { "tools": { "call": {} } }
                        }
                    },
                    "serverInfo": {
                        "name": "nanna",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            ),
            "notifications/initialized" | "initialized" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: None,
                result: None,
                error: None,
            },
            "tools/list" => JsonRpcResponse::success(
                req.id,
                serde_json::json!({
                    "tools": self.tool_list()
                }),
            ),
            "tools/call" => self.handle_tools_call(req.id, &params).await,
            "tasks/get" => self.handle_tasks_get(req.id, &params).await,
            "tasks/result" => self.handle_tasks_result(req.id, &params).await,
            "tasks/list" => self.handle_tasks_list(req.id).await,
            "tasks/cancel" => self.handle_tasks_cancel(req.id, &params).await,
            _ => {
                JsonRpcResponse::error(req.id, -32601, format!("Method not found: {}", req.method))
            }
        }
    }

    fn tool_list(&self) -> Value {
        serde_json::json!([
            {
                "name": "assign_task",
                "description": "Submit a coding task to be executed asynchronously in an isolated git worktree. This tool is task-augmented: clients MUST invoke it with a `task` field (MCP Tasks extension) and retrieve the result via tasks/result.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "description": {
                            "type": "string",
                            "description": "Description of the task to perform"
                        },
                        "repo_path": {
                            "type": "string",
                            "description": "Absolute path to the git repository"
                        },
                        "branch": {
                            "type": "string",
                            "description": "Branch or ref to base the worktree on (default: HEAD)"
                        },
                        "model": {
                            "type": "string",
                            "description": "Model name to use (default: server default)"
                        },
                        "max_iterations": {
                            "type": "integer",
                            "description": "Maximum agent iterations (default: server default)"
                        }
                    },
                    "required": ["description", "repo_path"]
                },
                "execution": { "taskSupport": "required" }
            },
            {
                "name": "onboard_repo",
                "description": "Generate a flake.nix for a pure Cargo Rust project that has none",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "repo_path": {
                            "type": "string",
                            "description": "Absolute path to the repository to onboard"
                        }
                    },
                    "required": ["repo_path"]
                }
            }
        ])
    }

    async fn handle_tools_call(&self, id: Option<Value>, params: &Value) -> JsonRpcResponse {
        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(name) => name.to_string(),
            None => {
                return JsonRpcResponse::error(id, -32602, "Missing tool name".to_string());
            }
        };

        let tool_params = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        // A request is task-augmented when it carries a `task` field in params.
        let task_field = params.get("task");
        let requested_ttl = task_field
            .and_then(|t| t.get("ttl"))
            .and_then(|v| v.as_u64());

        match tool_name.as_str() {
            // `assign_task` is `taskSupport: "required"`: it MUST be invoked as
            // a task. A non-task-augmented call gets -32601 per the spec.
            "assign_task" => {
                if task_field.is_none() {
                    return JsonRpcResponse::error(
                        id,
                        -32601,
                        "Tool 'assign_task' requires task augmentation (include a `task` field). \
                         Retrieve results via tasks/result."
                            .to_string(),
                    );
                }
                match handlers::handle_assign_task(
                    &tool_params,
                    &self.task_manager,
                    &self.provider,
                    &self.default_model,
                    self.default_max_iterations,
                    requested_ttl,
                )
                .await
                {
                    Ok(task_id) => match self.task_manager.poll(&task_id).await {
                        Some(task) => JsonRpcResponse::success(
                            id,
                            serde_json::json!({ "task": handlers::task_to_wire(&task) }),
                        ),
                        None => JsonRpcResponse::error(
                            id,
                            -32603,
                            "Task vanished immediately after creation".to_string(),
                        ),
                    },
                    Err(msg) => JsonRpcResponse::error(id, -32602, msg),
                }
            }
            // `onboard_repo` does not support task augmentation
            // (taskSupport defaults to "forbidden").
            "onboard_repo" => {
                if task_field.is_some() {
                    return JsonRpcResponse::error(
                        id,
                        -32601,
                        "Tool 'onboard_repo' does not support task augmentation".to_string(),
                    );
                }
                match handlers::handle_onboard_repo(&tool_params).await {
                    Ok(value) => JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "content": [{
                                "type": "text",
                                "text": serde_json::to_string_pretty(&value).unwrap_or_default()
                            }],
                            "isError": false
                        }),
                    ),
                    Err(msg) => JsonRpcResponse::error(id, -32603, msg),
                }
            }
            other => JsonRpcResponse::error(id, -32602, format!("Unknown tool: {}", other)),
        }
    }

    /// Extract a `taskId` string param, or produce a `-32602` error response.
    fn task_id_param(id: &Option<Value>, params: &Value) -> Result<TaskId, JsonRpcResponse> {
        match params.get("taskId").and_then(|v| v.as_str()) {
            Some(s) => Ok(TaskId(s.to_string())),
            None => Err(JsonRpcResponse::error(
                id.clone(),
                -32602,
                "Missing required param: taskId".to_string(),
            )),
        }
    }

    /// `tasks/get` — return current task status (no result payload).
    async fn handle_tasks_get(&self, id: Option<Value>, params: &Value) -> JsonRpcResponse {
        let task_id = match Self::task_id_param(&id, params) {
            Ok(t) => t,
            Err(resp) => return resp,
        };
        match self.task_manager.poll(&task_id).await {
            Some(task) => JsonRpcResponse::success(id, handlers::task_to_wire(&task)),
            None => JsonRpcResponse::error(
                id,
                -32602,
                format!("Failed to retrieve task: Task not found: {}", task_id),
            ),
        }
    }

    /// `tasks/result` — block until the task is terminal, then return the
    /// underlying `CallToolResult`.
    async fn handle_tasks_result(&self, id: Option<Value>, params: &Value) -> JsonRpcResponse {
        let task_id = match Self::task_id_param(&id, params) {
            Ok(t) => t,
            Err(resp) => return resp,
        };
        if self.task_manager.wait_terminal(&task_id).await.is_none() {
            return JsonRpcResponse::error(
                id,
                -32602,
                format!("Failed to retrieve task: Task not found: {}", task_id),
            );
        }
        match self.task_manager.poll(&task_id).await {
            Some(task) => {
                JsonRpcResponse::success(id, handlers::task_result_to_call_tool_result(&task))
            }
            None => JsonRpcResponse::error(
                id,
                -32602,
                format!("Failed to retrieve task: Task not found: {}", task_id),
            ),
        }
    }

    /// `tasks/list` — return all tasks (v1 returns the full set, no pagination).
    async fn handle_tasks_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let tasks = self.task_manager.list().await;
        let wire: Vec<Value> = tasks.iter().map(handlers::task_to_wire).collect();
        JsonRpcResponse::success(id, serde_json::json!({ "tasks": wire }))
    }

    /// `tasks/cancel` — cancel a task; already-terminal tasks yield -32602.
    async fn handle_tasks_cancel(&self, id: Option<Value>, params: &Value) -> JsonRpcResponse {
        let task_id = match Self::task_id_param(&id, params) {
            Ok(t) => t,
            Err(resp) => return resp,
        };
        // Distinguish "not found" from "already terminal": both surface as
        // -32602 per the spec, but with different messages.
        match self.task_manager.cancel(&task_id).await {
            Ok(task) => JsonRpcResponse::success(id, handlers::task_to_wire(&task)),
            Err(msg) => JsonRpcResponse::error(id, -32602, msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskManager;
    use async_trait::async_trait;
    use model::provider::ModelResult;
    use model::types::{ChatRequest, ChatResponse, ModelInfo};

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

    fn make_server() -> NannaMcpServer {
        NannaMcpServer::new(
            Arc::new(TaskManager::default()),
            Arc::new(NoopProvider),
            "qwen3:0.6b".to_string(),
            100,
        )
    }

    #[tokio::test]
    async fn test_initialize_returns_capabilities() {
        let server = make_server();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(1)),
            method: "initialize".to_string(),
            params: None,
        };
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["protocolVersion"], "2025-11-25");
        // Tasks capability is advertised per the MCP Tasks extension.
        assert!(result["capabilities"]["tasks"]["list"].is_object());
        assert!(result["capabilities"]["tasks"]["cancel"].is_object());
        assert!(result["capabilities"]["tasks"]["requests"]["tools"]["call"].is_object());
    }

    #[tokio::test]
    async fn test_tools_list_returns_two_tools() {
        let server = make_server();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(2)),
            method: "tools/list".to_string(),
            params: None,
        };
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_none());
        let tools = &resp.result.unwrap()["tools"];
        let arr = tools.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let names: Vec<&str> = arr.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"assign_task"));
        assert!(names.contains(&"onboard_repo"));
        // assign_task must declare task augmentation as required.
        let assign = arr
            .iter()
            .find(|t| t["name"] == "assign_task")
            .expect("assign_task present");
        assert_eq!(assign["execution"]["taskSupport"], "required");
        // onboard_repo does not declare task support (forbidden by default).
        let onboard = arr
            .iter()
            .find(|t| t["name"] == "onboard_repo")
            .expect("onboard_repo present");
        assert!(onboard.get("execution").is_none());
    }

    #[tokio::test]
    async fn test_unknown_method_returns_error() {
        let server = make_server();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(3)),
            method: "unknown/method".to_string(),
            params: None,
        };
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_tools_call_unknown_tool() {
        let server = make_server();
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(4)),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({
                "name": "nonexistent_tool",
                "arguments": {}
            })),
        };
        let resp = server.handle_request(req).await;
        assert!(resp.error.is_some());
    }

    fn parse_response(bytes: &[u8]) -> serde_json::Value {
        let s = std::str::from_utf8(bytes).unwrap();
        assert!(s.ends_with('\n'), "response must end with newline: {s}");
        serde_json::from_str(s.trim_end()).unwrap()
    }

    #[tokio::test]
    async fn test_process_line_initialize_returns_single_line_json() {
        let line = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"t\",\"version\":\"0\"}}}\n";
        let bytes = make_server().process_line(line).await.unwrap().unwrap();
        let v = parse_response(&bytes);
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["protocolVersion"], "2025-11-25");
        assert!(v["result"]["capabilities"]["tools"].is_object());
        assert!(!std::str::from_utf8(&bytes)
            .unwrap()
            .contains("Content-Length"));
    }

    #[tokio::test]
    async fn test_process_line_tools_list_returns_newline_terminated() {
        let bytes = make_server()
            .process_line("{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
            .await
            .unwrap()
            .unwrap();
        let v = parse_response(&bytes);
        assert_eq!(v["id"], 2);
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_process_line_notification_produces_none() {
        let out = make_server()
            .process_line("{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn test_process_line_blank_returns_none() {
        assert!(make_server().process_line("").await.unwrap().is_none());
        assert!(make_server().process_line("   \n").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_process_line_invalid_json_returns_parse_error() {
        let bytes = make_server()
            .process_line("not-json-at-all\n")
            .await
            .unwrap()
            .unwrap();
        let v = parse_response(&bytes);
        assert_eq!(v["error"]["code"], -32700);
        assert!(v["id"].is_null());
    }

    /// Drive `serve` over an in-memory duplex: feed `input` bytes, close the
    /// write side, and collect everything the server writes back.
    async fn serve_over_duplex(input: &[u8]) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let server = Arc::new(make_server());
        let (mut client_side, server_side) = tokio::io::duplex(64 * 1024);
        let (server_read, server_write) = tokio::io::split(server_side);
        let serve_handle = tokio::spawn(async move {
            let _ = server
                .serve(tokio::io::BufReader::new(server_read), server_write)
                .await;
        });
        client_side.write_all(input).await.unwrap();
        // `shutdown` closes only the write direction of the duplex, signalling
        // EOF to the server's reader while leaving the read direction open so
        // we can still collect responses. (Dropping a split `WriteHalf` would
        // not close it, since the `ReadHalf` keeps the stream alive.)
        client_side.shutdown().await.unwrap();
        let mut out = Vec::new();
        client_side.read_to_end(&mut out).await.unwrap();
        serve_handle.await.unwrap();
        out
    }

    #[tokio::test]
    async fn test_serve_drives_process_line_over_async_io() {
        // Responses are produced concurrently, so assert on the id set rather
        // than positional order.
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n";
        let output = serve_over_duplex(input).await;
        let text = std::str::from_utf8(&output).unwrap();
        let mut ids: Vec<i64> = text
            .split('\n')
            .filter(|s| !s.is_empty())
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["id"]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec![1, 2]);
    }

    #[tokio::test]
    async fn test_serve_returns_cleanly_on_eof_with_no_input() {
        let output = serve_over_duplex(b"").await;
        assert!(output.is_empty());
    }

    fn tools_call(id: i64, params: serde_json::Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(id)),
            method: "tools/call".to_string(),
            params: Some(params),
        }
    }

    fn method_call(id: i64, method: &str, params: serde_json::Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(id)),
            method: method.to_string(),
            params: Some(params),
        }
    }

    #[tokio::test]
    async fn test_assign_task_without_task_augmentation_is_method_not_found() {
        let server = make_server();
        let resp = server
            .handle_request(tools_call(
                10,
                serde_json::json!({
                    "name": "assign_task",
                    "arguments": { "description": "d", "repo_path": "/tmp" }
                }),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_assign_task_with_task_returns_create_task_result() {
        let server = make_server();
        // repo_path "/tmp" is not a git repo, so the worker fails at workspace
        // creation (no model call), but the immediate CreateTaskResult is
        // returned synchronously with status "working".
        let resp = server
            .handle_request(tools_call(
                11,
                serde_json::json!({
                    "name": "assign_task",
                    "arguments": { "description": "d", "repo_path": "/tmp" },
                    "task": { "ttl": 5000 }
                }),
            ))
            .await;
        assert!(resp.error.is_none());
        let task = &resp.result.unwrap()["task"];
        assert_eq!(task["status"], "working");
        assert_eq!(task["ttl"], 5000);
        assert!(task["taskId"].is_string());
        assert_eq!(task["pollInterval"], handlers::POLL_INTERVAL_MS);
    }

    #[tokio::test]
    async fn test_onboard_repo_rejects_task_augmentation() {
        let server = make_server();
        let resp = server
            .handle_request(tools_call(
                12,
                serde_json::json!({
                    "name": "onboard_repo",
                    "arguments": { "repo_path": "/tmp" },
                    "task": {}
                }),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn test_tools_call_missing_name_is_invalid_params() {
        let server = make_server();
        let resp = server
            .handle_request(tools_call(13, serde_json::json!({ "arguments": {} })))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn test_tasks_get_unknown_id_is_invalid_params() {
        let server = make_server();
        let resp = server
            .handle_request(method_call(
                14,
                "tasks/get",
                serde_json::json!({ "taskId": "nope" }),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn test_tasks_get_missing_task_id_is_invalid_params() {
        let server = make_server();
        let resp = server
            .handle_request(method_call(15, "tasks/get", serde_json::json!({})))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn test_tasks_result_unknown_id_is_invalid_params() {
        let server = make_server();
        let resp = server
            .handle_request(method_call(
                16,
                "tasks/result",
                serde_json::json!({ "taskId": "nope" }),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn test_tasks_cancel_unknown_id_is_invalid_params() {
        let server = make_server();
        let resp = server
            .handle_request(method_call(
                17,
                "tasks/cancel",
                serde_json::json!({ "taskId": "nope" }),
            ))
            .await;
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn test_tasks_list_empty_then_populated() {
        // Zero-permit manager so the submitted task stays queued (non-terminal)
        // and never calls the (unimplemented) NoopProvider.
        let server = NannaMcpServer::new(
            Arc::new(TaskManager::new(0)),
            Arc::new(NoopProvider),
            "qwen3:0.6b".to_string(),
            100,
        );
        let empty = server
            .handle_request(method_call(18, "tasks/list", serde_json::json!({})))
            .await;
        assert_eq!(empty.result.unwrap()["tasks"].as_array().unwrap().len(), 0);

        let created = server
            .handle_request(tools_call(
                19,
                serde_json::json!({
                    "name": "assign_task",
                    "arguments": { "description": "d", "repo_path": "/tmp" },
                    "task": {}
                }),
            ))
            .await;
        let task_id = created.result.unwrap()["task"]["taskId"]
            .as_str()
            .unwrap()
            .to_string();

        // tasks/get reflects the working task; ttl is null (none requested).
        let got = server
            .handle_request(method_call(
                20,
                "tasks/get",
                serde_json::json!({ "taskId": task_id }),
            ))
            .await;
        let task = got.result.unwrap();
        assert_eq!(task["status"], "working");
        assert!(task["ttl"].is_null());

        let listed = server
            .handle_request(method_call(21, "tasks/list", serde_json::json!({})))
            .await;
        assert_eq!(listed.result.unwrap()["tasks"].as_array().unwrap().len(), 1);

        // tasks/cancel transitions it to cancelled and returns the task.
        let cancelled = server
            .handle_request(method_call(
                22,
                "tasks/cancel",
                serde_json::json!({ "taskId": task_id }),
            ))
            .await;
        assert_eq!(cancelled.result.unwrap()["status"], "cancelled");
    }

    #[tokio::test]
    async fn test_tasks_result_returns_terminal_call_tool_result() {
        // Cancel a queued task, then tasks/result must return the terminal
        // CallToolResult (isError true for a cancelled task) without blocking.
        let server = NannaMcpServer::new(
            Arc::new(TaskManager::new(0)),
            Arc::new(NoopProvider),
            "qwen3:0.6b".to_string(),
            100,
        );
        let created = server
            .handle_request(tools_call(
                23,
                serde_json::json!({
                    "name": "assign_task",
                    "arguments": { "description": "d", "repo_path": "/tmp" },
                    "task": {}
                }),
            ))
            .await;
        let task_id = created.result.unwrap()["task"]["taskId"]
            .as_str()
            .unwrap()
            .to_string();

        server
            .handle_request(method_call(
                24,
                "tasks/cancel",
                serde_json::json!({ "taskId": task_id }),
            ))
            .await;

        let result = server
            .handle_request(method_call(
                25,
                "tasks/result",
                serde_json::json!({ "taskId": task_id }),
            ))
            .await;
        let body = result.result.unwrap();
        assert_eq!(body["isError"], true);
        assert_eq!(
            body["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
            task_id.as_str()
        );
    }
}
