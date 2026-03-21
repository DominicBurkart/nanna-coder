pub mod handlers;

use crate::task::TaskManager;
use model::provider::ModelProvider;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

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
    _model_guard: Option<crate::container::ModelGuard>,
}

impl NannaMcpServer {
    pub fn new(
        task_manager: Arc<TaskManager>,
        provider: Arc<dyn ModelProvider>,
        default_model: String,
        default_max_iterations: usize,
        model_guard: Option<crate::container::ModelGuard>,
    ) -> Self {
        Self {
            task_manager,
            provider,
            default_model,
            default_max_iterations,
            _model_guard: model_guard,
        }
    }

    pub async fn run_stdio(self) -> Result<(), Box<dyn std::error::Error>> {
        self.run_on(tokio::io::stdin(), tokio::io::stdout())
            .await
            .map_err(|e| e as Box<dyn std::error::Error>)
    }

    pub async fn run_on<R, W>(
        self,
        reader: R,
        writer: W,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let mut reader = BufReader::new(reader);
        let mut writer = writer;

        loop {
            let mut header_buf = String::new();
            let mut content_length: Option<usize> = None;

            loop {
                header_buf.clear();
                let bytes_read = reader.read_line(&mut header_buf).await?;
                if bytes_read == 0 {
                    return Ok(());
                }
                let line = header_buf.trim_end_matches(['\r', '\n']);
                if line.is_empty() {
                    break;
                }
                if let Some(rest) = line.strip_prefix("Content-Length: ") {
                    content_length = rest.trim().parse().ok();
                }
            }

            let content_length = match content_length {
                Some(n) => n,
                None => continue,
            };

            let mut body = vec![0u8; content_length];
            tokio::io::AsyncReadExt::read_exact(&mut reader, &mut body).await?;

            let response = match serde_json::from_slice::<JsonRpcRequest>(&body) {
                Ok(req) => self.handle_request(req).await,
                Err(e) => JsonRpcResponse::error(None, -32700, format!("Parse error: {}", e)),
            };

            if response.id.is_none() && response.error.is_none() {
                continue;
            }

            let body = serde_json::to_vec(&response)?;
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            writer.write_all(header.as_bytes()).await?;
            writer.write_all(&body).await?;
            writer.flush().await?;
        }
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
            Arc::new(TaskManager::new()),
            Arc::new(NoopProvider),
            "qwen3:0.6b".to_string(),
            100,
            None,
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
    async fn test_tools_list_returns_three_tools() {
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
        assert_eq!(tools.as_array().unwrap().len(), 3);
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"assign_task"));
        assert!(names.contains(&"poll_task"));
        assert!(names.contains(&"get_result"));
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

    async fn send_framed(
        writer: &mut (impl tokio::io::AsyncWrite + Unpin),
        msg: serde_json::Value,
    ) {
        let body = serde_json::to_vec(&msg).unwrap();
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        tokio::io::AsyncWriteExt::write_all(writer, header.as_bytes())
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::write_all(writer, &body)
            .await
            .unwrap();
        tokio::io::AsyncWriteExt::flush(writer).await.unwrap();
    }

    async fn recv_framed(
        reader: &mut tokio::io::BufReader<impl tokio::io::AsyncRead + Unpin>,
    ) -> serde_json::Value {
        let mut content_length: Option<usize> = None;
        let mut header_buf = String::new();
        loop {
            header_buf.clear();
            reader.read_line(&mut header_buf).await.unwrap();
            let line = header_buf.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some(rest) = line.strip_prefix("Content-Length: ") {
                content_length = rest.trim().parse().ok();
            }
        }
        let n = content_length.expect("no Content-Length header");
        let mut body = vec![0u8; n];
        tokio::io::AsyncReadExt::read_exact(reader, &mut body)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn test_stdio_initialize_round_trip() {
        let (client, server_io) = tokio::io::duplex(8192);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server_task = tokio::spawn(make_server().run_on(server_read, server_write));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_reader = tokio::io::BufReader::new(client_read);

        send_framed(
            &mut client_write,
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await;

        let resp = recv_framed(&mut client_reader).await;
        assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");
        assert!(resp["result"]["capabilities"]["tools"].is_object());

        server_task.abort();
    }

    #[tokio::test]
    async fn test_stdio_tools_list_round_trip() {
        let (client, server_io) = tokio::io::duplex(8192);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server_task = tokio::spawn(make_server().run_on(server_read, server_write));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_reader = tokio::io::BufReader::new(client_read);

        send_framed(
            &mut client_write,
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        )
        .await;

        let resp = recv_framed(&mut client_reader).await;
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"assign_task"));
        assert!(names.contains(&"poll_task"));
        assert!(names.contains(&"get_result"));

        server_task.abort();
    }

    #[tokio::test]
    async fn test_stdio_eof_clean_shutdown() {
        let (client, server_io) = tokio::io::duplex(8192);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server_task = tokio::spawn(make_server().run_on(server_read, server_write));
        drop(client);
        server_task.await.unwrap().unwrap();
    }
}
