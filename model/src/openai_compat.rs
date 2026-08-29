use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
    ModelInfo, ToolCall, ToolDefinition, Usage,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, info};

/// A provider that speaks the OpenAI-compatible `/v1/chat/completions` API.
///
/// Works with vLLM, LiteLLM, OpenRouter, llama.cpp server, and any endpoint
/// that implements the OpenAI chat completion spec.
pub struct OpenAICompatProvider {
    base_url: String,
    api_key: Option<String>,
    http_client: reqwest::Client,
    default_model: String,
}

impl OpenAICompatProvider {
    pub fn new(
        base_url: impl Into<String>,
        api_key: Option<String>,
        default_model: impl Into<String>,
        timeout: Duration,
    ) -> ModelResult<Self> {
        let base_url = base_url.into();
        if base_url.is_empty() {
            return Err(ModelError::InvalidConfig {
                message: "Base URL cannot be empty".to_string(),
            });
        }
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err(ModelError::InvalidConfig {
                message: "Base URL must start with http:// or https://".to_string(),
            });
        }

        let base_url = base_url.trim_end_matches('/').to_string();

        let http_client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| ModelError::Unknown {
                message: format!("Failed to build HTTP client: {}", e),
            })?;

        Ok(Self {
            base_url,
            api_key,
            http_client,
            default_model: default_model.into(),
        })
    }

    fn messages_to_json(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .map(|msg| {
                let role = match &msg.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                };

                let mut obj = serde_json::json!({
                    "role": role,
                });

                // OpenAI spec: content can be null for assistant messages with tool_calls
                match &msg.content {
                    Some(c) => obj["content"] = Value::String(c.clone()),
                    None => obj["content"] = Value::Null,
                }

                if let Some(tool_calls) = &msg.tool_calls {
                    if !tool_calls.is_empty() {
                        let tc_json: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.function.name,
                                        "arguments": tc.function.arguments.to_string()
                                    }
                                })
                            })
                            .collect();
                        obj["tool_calls"] = Value::Array(tc_json);
                    }
                }

                if let Some(tool_call_id) = &msg.tool_call_id {
                    obj["tool_call_id"] = Value::String(tool_call_id.clone());
                }

                obj
            })
            .collect()
    }

    fn tools_to_json(tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": tool.function
                })
            })
            .collect()
    }

    fn parse_response(raw: OpenAIRawResponse) -> ModelResult<ChatResponse> {
        let choices = raw
            .choices
            .into_iter()
            .map(|c| {
                let tool_calls = c.message.tool_calls.map(|tcs| {
                    tcs.into_iter()
                        .map(|tc| ToolCall {
                            id: tc.id,
                            function: FunctionCall {
                                name: tc.function.name,
                                arguments: serde_json::from_str(&tc.function.arguments)
                                    .unwrap_or(Value::String(tc.function.arguments)),
                            },
                        })
                        .collect()
                });

                // `finish_reason` may be absent (null) or carry an unrecognised value from a
                // non-standard endpoint. Both map to `Stop` as a safe default. This behaviour
                // is explicitly tested by `test_response_finish_reason_null_becomes_stop`.
                let finish_reason = match c.finish_reason.as_deref() {
                    Some("tool_calls") => Some(FinishReason::ToolCalls),
                    Some("length") => Some(FinishReason::Length),
                    Some("content_filter") => Some(FinishReason::ContentFilter),
                    _ => Some(FinishReason::Stop),
                };

                Choice {
                    message: ChatMessage {
                        role: MessageRole::Assistant,
                        content: c.message.content,
                        tool_calls,
                        tool_call_id: None,
                    },
                    finish_reason,
                }
            })
            .collect();

        let usage = raw.usage.map(|u| Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });

        Ok(ChatResponse { choices, usage })
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(key) = &self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }
}

// ---------- OpenAI wire types (private) ----------

#[derive(Deserialize)]
struct OpenAIRawResponse {
    choices: Vec<OpenAIRawChoice>,
    usage: Option<OpenAIRawUsage>,
}

#[derive(Deserialize)]
struct OpenAIRawChoice {
    message: OpenAIRawMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAIRawMessage {
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIRawToolCall>>,
}

#[derive(Deserialize)]
struct OpenAIRawToolCall {
    id: String,
    function: OpenAIRawFunction,
}

#[derive(Deserialize)]
struct OpenAIRawFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct OpenAIRawUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[derive(Deserialize)]
struct OpenAIModelsResponse {
    data: Vec<OpenAIModelEntry>,
}

#[derive(Deserialize)]
struct OpenAIModelEntry {
    id: String,
}

// ---------- ModelProvider impl ----------

#[async_trait]
impl ModelProvider for OpenAICompatProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        let model = if request.model.is_empty() {
            &self.default_model
        } else {
            &request.model
        };
        debug!("OpenAI-compat chat request with model: {}", model);

        let messages = Self::messages_to_json(&request.messages);

        let mut payload = serde_json::json!({
            "model": model,
            "messages": messages,
        });

        if let Some(temp) = request.temperature {
            payload["temperature"] = serde_json::json!(temp);
        }
        if let Some(max) = request.max_tokens {
            payload["max_tokens"] = serde_json::json!(max);
        }

        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                payload["tools"] = Value::Array(Self::tools_to_json(tools));
            }
        }

        let url = format!("{}/v1/chat/completions", self.base_url);

        let req = self.http_client.post(&url).json(&payload);
        let req = self.apply_auth(req);

        let response = req.send().await.map_err(|e| {
            if e.is_timeout() {
                ModelError::ServiceUnavailable {
                    message: "Request timeout".to_string(),
                }
            } else if e.is_connect() {
                ModelError::ServiceUnavailable {
                    message: "Cannot connect to OpenAI-compatible service".to_string(),
                }
            } else {
                ModelError::Network(e)
            }
        })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ModelError::Authentication);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ModelError::RateLimit);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            // Truncate to avoid leaking large or sensitive response bodies
            // (some gateways reflect authentication context in error payloads).
            let body = if body.len() > 512 {
                &body[..512]
            } else {
                &body
            };
            return Err(ModelError::Unknown {
                message: format!("OpenAI-compat API error {}: {}", status, body),
            });
        }

        let raw: OpenAIRawResponse = response.json().await.map_err(|e| ModelError::Unknown {
            message: format!("Failed to parse response: {}", e),
        })?;

        info!("OpenAI-compat chat request completed");

        Self::parse_response(raw)
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        debug!("Listing models from OpenAI-compat endpoint");

        let url = format!("{}/v1/models", self.base_url);
        let req = self.http_client.get(&url);
        let req = self.apply_auth(req);

        let response = req.send().await.map_err(|e| {
            if e.is_connect() {
                ModelError::ServiceUnavailable {
                    message: "Cannot connect to OpenAI-compatible service".to_string(),
                }
            } else {
                ModelError::Network(e)
            }
        })?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ModelError::Unknown {
                message: format!("Failed to list models: {}", body),
            });
        }

        let raw: OpenAIModelsResponse = response.json().await.map_err(|e| ModelError::Unknown {
            message: format!("Failed to parse models response: {}", e),
        })?;

        Ok(raw
            .data
            .into_iter()
            .map(|m| ModelInfo {
                name: m.id,
                size: None,
                digest: None,
                modified_at: None,
            })
            .collect())
    }

    async fn health_check(&self) -> ModelResult<()> {
        debug!("Health check against OpenAI-compat endpoint");

        let url = format!("{}/v1/models", self.base_url);
        let req = self.http_client.get(&url);
        let req = self.apply_auth(req);

        let response = req.send().await.map_err(|e| {
            if e.is_connect() {
                ModelError::ServiceUnavailable {
                    message: "Cannot connect to OpenAI-compatible service".to_string(),
                }
            } else {
                ModelError::Network(e)
            }
        })?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(ModelError::ServiceUnavailable {
                message: format!("Health check returned status {}", response.status()),
            })
        }
    }

    fn provider_name(&self) -> &'static str {
        "openai-compat"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;

    #[test]
    fn test_request_body_serialization() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Hello"),
        ];
        let json_msgs = OpenAICompatProvider::messages_to_json(&messages);

        assert_eq!(json_msgs.len(), 2);
        assert_eq!(json_msgs[0]["role"], "system");
        assert_eq!(json_msgs[0]["content"], "You are helpful");
        assert_eq!(json_msgs[1]["role"], "user");
        assert_eq!(json_msgs[1]["content"], "Hello");
    }

    #[test]
    fn test_request_body_with_tool_calls() {
        let msg = ChatMessage::assistant_with_tools(
            None,
            vec![ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "read_file".to_string(),
                    arguments: serde_json::json!({"path": "/tmp/foo"}),
                },
            }],
        );
        let json_msgs = OpenAICompatProvider::messages_to_json(&[msg]);

        assert_eq!(json_msgs[0]["role"], "assistant");
        assert!(json_msgs[0]["content"].is_null());
        let tc = &json_msgs[0]["tool_calls"][0];
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "read_file");
        // OpenAI spec: arguments is a string
        assert!(tc["function"]["arguments"].is_string());
    }

    #[test]
    fn test_response_deserialization() {
        let raw_json = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help you?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "total_tokens": 18
            }
        }"#;

        let raw: OpenAIRawResponse = serde_json::from_str(raw_json).unwrap();
        let response = OpenAICompatProvider::parse_response(raw).unwrap();

        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].message.role, MessageRole::Assistant);
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("Hello! How can I help you?")
        );
        assert_eq!(response.choices[0].finish_reason, Some(FinishReason::Stop));

        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.total_tokens, 18);
    }

    #[test]
    fn test_response_with_tool_calls() {
        let raw_json = r#"{
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"/tmp/test\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": null
        }"#;

        let raw: OpenAIRawResponse = serde_json::from_str(raw_json).unwrap();
        let response = OpenAICompatProvider::parse_response(raw).unwrap();

        assert_eq!(
            response.choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        );
        let tool_calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_abc");
        assert_eq!(tool_calls[0].function.name, "read_file");
        assert_eq!(
            tool_calls[0].function.arguments,
            serde_json::json!({"path": "/tmp/test"})
        );
    }

    #[test]
    fn test_provider_constructor_validation() {
        let result = OpenAICompatProvider::new("", None, "gpt-4", Duration::from_secs(30));
        assert!(result.is_err());

        let result = OpenAICompatProvider::new("not-a-url", None, "gpt-4", Duration::from_secs(30));
        assert!(result.is_err());

        let result = OpenAICompatProvider::new(
            "http://localhost:8080",
            None,
            "gpt-4",
            Duration::from_secs(30),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_provider_name() {
        let provider = OpenAICompatProvider::new(
            "http://localhost:8080",
            None,
            "gpt-4",
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(provider.provider_name(), "openai-compat");
    }

    #[test]
    fn test_trailing_slash_stripped() {
        let provider = OpenAICompatProvider::new(
            "http://localhost:8080/",
            None,
            "gpt-4",
            Duration::from_secs(30),
        )
        .unwrap();
        assert_eq!(provider.base_url, "http://localhost:8080");
    }

    #[test]
    fn test_response_finish_reason_null_becomes_stop() {
        // Documents intentional behaviour: absent/null finish_reason maps to Stop.
        // Update this assertion if the mapping changes in future.
        let raw_json = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":null}],"usage":null}"#;
        let raw: OpenAIRawResponse = serde_json::from_str(raw_json).unwrap();
        let resp = OpenAICompatProvider::parse_response(raw).unwrap();
        assert_eq!(resp.choices[0].finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn test_response_finish_reason_unknown_string_becomes_stop() {
        // An unrecognised finish_reason from a non-standard endpoint also maps to Stop.
        let raw_json = r#"{"choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"unknown_future_value"}],"usage":null}"#;
        let raw: OpenAIRawResponse = serde_json::from_str(raw_json).unwrap();
        let resp = OpenAICompatProvider::parse_response(raw).unwrap();
        assert_eq!(resp.choices[0].finish_reason, Some(FinishReason::Stop));
    }

    // ---- HTTP integration tests (wiremock-backed) ----
    // Close the diff-coverage gap on async dispatch / error-mapping paths
    // that are unreachable without mocking the OpenAI-compatible server.

    use crate::provider::ModelError;
    use crate::types::{ChatRequest, FunctionDefinition, JsonSchema, SchemaType};
    use wiremock::matchers::{header, header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn provider_for(server: &MockServer, api_key: Option<&str>) -> OpenAICompatProvider {
        OpenAICompatProvider::new(
            server.uri(),
            api_key.map(String::from),
            "default-model",
            Duration::from_secs(5),
        )
        .unwrap()
    }

    fn simple_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: name.to_string(),
                description: "t".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: None,
                    required: None,
                },
            },
        }
    }

    #[tokio::test]
    async fn http_chat_happy_path_with_auth_temperature_and_max_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("Authorization", "Bearer sk-test-abc"))
            .and(wiremock::matchers::body_string_contains(
                "\"temperature\":0.2",
            ))
            .and(wiremock::matchers::body_string_contains(
                "\"max_tokens\":64",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hi"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 4, "completion_tokens": 2, "total_tokens": 6}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = provider_for(&server, Some("sk-test-abc"));
        let req = ChatRequest::new("gpt-4", vec![ChatMessage::user("hi")])
            .with_temperature(0.2)
            .with_max_tokens(64);
        let resp = provider.chat(req).await.unwrap();
        assert_eq!(resp.choices[0].message.content.as_deref(), Some("hi"));
        assert_eq!(resp.usage.unwrap().total_tokens, 6);
    }

    #[tokio::test]
    async fn http_chat_omits_authorization_when_no_key() {
        // Negative assertion: if provider sent Authorization, the 500 mock
        // fires (`expect(0)` panics on teardown).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header_exists("Authorization"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        provider_for(&server, None)
            .chat(ChatRequest::new("m", vec![ChatMessage::user("hi")]))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn http_chat_uses_default_model_when_request_model_empty() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::body_string_contains("default-model"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"choices":[{"message":{"role":"assistant","content":"x"},"finish_reason":"stop"}],"usage":null}"#,
            ))
            .expect(1)
            .mount(&server)
            .await;
        provider_for(&server, None)
            .chat(ChatRequest::new("", vec![ChatMessage::user("hi")]))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn http_chat_serializes_tools_array_and_returns_tool_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::body_string_contains("\"tools\""))
            .and(wiremock::matchers::body_string_contains("read_file"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant", "content": null,
                        "tool_calls": [{"id": "c1", "type": "function",
                            "function": {"name": "read_file", "arguments": "{\"path\":\"/a\"}"}}]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": null
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = provider_for(&server, None);
        let req = ChatRequest::new("m", vec![ChatMessage::user("read it")])
            .with_tools(vec![simple_tool("read_file")]);
        let tcs = provider.chat(req).await.unwrap().choices[0]
            .message
            .tool_calls
            .clone()
            .unwrap();
        assert_eq!(tcs[0].function.name, "read_file");
        assert_eq!(tcs[0].function.arguments, serde_json::json!({"path": "/a"}));
    }

    #[test]
    fn messages_to_json_covers_roles_tool_call_id_and_empty_tool_calls() {
        // Covers: all four MessageRole arms, tool_call_id branch, and the
        // "empty tool_calls -> omit field" path (assistant_with_tools(_, [])).
        let out = OpenAICompatProvider::messages_to_json(&[
            ChatMessage::system("sys"),
            ChatMessage::user("u"),
            ChatMessage::assistant_with_tools(
                None,
                vec![ToolCall {
                    id: "call_x".to_string(),
                    function: FunctionCall {
                        name: "fn_x".to_string(),
                        arguments: serde_json::json!({}),
                    },
                }],
            ),
            ChatMessage::tool_response("call_x", "result"),
            ChatMessage::assistant_with_tools(Some("text".to_string()), vec![]),
        ]);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[2]["role"], "assistant");
        assert!(out[2]["content"].is_null());
        assert_eq!(out[2]["tool_calls"][0]["id"], "call_x");
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["tool_call_id"], "call_x");
        // Empty tool_calls vec must not emit a `tool_calls` key.
        assert!(out[4].get("tool_calls").is_none());
    }

    async fn assert_chat_status_maps_to(server_status: u16, expected: &str) {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(server_status))
            .mount(&server)
            .await;
        let provider = provider_for(&server, Some("k"));
        let err = provider
            .chat(ChatRequest::new("m", vec![ChatMessage::user("hi")]))
            .await
            .unwrap_err();
        match (expected, &err) {
            ("auth", ModelError::Authentication) => {}
            ("rate", ModelError::RateLimit) => {}
            _ => panic!("status {} mapped to wrong error: {:?}", server_status, err),
        }
    }

    #[tokio::test]
    async fn http_chat_status_code_error_mapping() {
        // 401 and 403 -> Authentication; 429 -> RateLimit.
        assert_chat_status_maps_to(401, "auth").await;
        assert_chat_status_maps_to(403, "auth").await;
        assert_chat_status_maps_to(429, "rate").await;
    }

    #[tokio::test]
    async fn http_chat_5xx_returns_unknown_with_truncated_body() {
        // Short body is included; bodies > 512 chars are truncated to avoid
        // leaking large or sensitive response payloads.
        let server = MockServer::start().await;
        let head = "A".repeat(512);
        let body = format!("{}SENTINEL", head);
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string(body))
            .mount(&server)
            .await;

        let provider = provider_for(&server, None);
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        let err = provider.chat(req).await.unwrap_err();
        let ModelError::Unknown { message } = err else {
            panic!("expected Unknown error");
        };
        assert!(message.contains("503"));
        assert!(!message.contains("SENTINEL"), "body was not truncated");
    }

    #[tokio::test]
    async fn http_chat_malformed_json_returns_unknown_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
            .mount(&server)
            .await;
        let err = provider_for(&server, None)
            .chat(ChatRequest::new("m", vec![ChatMessage::user("hi")]))
            .await
            .unwrap_err();
        let ModelError::Unknown { message } = err else {
            panic!("expected Unknown");
        };
        assert!(message.contains("Failed to parse response"));
    }

    #[tokio::test]
    async fn http_url_parse_error_maps_to_network_on_all_endpoints() {
        // Bad URL -> reqwest error that is neither timeout nor connect, so the
        // default `ModelError::Network(e)` arm fires on every endpoint.
        let bad = OpenAICompatProvider::new("http://[invalid", None, "m", Duration::from_secs(2))
            .unwrap();
        assert!(matches!(
            bad.chat(ChatRequest::new("m", vec![ChatMessage::user("h")]))
                .await
                .unwrap_err(),
            ModelError::Network(_)
        ));
        assert!(matches!(
            bad.list_models().await.unwrap_err(),
            ModelError::Network(_)
        ));
        assert!(matches!(
            bad.health_check().await.unwrap_err(),
            ModelError::Network(_)
        ));
    }

    #[tokio::test]
    async fn http_connect_failure_maps_to_service_unavailable_on_all_endpoints() {
        // Closed port -> reqwest `is_connect()` -> ServiceUnavailable on every endpoint.
        let p = OpenAICompatProvider::new("http://127.0.0.1:1", None, "m", Duration::from_secs(2))
            .unwrap();
        assert!(matches!(
            p.chat(ChatRequest::new("m", vec![ChatMessage::user("h")]))
                .await
                .unwrap_err(),
            ModelError::ServiceUnavailable { .. }
        ));
        assert!(matches!(
            p.list_models().await.unwrap_err(),
            ModelError::ServiceUnavailable { .. }
        ));
        assert!(matches!(
            p.health_check().await.unwrap_err(),
            ModelError::ServiceUnavailable { .. }
        ));
    }

    #[tokio::test]
    async fn http_chat_timeout_returns_service_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(5))
                    .set_body_json(serde_json::json!({"choices": [], "usage": null})),
            )
            .mount(&server)
            .await;

        let provider =
            OpenAICompatProvider::new(server.uri(), None, "m", Duration::from_millis(150)).unwrap();
        let req = ChatRequest::new("m", vec![ChatMessage::user("hi")]);
        let err = provider.chat(req).await.unwrap_err();
        let ModelError::ServiceUnavailable { message } = err else {
            panic!("expected ServiceUnavailable");
        };
        assert!(message.to_lowercase().contains("timeout"));
    }

    #[tokio::test]
    async fn http_list_models_round_trip_with_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("Authorization", "Bearer sk-list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "model-a"}, {"id": "model-b"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let provider = provider_for(&server, Some("sk-list"));
        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].name, "model-a");
        assert!(models[0].size.is_none() && models[0].digest.is_none());
    }

    #[tokio::test]
    async fn http_list_models_error_status_returns_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let provider = provider_for(&server, None);
        let err = provider.list_models().await.unwrap_err();
        let ModelError::Unknown { message } = err else {
            panic!("expected Unknown");
        };
        assert!(message.contains("boom"));
    }

    #[tokio::test]
    async fn http_list_models_malformed_json_returns_unknown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
            .mount(&server)
            .await;

        let provider = provider_for(&server, None);
        let err = provider.list_models().await.unwrap_err();
        let ModelError::Unknown { message } = err else {
            panic!("expected Unknown");
        };
        assert!(message.contains("Failed to parse models response"));
    }

    #[tokio::test]
    async fn http_health_check_success() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})))
            .expect(1)
            .mount(&server)
            .await;

        let provider = provider_for(&server, None);
        provider.health_check().await.unwrap();
    }

    #[tokio::test]
    async fn http_health_check_non_2xx_returns_service_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let provider = provider_for(&server, None);
        let err = provider.health_check().await.unwrap_err();
        let ModelError::ServiceUnavailable { message } = err else {
            panic!("expected ServiceUnavailable");
        };
        assert!(message.contains("503"));
    }

    // ---- GatewayConfig::build_provider dispatch tests ----

    #[test]
    fn gateway_build_dispatch_and_config_validation() {
        use crate::config::{GatewayConfig, OllamaConfig, OpenAICompatConfig};

        // Default config is sensible and validates.
        let cfg = OpenAICompatConfig::default();
        assert_eq!(cfg.base_url, "http://localhost:8080");
        assert!(cfg.api_key.is_none());
        assert_eq!(cfg.default_model, "default");
        assert_eq!(cfg.timeout, Duration::from_secs(30));
        assert!(cfg.validate().is_ok());

        // Zero timeout is rejected.
        let bad_to = OpenAICompatConfig {
            timeout: Duration::from_secs(0),
            ..OpenAICompatConfig::default()
        };
        assert!(bad_to.validate().unwrap_err().contains("Timeout"));

        // build_provider: invalid config -> InvalidConfig.
        let bad = OpenAICompatConfig {
            base_url: "".to_string(),
            ..OpenAICompatConfig::default()
        };
        let result = GatewayConfig::OpenaiCompat(bad).build_provider();
        let err = result.err().expect("expected InvalidConfig error");
        assert!(matches!(err, ModelError::InvalidConfig { .. }));

        // build_provider: valid OpenAI-compat -> dispatches to that provider.
        let cfg = OpenAICompatConfig {
            base_url: "http://localhost:9999".to_string(),
            api_key: Some("k".to_string()),
            default_model: "m".to_string(),
            timeout: Duration::from_secs(1),
        };
        let p = GatewayConfig::OpenaiCompat(cfg).build_provider().unwrap();
        assert_eq!(p.provider_name(), "openai-compat");

        // build_provider: Ollama variant -> dispatches to OllamaProvider.
        let p = GatewayConfig::Ollama(OllamaConfig::default())
            .build_provider()
            .unwrap();
        assert_eq!(p.provider_name(), "ollama");
    }
}
