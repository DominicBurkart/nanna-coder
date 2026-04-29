pub mod handlers;

use crate::task::TaskManager;
use model::provider::ModelProvider;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

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

    pub async fn serve<R, W>(
        &self,
        mut reader: R,
        mut writer: W,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        R: tokio::io::AsyncBufRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).await?;
            if bytes_read == 0 {
                return Ok(());
            }

            // Detect Content-Length framing: if the first line of a message is a
            // Content-Length header, consume the rest of the header block (an
            // empty line), then read exactly N bytes of JSON body. Responses are
            // written with matching Content-Length framing so framed clients can
            // parse them. Otherwise we fall through to the line-delimited path
            // used by the existing unit tests.
            let trimmed_hdr = line.trim_end_matches(['\r', '\n']);
            if trimmed_hdr
                .to_ascii_lowercase()
                .starts_with("content-length:")
            {
                let content_length: usize = trimmed_hdr
                    .split_once(':')
                    .and_then(|(_, v)| v.trim().parse().ok())
                    .unwrap_or(0);

                // Consume header lines until we hit an empty line (end of headers).
                loop {
                    line.clear();
                    let n = reader.read_line(&mut line).await?;
                    if n == 0 {
                        return Ok(());
                    }
                    if line.trim_end_matches(['\r', '\n']).is_empty() {
                        break;
                    }
                }

                let mut body = vec![0u8; content_length];
                reader.read_exact(&mut body).await?;
                let body_str = String::from_utf8_lossy(&body).into_owned();

                if let Some(response_bytes) = self.process_framed(&body_str).await? {
                    writer.write_all(&response_bytes).await?;
                    writer.flush().await?;
                }
                continue;
            }

            if let Some(response_bytes) = self.process_line(&line).await? {
                writer.write_all(&response_bytes).await?;
                writer.flush().await?;
            }
        }
    }

    /// Process a single JSON-RPC message body (already extracted from
    /// Content-Length framing) and return a Content-Length framed response.
    async fn process_framed(
        &self,
        body: &str,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
        let trimmed = body.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let response = match serde_json::from_str::<JsonRpcRequest>(trimmed) {
            Ok(req) => self.handle_request(req).await,
            Err(e) => JsonRpcResponse::error(None, -32700, format!("Parse error: {e}")),
        };

        if response.id.is_none() && response.error.is_none() {
            return Ok(None);
        }

        let json = serde_json::to_vec(&response)?;
        let header = format!("Content-Length: {}\r\n\r\n", json.len());
        let mut out = Vec::with_capacity(header.len() + json.len());
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(&json);
        Ok(Some(out))
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
            Err(e) => JsonRpcResponse::error(None, -32700, format!("Parse error: {e}")),
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
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
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
            _ => {
                JsonRpcResponse::error(req.id, -32601, format!("Method not found: {}", req.method))
            }
        }
    }

    fn tool_list(&self) -> Value {
        serde_json::json!([
            {
                "name": "assign_task",
                "description": "Submit a coding task to be executed asynchronously in an isolated git worktree",
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
                }
            },
            {
                "name": "poll_task",
                "description": "Check the current status of a submitted task",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "The task ID returned by assign_task"
                        }
                    },
                    "required": ["task_id"]
                }
            },
            {
                "name": "get_result",
                "description": "Retrieve the final result of a completed or failed task",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "The task ID returned by assign_task"
                        }
                    },
                    "required": ["task_id"]
                }
            },
            {
                "name": "list_tasks",
                "description": "List all submitted tasks with their current status",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "cancel_task",
                "description": "Cancel a pending or running task",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "The task ID returned by assign_task"
                        }
                    },
                    "required": ["task_id"]
                }
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
            Some(name) => name,
            None => {
                return JsonRpcResponse::error(id, -32602, "Missing tool name".to_string());
            }
        };

        let tool_params = params
            .get("arguments")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));

        let result = match tool_name {
            "assign_task" => {
                handlers::handle_assign_task(
                    &tool_params,
                    &self.task_manager,
                    &self.provider,
                    &self.default_model,
                    self.default_max_iterations,
                )
                .await
            }
            "poll_task" => handlers::handle_poll_task(&tool_params, &self.task_manager).await,
            "get_result" => handlers::handle_get_result(&tool_params, &self.task_manager).await,
            "list_tasks" => handlers::handle_list_tasks(&self.task_manager).await,
            "cancel_task" => handlers::handle_cancel_task(&tool_params, &self.task_manager).await,
            "onboard_repo" => handlers::handle_onboard_repo(&tool_params).await,
            other => Err(format!("Unknown tool: {}", other)),
        };

        match result {
            Ok(value) => JsonRpcResponse::success(
                id,
                serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&value).unwrap_or_default()
                    }]
                }),
            ),
            Err(msg) => JsonRpcResponse::error(id, -32603, msg),
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
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[tokio::test]
    async fn test_tools_list_returns_six_tools() {
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
        assert_eq!(tools.as_array().unwrap().len(), 6);
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"assign_task"));
        assert!(names.contains(&"poll_task"));
        assert!(names.contains(&"get_result"));
        assert!(names.contains(&"list_tasks"));
        assert!(names.contains(&"cancel_task"));
        assert!(names.contains(&"onboard_repo"));
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
        let line = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"t\",\"version\":\"0\"}}}\n";
        let bytes = make_server().process_line(line).await.unwrap().unwrap();
        let v = parse_response(&bytes);
        assert_eq!(v["id"], 1);
        assert_eq!(v["result"]["protocolVersion"], "2024-11-05");
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
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 6);
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

    #[tokio::test]
    async fn test_serve_drives_process_line_over_async_io() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n".to_vec();
        let mut output: Vec<u8> = Vec::new();
        make_server()
            .serve(std::io::Cursor::new(input), &mut output)
            .await
            .unwrap();
        let text = std::str::from_utf8(&output).unwrap();
        let lines: Vec<&str> = text.split('\n').filter(|s| !s.is_empty()).collect();
        assert_eq!(lines.len(), 2);
        let v0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let v1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v0["id"], 1);
        assert_eq!(v1["id"], 2);
    }

    #[tokio::test]
    async fn test_serve_returns_cleanly_on_eof_with_no_input() {
        let mut output: Vec<u8> = Vec::new();
        make_server()
            .serve(std::io::Cursor::new(Vec::<u8>::new()), &mut output)
            .await
            .unwrap();
        assert!(output.is_empty());
    }
}
