use crate::anthropic_config::AnthropicConfig;
use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
    ModelInfo, ToolCall, ToolChoice, ToolDefinition, Usage,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, error, info, warn};

pub struct AnthropicProvider {
    config: AnthropicConfig,
    http_client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig) -> ModelResult<Self> {
        config
            .validate()
            .map_err(|msg| ModelError::InvalidConfig { message: msg })?;

        let http_client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| ModelError::Unknown {
                message: format!("Failed to build HTTP client: {}", e),
            })?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Extract system messages from the message list.
    /// Anthropic requires system messages as a top-level parameter, not in the messages array.
    fn extract_system(messages: &[ChatMessage]) -> Option<String> {
        let system_parts: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .filter_map(|m| m.content.as_deref())
            .collect();

        if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        }
    }

    /// Convert internal ChatMessages to Anthropic API message format.
    /// System messages are excluded (handled separately via extract_system).
    /// Tool response messages are mapped to user messages with tool_result content blocks.
    fn messages_to_anthropic(messages: &[ChatMessage]) -> Vec<Value> {
        messages
            .iter()
            .filter(|m| m.role != MessageRole::System)
            .map(|msg| match &msg.role {
                MessageRole::Tool => {
                    // Anthropic: tool results are sent as user messages with tool_result content blocks
                    serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": msg.tool_call_id.as_deref().unwrap_or(""),
                            "content": msg.content.as_deref().unwrap_or("")
                        }]
                    })
                }
                MessageRole::Assistant => {
                    if let Some(tool_calls) = &msg.tool_calls {
                        if !tool_calls.is_empty() {
                            let mut content_blocks: Vec<Value> = Vec::new();

                            if let Some(text) = &msg.content {
                                if !text.is_empty() {
                                    content_blocks.push(serde_json::json!({
                                        "type": "text",
                                        "text": text
                                    }));
                                }
                            }

                            for tc in tool_calls {
                                content_blocks.push(serde_json::json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "input": tc.function.arguments
                                }));
                            }

                            serde_json::json!({
                                "role": "assistant",
                                "content": content_blocks
                            })
                        } else {
                            serde_json::json!({
                                "role": "assistant",
                                "content": msg.content.as_deref().unwrap_or("")
                            })
                        }
                    } else {
                        serde_json::json!({
                            "role": "assistant",
                            "content": msg.content.as_deref().unwrap_or("")
                        })
                    }
                }
                MessageRole::User => {
                    serde_json::json!({
                        "role": "user",
                        "content": msg.content.as_deref().unwrap_or("")
                    })
                }
                MessageRole::System => unreachable!("System messages are filtered out"),
            })
            .collect()
    }

    /// Convert internal ToolDefinitions to Anthropic tool format.
    fn tools_to_anthropic(tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.function.name,
                    "description": tool.function.description,
                    "input_schema": {
                        "type": serde_json::to_value(&tool.function.parameters.schema_type)
                            .unwrap_or(Value::String("object".to_string())),
                        "properties": tool.function.parameters.properties,
                        "required": tool.function.parameters.required
                    }
                })
            })
            .collect()
    }

    /// Convert internal ToolChoice to Anthropic tool_choice format.
    fn tool_choice_to_anthropic(tool_choice: &ToolChoice) -> Value {
        match tool_choice {
            ToolChoice::Auto => serde_json::json!({"type": "auto"}),
            ToolChoice::None => serde_json::json!({"type": "none"}),
            ToolChoice::Required => serde_json::json!({"type": "any"}),
            ToolChoice::Specific(name) => {
                serde_json::json!({"type": "tool", "name": name})
            }
        }
    }

    /// Map Anthropic stop_reason to internal FinishReason.
    fn map_stop_reason(stop_reason: &str) -> FinishReason {
        match stop_reason {
            "end_turn" => FinishReason::Stop,
            "tool_use" => FinishReason::ToolCalls,
            "max_tokens" => FinishReason::Length,
            "content_filter" => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        }
    }

    /// Parse Anthropic API response into internal ChatResponse.
    fn parse_response(raw: AnthropicRawResponse) -> ModelResult<ChatResponse> {
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in &raw.content {
            match block.block_type.as_str() {
                "text" => {
                    if let Some(text) = &block.text {
                        text_parts.push(text.clone());
                    }
                }
                "tool_use" => {
                    if let (Some(id), Some(name), Some(input)) =
                        (&block.id, &block.name, &block.input)
                    {
                        tool_calls.push(ToolCall {
                            id: id.clone(),
                            function: FunctionCall {
                                name: name.clone(),
                                arguments: input.clone(),
                            },
                        });
                    }
                }
                _ => {
                    debug!("Unknown content block type: {}", block.block_type);
                }
            }
        }

        let finish_reason = Self::map_stop_reason(&raw.stop_reason.unwrap_or_default());

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let tool_calls_opt = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        };

        let message = ChatMessage {
            role: MessageRole::Assistant,
            content,
            tool_calls: tool_calls_opt,
            tool_call_id: None,
        };

        let usage = Some(Usage {
            prompt_tokens: raw.usage.input_tokens,
            completion_tokens: raw.usage.output_tokens,
            total_tokens: raw.usage.input_tokens + raw.usage.output_tokens,
        });

        Ok(ChatResponse {
            choices: vec![Choice {
                message,
                finish_reason: Some(finish_reason),
            }],
            usage,
        })
    }

    /// Send a request with retry logic for rate limiting (429 status).
    async fn send_with_retry(&self, payload: &Value) -> ModelResult<AnthropicRawResponse> {
        let url = format!("{}/v1/messages", self.config.base_url);
        let mut last_error: Option<ModelError> = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay = std::time::Duration::from_millis(500 * 2u64.pow(attempt - 1));
                warn!(
                    "Rate limited, retrying in {:?} (attempt {}/{})",
                    delay, attempt, self.config.max_retries
                );
                tokio::time::sleep(delay).await;
            }

            let response = self
                .http_client
                .post(&url)
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", &self.config.api_key)
                .header("content-type", "application/json")
                .json(payload)
                .send()
                .await
                .map_err(Self::handle_reqwest_error)?;

            let status = response.status();

            if status.is_success() {
                let raw: AnthropicRawResponse =
                    response.json().await.map_err(|e| ModelError::Unknown {
                        message: format!("Failed to parse Anthropic response: {}", e),
                    })?;
                return Ok(raw);
            }

            if status.as_u16() == 429 {
                last_error = Some(ModelError::RateLimit);
                continue;
            }

            if status.as_u16() == 401 {
                return Err(ModelError::Authentication);
            }

            let body = response.text().await.unwrap_or_default();

            if status.as_u16() == 404 {
                return Err(ModelError::ModelNotFound {
                    model: payload
                        .get("model")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                });
            }

            return Err(ModelError::Unknown {
                message: format!("Anthropic API error {}: {}", status, body),
            });
        }

        Err(last_error.unwrap_or(ModelError::RateLimit))
    }

    fn handle_reqwest_error(e: reqwest::Error) -> ModelError {
        if e.is_timeout() {
            ModelError::ServiceUnavailable {
                message: "Request timeout".to_string(),
            }
        } else if e.is_connect() {
            ModelError::ServiceUnavailable {
                message: "Cannot connect to Anthropic API".to_string(),
            }
        } else {
            ModelError::Unknown {
                message: format!("Network error: {}", e),
            }
        }
    }
}

// Anthropic API response types

#[derive(Debug, Deserialize)]
struct AnthropicRawResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        debug!("Starting Anthropic chat request with model: {}", request.model);

        let system = Self::extract_system(&request.messages);
        let messages = Self::messages_to_anthropic(&request.messages);

        let max_tokens = request.max_tokens.unwrap_or(4096);

        let mut payload = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": max_tokens
        });

        if let Some(system_text) = system {
            payload["system"] = Value::String(system_text);
        }

        if let Some(temperature) = request.temperature {
            payload["temperature"] = serde_json::json!(temperature);
        }

        if let Some(tools) = &request.tools {
            if !tools.is_empty() {
                payload["tools"] = Value::Array(Self::tools_to_anthropic(tools));
            }
        }

        if let Some(tool_choice) = &request.tool_choice {
            payload["tool_choice"] = Self::tool_choice_to_anthropic(tool_choice);
        }

        let raw = self.send_with_retry(&payload).await?;

        info!("Anthropic chat request completed successfully");

        Self::parse_response(raw)
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        debug!("Listing known Anthropic models");

        let models = vec![
            ModelInfo {
                name: "claude-sonnet-4-20250514".to_string(),
                size: None,
                digest: None,
                modified_at: None,
            },
            ModelInfo {
                name: "claude-opus-4-20250514".to_string(),
                size: None,
                digest: None,
                modified_at: None,
            },
            ModelInfo {
                name: "claude-haiku-4-20250514".to_string(),
                size: None,
                digest: None,
                modified_at: None,
            },
            ModelInfo {
                name: "claude-3-5-sonnet-20241022".to_string(),
                size: None,
                digest: None,
                modified_at: None,
            },
            ModelInfo {
                name: "claude-3-5-haiku-20241022".to_string(),
                size: None,
                digest: None,
                modified_at: None,
            },
            ModelInfo {
                name: "claude-3-opus-20240229".to_string(),
                size: None,
                digest: None,
                modified_at: None,
            },
        ];

        info!("Returned {} known Anthropic models", models.len());
        Ok(models)
    }

    async fn health_check(&self) -> ModelResult<()> {
        debug!("Performing Anthropic health check");

        // Send a minimal messages request to verify connectivity and auth
        let payload = serde_json::json!({
            "model": self.config.model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 1
        });

        let url = format!("{}/v1/messages", self.config.base_url);

        let response = self
            .http_client
            .post(&url)
            .header("anthropic-version", "2023-06-01")
            .header("x-api-key", &self.config.api_key)
            .header("content-type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(Self::handle_reqwest_error)?;

        let status = response.status();

        if status.is_success() {
            info!("Anthropic health check passed");
            Ok(())
        } else if status.as_u16() == 401 {
            error!("Anthropic health check failed: authentication error");
            Err(ModelError::Authentication)
        } else {
            let body = response.text().await.unwrap_or_default();
            error!("Anthropic health check failed: {} {}", status, body);
            Err(ModelError::ServiceUnavailable {
                message: format!("Anthropic API returned {}: {}", status, body),
            })
        }
    }

    fn provider_name(&self) -> &'static str {
        "anthropic"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition,
    };
    use std::collections::HashMap;

    fn make_config() -> AnthropicConfig {
        AnthropicConfig::new().with_api_key("sk-test-key-for-unit-tests")
    }

    #[test]
    fn test_messages_to_anthropic_basic() {
        let messages = vec![
            ChatMessage::user("Hello"),
            ChatMessage::assistant("Hi there!"),
            ChatMessage::user("How are you?"),
        ];

        let result = AnthropicProvider::messages_to_anthropic(&messages);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0]["role"], "user");
        assert_eq!(result[0]["content"], "Hello");
        assert_eq!(result[1]["role"], "assistant");
        assert_eq!(result[1]["content"], "Hi there!");
        assert_eq!(result[2]["role"], "user");
        assert_eq!(result[2]["content"], "How are you?");
    }

    #[test]
    fn test_messages_to_anthropic_system_extraction() {
        let messages = vec![
            ChatMessage::system("You are a helpful assistant."),
            ChatMessage::system("Always be concise."),
            ChatMessage::user("Hello"),
        ];

        let system = AnthropicProvider::extract_system(&messages);
        assert_eq!(
            system,
            Some("You are a helpful assistant.\n\nAlways be concise.".to_string())
        );

        let result = AnthropicProvider::messages_to_anthropic(&messages);
        // System messages should be filtered out
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");
    }

    #[test]
    fn test_messages_to_anthropic_tool_results() {
        let messages = vec![ChatMessage::tool_response("toolu_123", "The weather is sunny")];

        let result = AnthropicProvider::messages_to_anthropic(&messages);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "user");

        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "toolu_123");
        assert_eq!(content[0]["content"], "The weather is sunny");
    }

    #[test]
    fn test_tools_to_anthropic_format() {
        let mut props = HashMap::new();
        props.insert(
            "location".to_string(),
            PropertySchema {
                schema_type: SchemaType::String,
                description: Some("The city name".to_string()),
                items: None,
            },
        );

        let tool = ToolDefinition {
            function: FunctionDefinition {
                name: "get_weather".to_string(),
                description: "Get weather for a location".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some(props),
                    required: Some(vec!["location".to_string()]),
                },
            },
        };

        let result = AnthropicProvider::tools_to_anthropic(&[tool]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "get_weather");
        assert_eq!(result[0]["description"], "Get weather for a location");
        assert_eq!(result[0]["input_schema"]["type"], "object");
        assert!(result[0]["input_schema"]["properties"]["location"].is_object());
        assert_eq!(result[0]["input_schema"]["required"][0], "location");
    }

    #[test]
    fn test_parse_response_text() {
        let raw = AnthropicRawResponse {
            content: vec![ContentBlock {
                block_type: "text".to_string(),
                text: Some("Hello! How can I help you today?".to_string()),
                id: None,
                name: None,
                input: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 8,
            },
        };

        let response = AnthropicProvider::parse_response(raw).unwrap();

        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content,
            Some("Hello! How can I help you today?".to_string())
        );
        assert_eq!(response.choices[0].finish_reason, Some(FinishReason::Stop));
        assert!(response.choices[0].message.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_tool_use() {
        let raw = AnthropicRawResponse {
            content: vec![
                ContentBlock {
                    block_type: "text".to_string(),
                    text: Some("Let me check the weather.".to_string()),
                    id: None,
                    name: None,
                    input: None,
                },
                ContentBlock {
                    block_type: "tool_use".to_string(),
                    text: None,
                    id: Some("toolu_01A".to_string()),
                    name: Some("get_weather".to_string()),
                    input: Some(serde_json::json!({"location": "Paris"})),
                },
            ],
            stop_reason: Some("tool_use".to_string()),
            usage: AnthropicUsage {
                input_tokens: 15,
                output_tokens: 25,
            },
        };

        let response = AnthropicProvider::parse_response(raw).unwrap();

        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].finish_reason,
            Some(FinishReason::ToolCalls)
        );
        assert_eq!(
            response.choices[0].message.content,
            Some("Let me check the weather.".to_string())
        );

        let tool_calls = response.choices[0].message.tool_calls.as_ref().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "toolu_01A");
        assert_eq!(tool_calls[0].function.name, "get_weather");
        assert_eq!(tool_calls[0].function.arguments["location"], "Paris");
    }

    #[test]
    fn test_parse_response_usage() {
        let raw = AnthropicRawResponse {
            content: vec![ContentBlock {
                block_type: "text".to_string(),
                text: Some("Hi".to_string()),
                id: None,
                name: None,
                input: None,
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 100,
                output_tokens: 50,
            },
        };

        let response = AnthropicProvider::parse_response(raw).unwrap();
        let usage = response.usage.unwrap();

        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
        assert_eq!(usage.total_tokens, 150);
    }

    #[test]
    fn test_stop_reason_mapping() {
        assert_eq!(
            AnthropicProvider::map_stop_reason("end_turn"),
            FinishReason::Stop
        );
        assert_eq!(
            AnthropicProvider::map_stop_reason("tool_use"),
            FinishReason::ToolCalls
        );
        assert_eq!(
            AnthropicProvider::map_stop_reason("max_tokens"),
            FinishReason::Length
        );
        assert_eq!(
            AnthropicProvider::map_stop_reason("content_filter"),
            FinishReason::ContentFilter
        );
        // Unknown reason defaults to Stop
        assert_eq!(
            AnthropicProvider::map_stop_reason("unknown_reason"),
            FinishReason::Stop
        );
    }

    #[test]
    fn test_config_from_env() {
        let config = AnthropicConfig::default();
        // Without ANTHROPIC_API_KEY set, it defaults to empty string
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn test_config_validation() {
        // Empty API key should fail
        let config = AnthropicConfig::new().with_api_key("");
        assert!(config.validate().is_err());

        // Valid config should pass
        let config = make_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_provider_creation() {
        let config = make_config();
        let provider = AnthropicProvider::new(config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "anthropic");
    }

    #[test]
    fn test_provider_creation_invalid_config() {
        let config = AnthropicConfig::new().with_api_key("");
        let provider = AnthropicProvider::new(config);
        assert!(provider.is_err());
    }

    #[test]
    fn test_assistant_with_tool_calls_to_anthropic() {
        let msg = ChatMessage::assistant_with_tools(
            Some("I'll check that.".to_string()),
            vec![ToolCall {
                id: "toolu_abc".to_string(),
                function: FunctionCall {
                    name: "search".to_string(),
                    arguments: serde_json::json!({"query": "rust"}),
                },
            }],
        );

        let result = AnthropicProvider::messages_to_anthropic(&[msg]);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["role"], "assistant");

        let content = result[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "I'll check that.");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_abc");
        assert_eq!(content[1]["name"], "search");
    }

    #[test]
    fn test_tool_choice_mapping() {
        assert_eq!(
            AnthropicProvider::tool_choice_to_anthropic(&ToolChoice::Auto),
            serde_json::json!({"type": "auto"})
        );
        assert_eq!(
            AnthropicProvider::tool_choice_to_anthropic(&ToolChoice::None),
            serde_json::json!({"type": "none"})
        );
        assert_eq!(
            AnthropicProvider::tool_choice_to_anthropic(&ToolChoice::Required),
            serde_json::json!({"type": "any"})
        );
        assert_eq!(
            AnthropicProvider::tool_choice_to_anthropic(&ToolChoice::Specific(
                "get_weather".to_string()
            )),
            serde_json::json!({"type": "tool", "name": "get_weather"})
        );
    }

    #[tokio::test]
    async fn test_list_models() {
        let config = make_config();
        let provider = AnthropicProvider::new(config).unwrap();
        let models = provider.list_models().await.unwrap();
        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m.name.contains("claude")));
    }

    #[test]
    fn test_extract_system_none() {
        let messages = vec![ChatMessage::user("Hello")];
        assert!(AnthropicProvider::extract_system(&messages).is_none());
    }
}
