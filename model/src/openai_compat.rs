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
}
