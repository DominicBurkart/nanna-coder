//! Anthropic provider implementing the [`crate::provider::ModelProvider`]
//! trait against the `/v1/messages` API.
//!
//! The provider translates between the workspace's OpenAI-shaped
//! [`crate::types::ChatRequest`] / [`crate::types::ChatResponse`] and
//! Anthropic's content-block-oriented wire format. The translation is
//! pure (no I/O) and is unit-tested separately from the HTTP transport.
//!
//! # Setup
//!
//! ```no_run
//! use model::anthropic::AnthropicProvider;
//! use model::provider::ModelProvider;
//! use model::types::{ChatMessage, ChatRequest};
//!
//! # async fn run() -> model::provider::ModelResult<()> {
//! let provider = AnthropicProvider::from_env()?;
//! let request = ChatRequest::new(
//!     "claude-opus-4-7",
//!     vec![ChatMessage::user("Say hi in one word.")],
//! );
//! let response = provider.chat(request).await?;
//! println!("{:?}", response.choices[0].message.content);
//! # Ok(())
//! # }
//! ```

use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
    ModelInfo, ToolCall, ToolChoice, ToolDefinition, Usage,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Default Anthropic API base URL. Overridable for tests / proxies.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Anthropic's required API version header.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default `max_tokens` when [`ChatRequest::max_tokens`] is not set.
/// Anthropic requires the field; we pick a generous-but-bounded default
/// so callers do not have to think about it.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Provider implementation for Anthropic's Messages API.
///
/// Constructed once and reused across requests; the underlying
/// [`reqwest::Client`] holds a connection pool. The API key is validated
/// once at construction (must be ASCII-printable so it can become an HTTP
/// header value); subsequent requests cannot panic on header assembly.
pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: reqwest::header::HeaderValue,
    base_url: String,
}

impl AnthropicProvider {
    /// Construct from the `ANTHROPIC_API_KEY` env var.
    pub fn from_env() -> ModelResult<Self> {
        let api_key =
            std::env::var("ANTHROPIC_API_KEY").map_err(|_| ModelError::InvalidConfig {
                message: "ANTHROPIC_API_KEY env var not set".to_string(),
            })?;
        Self::with_api_key(api_key)
    }

    /// Construct with an explicit API key. Use [`AnthropicProvider::from_env`]
    /// for the common case. Returns [`ModelError::InvalidConfig`] if the
    /// key contains characters that cannot appear in an HTTP header value
    /// (e.g. a stray BOM, newline, or non-ASCII byte).
    pub fn with_api_key(api_key: impl Into<String>) -> ModelResult<Self> {
        let raw = api_key.into();
        let mut header = reqwest::header::HeaderValue::from_str(&raw).map_err(|_| {
            ModelError::InvalidConfig {
                message: "ANTHROPIC_API_KEY contains characters that cannot be sent as an HTTP \
                          header value (non-ASCII / control / BOM)"
                    .to_string(),
            }
        })?;
        header.set_sensitive(true);
        Ok(Self {
            client: reqwest::Client::new(),
            api_key: header,
            base_url: DEFAULT_BASE_URL.to_string(),
        })
    }

    /// Override the base URL, e.g. for tests or proxies.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-api-key", self.api_key.clone());
        h.insert(
            "anthropic-version",
            reqwest::header::HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        h.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        h
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        let body = translate_request(request);
        let url = format!("{}/v1/messages", self.base_url);
        let resp = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let bytes = resp.bytes().await?;

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ModelError::Authentication);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(ModelError::RateLimit);
        }
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE {
            return Err(ModelError::ServiceUnavailable {
                message: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        if !status.is_success() {
            // Try to parse the structured error first; fall back to raw body.
            if let Ok(err) = serde_json::from_slice::<AnthropicErrorEnvelope>(&bytes) {
                let detail = err.error;
                if detail.error_type == "not_found_error" {
                    // Heuristic: Anthropic returns 404 for unknown models.
                    return Err(ModelError::ModelNotFound {
                        model: body.model.clone(),
                    });
                }
                return Err(ModelError::Unknown {
                    message: format!("{}: {}", detail.error_type, detail.message),
                });
            }
            return Err(ModelError::Unknown {
                message: format!(
                    "anthropic api returned {} — body: {}",
                    status,
                    String::from_utf8_lossy(&bytes)
                ),
            });
        }

        let parsed: AnthropicResponse = serde_json::from_slice(&bytes)?;
        Ok(translate_response(parsed))
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        let url = format!("{}/v1/models", self.base_url);
        let resp = self
            .client
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ModelError::Authentication);
        }
        if !status.is_success() {
            return Err(ModelError::Unknown {
                message: format!("list_models returned {status}"),
            });
        }
        let body: AnthropicModelList = resp.json().await?;
        Ok(body
            .data
            .into_iter()
            .map(|m| ModelInfo {
                name: m.id,
                size: None,
                digest: None,
                modified_at: m.created_at,
            })
            .collect())
    }

    async fn health_check(&self) -> ModelResult<()> {
        // Cheapest check: GET /v1/models. Validates auth and reachability
        // without spending tokens.
        self.list_models().await.map(|_| ())
    }

    fn provider_name(&self) -> &'static str {
        "anthropic"
    }
}

// -------------------------------------------------------------------------
// Wire-format types — kept private; only the translation functions cross
// the module boundary.
// -------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq)]
struct AnthropicRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
}

#[derive(Debug, Serialize, PartialEq)]
struct AnthropicMessage {
    role: String, // "user" | "assistant"
    content: Vec<AnthropicContent>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContent {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize, PartialEq)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    role: String,
    content: Vec<AnthropicResponseBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicResponseBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorEnvelope {
    error: AnthropicErrorDetail,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetail {
    #[serde(rename = "type")]
    error_type: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelList {
    data: Vec<AnthropicModelInfo>,
}

#[derive(Debug, Deserialize)]
struct AnthropicModelInfo {
    id: String,
    created_at: Option<String>,
}

// -------------------------------------------------------------------------
// Translation
// -------------------------------------------------------------------------

fn translate_request(request: ChatRequest) -> AnthropicRequest {
    let ChatRequest {
        model,
        messages,
        tools,
        tool_choice,
        temperature,
        max_tokens,
    } = request;

    // Lift system messages to the top-level `system` field. Anthropic
    // accepts a single system string; if multiple System messages exist
    // we concatenate with newlines.
    let mut system_parts: Vec<String> = Vec::new();
    let mut non_system: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for msg in messages {
        match msg.role {
            MessageRole::System => {
                if let Some(content) = msg.content {
                    if !content.is_empty() {
                        system_parts.push(content);
                    }
                }
            }
            _ => non_system.push(msg),
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    // Walk remaining messages, merging consecutive same-role messages so
    // that a chain like {assistant tool_use, tool result, tool result} ends
    // up as {assistant: [tool_use], user: [tool_result, tool_result]} —
    // Anthropic rejects two adjacent same-role messages.
    let mut anth_msgs: Vec<AnthropicMessage> = Vec::new();
    let mut current_role: Option<&'static str> = None;
    let mut current_blocks: Vec<AnthropicContent> = Vec::new();

    for msg in non_system {
        let target_role = match msg.role {
            MessageRole::User | MessageRole::Tool => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::System => unreachable!("system messages were partitioned out above"),
        };
        let blocks = message_to_blocks(msg);
        if blocks.is_empty() {
            continue;
        }
        match current_role {
            Some(r) if r == target_role => {
                current_blocks.extend(blocks);
            }
            _ => {
                if let Some(r) = current_role.take() {
                    anth_msgs.push(AnthropicMessage {
                        role: r.to_string(),
                        content: std::mem::take(&mut current_blocks),
                    });
                }
                current_role = Some(target_role);
                current_blocks = blocks;
            }
        }
    }
    if let Some(r) = current_role {
        anth_msgs.push(AnthropicMessage {
            role: r.to_string(),
            content: current_blocks,
        });
    }

    let tools: Option<Vec<AnthropicTool>> =
        tools.map(|defs| defs.into_iter().map(tool_definition_to_anthropic).collect());
    // Anthropic returns 400 if `tool_choice` is set but `tools` is absent.
    // Drop a stale `tool_choice` rather than emitting an invalid payload.
    let tool_choice = if tools.is_some() {
        tool_choice.map(tool_choice_to_value)
    } else {
        None
    };

    AnthropicRequest {
        model,
        messages: anth_msgs,
        system,
        max_tokens: max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        temperature,
        tools,
        tool_choice,
    }
}

fn message_to_blocks(msg: ChatMessage) -> Vec<AnthropicContent> {
    let mut out = Vec::new();
    match msg.role {
        MessageRole::User => {
            if let Some(text) = msg.content {
                if !text.is_empty() {
                    out.push(AnthropicContent::Text { text });
                }
            }
        }
        MessageRole::Assistant => {
            if let Some(text) = msg.content {
                if !text.is_empty() {
                    out.push(AnthropicContent::Text { text });
                }
            }
            if let Some(calls) = msg.tool_calls {
                for call in calls {
                    out.push(AnthropicContent::ToolUse {
                        id: call.id,
                        name: call.function.name,
                        input: call.function.arguments,
                    });
                }
            }
        }
        MessageRole::Tool => {
            // OpenAI-style: Tool message has tool_call_id + content. Map
            // to Anthropic's tool_result block, which lives inside a user
            // message. A missing tool_call_id is a programming error
            // (Tool messages should always be paired with a prior
            // tool_use); emitting an empty tool_use_id would cause
            // Anthropic to 400, so we drop the message and warn instead
            // — surfacing the upstream bug rather than masking it.
            match msg.tool_call_id {
                Some(tool_use_id) if !tool_use_id.is_empty() => {
                    out.push(AnthropicContent::ToolResult {
                        tool_use_id,
                        content: msg.content.unwrap_or_default(),
                    });
                }
                _ => {
                    tracing::warn!(
                        "dropping Tool-role message with missing/empty tool_call_id; \
                         this is a programming error in the caller"
                    );
                }
            }
        }
        MessageRole::System => {}
    }
    out
}

fn tool_definition_to_anthropic(def: ToolDefinition) -> AnthropicTool {
    let f = def.function;
    let input_schema = serde_json::to_value(&f.parameters).unwrap_or_else(|_| json!({}));
    AnthropicTool {
        name: f.name,
        description: f.description,
        input_schema,
    }
}

fn tool_choice_to_value(choice: ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({"type": "auto"}),
        ToolChoice::None => json!({"type": "none"}),
        ToolChoice::Required => json!({"type": "any"}),
        ToolChoice::Specific(name) => json!({"type": "tool", "name": name}),
    }
}

fn translate_response(resp: AnthropicResponse) -> ChatResponse {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for block in resp.content {
        match block {
            AnthropicResponseBlock::Text { text } => text_parts.push(text),
            AnthropicResponseBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall {
                    id,
                    function: FunctionCall {
                        name,
                        arguments: input,
                    },
                });
            }
        }
    }
    let content = if text_parts.is_empty() {
        None
    } else {
        Some(text_parts.join("\n"))
    };
    let tool_calls_opt = if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    };

    let finish_reason = match resp.stop_reason.as_deref() {
        Some("end_turn") | Some("stop_sequence") => Some(FinishReason::Stop),
        Some("max_tokens") => Some(FinishReason::Length),
        Some("tool_use") => Some(FinishReason::ToolCalls),
        _ => Some(FinishReason::Stop),
    };

    let message = ChatMessage {
        role: MessageRole::Assistant,
        content,
        tool_calls: tool_calls_opt,
        tool_call_id: None,
    };

    let total = resp.usage.input_tokens + resp.usage.output_tokens;
    ChatResponse {
        choices: vec![Choice {
            message,
            finish_reason,
        }],
        usage: Some(Usage {
            prompt_tokens: resp.usage.input_tokens,
            completion_tokens: resp.usage.output_tokens,
            total_tokens: total,
        }),
    }
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        ChatMessage, FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolCall,
        ToolDefinition,
    };
    use std::collections::HashMap;

    fn schema_object_with(props: Vec<(&str, SchemaType)>, required: Vec<&str>) -> JsonSchema {
        let mut map = HashMap::new();
        for (name, ty) in props {
            map.insert(
                name.to_string(),
                PropertySchema {
                    schema_type: ty,
                    description: None,
                    items: None,
                },
            );
        }
        JsonSchema {
            schema_type: SchemaType::Object,
            properties: Some(map),
            required: Some(required.into_iter().map(str::to_string).collect()),
        }
    }

    #[test]
    fn translate_lifts_system_to_top_level() {
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::system("You are concise."),
                ChatMessage::user("Hi."),
            ],
        );
        let out = translate_request(req);
        assert_eq!(out.system.as_deref(), Some("You are concise."));
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].role, "user");
    }

    #[test]
    fn translate_concatenates_multiple_system_messages() {
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::system("Rule A."),
                ChatMessage::system("Rule B."),
                ChatMessage::user("Hi."),
            ],
        );
        let out = translate_request(req);
        assert_eq!(out.system.as_deref(), Some("Rule A.\n\nRule B."));
    }

    #[test]
    fn translate_default_max_tokens_is_4096() {
        let req = ChatRequest::new("claude-opus-4-7", vec![ChatMessage::user("Hi.")]);
        let out = translate_request(req);
        assert_eq!(out.max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn translate_explicit_max_tokens_passes_through() {
        let req = ChatRequest::new("claude-opus-4-7", vec![ChatMessage::user("Hi.")])
            .with_max_tokens(256);
        let out = translate_request(req);
        assert_eq!(out.max_tokens, 256);
    }

    #[test]
    fn translate_assistant_tool_calls_become_tool_use_blocks() {
        let calls = vec![ToolCall {
            id: "toolu_abc".to_string(),
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: json!({"path": "src/lib.rs"}),
            },
        }];
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::user("Read it."),
                ChatMessage::assistant_with_tools(Some("Reading.".to_string()), calls),
            ],
        );
        let out = translate_request(req);
        // user, assistant
        assert_eq!(out.messages.len(), 2);
        let assistant = &out.messages[1];
        assert_eq!(assistant.role, "assistant");
        // [text, tool_use]
        assert_eq!(assistant.content.len(), 2);
        assert!(matches!(
            assistant.content[0],
            AnthropicContent::Text { .. }
        ));
        assert!(matches!(
            assistant.content[1],
            AnthropicContent::ToolUse { .. }
        ));
        if let AnthropicContent::ToolUse { id, name, input } = &assistant.content[1] {
            assert_eq!(id, "toolu_abc");
            assert_eq!(name, "read_file");
            assert_eq!(input, &json!({"path": "src/lib.rs"}));
        }
    }

    #[test]
    fn translate_tool_response_becomes_user_with_tool_result() {
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::assistant_with_tools(
                    None,
                    vec![ToolCall {
                        id: "toolu_1".to_string(),
                        function: FunctionCall {
                            name: "read_file".to_string(),
                            arguments: json!({}),
                        },
                    }],
                ),
                ChatMessage::tool_response("toolu_1", "file contents"),
            ],
        );
        let out = translate_request(req);
        assert_eq!(out.messages.len(), 2);
        let user_msg = &out.messages[1];
        assert_eq!(user_msg.role, "user");
        assert_eq!(user_msg.content.len(), 1);
        match &user_msg.content[0] {
            AnthropicContent::ToolResult {
                tool_use_id,
                content,
            } => {
                assert_eq!(tool_use_id, "toolu_1");
                assert_eq!(content, "file contents");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn translate_consecutive_tool_messages_merge_into_single_user() {
        // Anthropic rejects adjacent same-role messages. Two sequential
        // tool responses (e.g. for parallel tool_use blocks) must be
        // merged into one user message with two tool_result blocks.
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::assistant_with_tools(
                    None,
                    vec![
                        ToolCall {
                            id: "toolu_1".to_string(),
                            function: FunctionCall {
                                name: "f1".to_string(),
                                arguments: json!({}),
                            },
                        },
                        ToolCall {
                            id: "toolu_2".to_string(),
                            function: FunctionCall {
                                name: "f2".to_string(),
                                arguments: json!({}),
                            },
                        },
                    ],
                ),
                ChatMessage::tool_response("toolu_1", "result 1"),
                ChatMessage::tool_response("toolu_2", "result 2"),
            ],
        );
        let out = translate_request(req);
        assert_eq!(
            out.messages.len(),
            2,
            "tool responses merge into 1 user msg"
        );
        let user = &out.messages[1];
        assert_eq!(user.role, "user");
        assert_eq!(user.content.len(), 2);
        assert!(matches!(
            user.content[0],
            AnthropicContent::ToolResult { .. }
        ));
        assert!(matches!(
            user.content[1],
            AnthropicContent::ToolResult { .. }
        ));
    }

    #[test]
    fn translate_drops_empty_messages() {
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![ChatMessage::user(""), ChatMessage::user("real content")],
        );
        let out = translate_request(req);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].content.len(), 1);
    }

    fn fake_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: name.to_string(),
                description: format!("a fake {name}"),
                parameters: schema_object_with(vec![], vec![]),
            },
        }
    }

    #[test]
    fn translate_tool_choice_required_maps_to_any() {
        let mut req = ChatRequest::new("claude-opus-4-7", vec![ChatMessage::user("Hi.")])
            .with_tools(vec![fake_tool("noop")]);
        req.tool_choice = Some(ToolChoice::Required);
        let out = translate_request(req);
        assert_eq!(out.tool_choice, Some(json!({"type": "any"})));
    }

    #[test]
    fn translate_tool_choice_specific_includes_name() {
        let mut req = ChatRequest::new("claude-opus-4-7", vec![ChatMessage::user("Hi.")])
            .with_tools(vec![fake_tool("read_file")]);
        req.tool_choice = Some(ToolChoice::Specific("read_file".to_string()));
        let out = translate_request(req);
        assert_eq!(
            out.tool_choice,
            Some(json!({"type": "tool", "name": "read_file"}))
        );
    }

    #[test]
    fn translate_drops_tool_choice_when_tools_absent() {
        // Bug fix: Anthropic returns 400 if `tool_choice` is set without
        // `tools`. The translator must silently drop the choice rather
        // than emit an invalid payload.
        let mut req = ChatRequest::new("claude-opus-4-7", vec![ChatMessage::user("Hi.")]);
        req.tool_choice = Some(ToolChoice::Required);
        let out = translate_request(req);
        assert!(out.tools.is_none());
        assert!(out.tool_choice.is_none());
    }

    #[test]
    fn translate_drops_tool_message_with_missing_tool_call_id() {
        // Bug fix: a Tool-role message without `tool_call_id` produced an
        // empty `tool_use_id` which Anthropic rejects. The translator now
        // drops the message and warns; the assistant-side tool_use survives.
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::assistant_with_tools(
                    None,
                    vec![ToolCall {
                        id: "toolu_1".to_string(),
                        function: FunctionCall {
                            name: "f".to_string(),
                            arguments: json!({}),
                        },
                    }],
                ),
                // Tool message with no tool_call_id — caller bug.
                ChatMessage {
                    role: crate::types::MessageRole::Tool,
                    content: Some("oops".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage::user("continue"),
            ],
        );
        let out = translate_request(req);
        // Expected: assistant (tool_use) → user (text "continue").
        // The malformed Tool message contributes nothing.
        assert_eq!(out.messages.len(), 2);
        assert_eq!(out.messages[0].role, "assistant");
        assert_eq!(out.messages[1].role, "user");
        assert_eq!(out.messages[1].content.len(), 1);
        match &out.messages[1].content[0] {
            AnthropicContent::Text { text } => assert_eq!(text, "continue"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn translate_drops_tool_message_with_empty_tool_call_id() {
        // Same bug as above, with an explicit empty string.
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![ChatMessage {
                role: crate::types::MessageRole::Tool,
                content: Some("oops".to_string()),
                tool_calls: None,
                tool_call_id: Some(String::new()),
            }],
        );
        let out = translate_request(req);
        assert!(out.messages.is_empty());
    }

    #[test]
    fn with_api_key_rejects_control_chars() {
        // A NUL byte (or any 0x00–0x1F control char other than tab) cannot
        // become a header value. Validation happens at construction so
        // chat() never panics on header assembly.
        let r = AnthropicProvider::with_api_key("sk-test\0bad");
        assert!(matches!(r, Err(ModelError::InvalidConfig { .. })));
    }

    #[test]
    fn with_api_key_rejects_newline_in_key() {
        // A trailing newline (common copy-paste mistake) is a control
        // character and would have panicked in the old `.expect()` path.
        let r = AnthropicProvider::with_api_key("sk-test\nbadline");
        assert!(matches!(r, Err(ModelError::InvalidConfig { .. })));
    }

    #[test]
    fn with_api_key_accepts_normal_key() {
        let r = AnthropicProvider::with_api_key("sk-ant-fake-key-001");
        assert!(r.is_ok());
    }

    #[test]
    fn translate_tool_definition_includes_input_schema() {
        let tool = ToolDefinition {
            function: FunctionDefinition {
                name: "read_file".to_string(),
                description: "Read a file by path".to_string(),
                parameters: schema_object_with(vec![("path", SchemaType::String)], vec!["path"]),
            },
        };
        let req = ChatRequest::new("claude-opus-4-7", vec![ChatMessage::user("Hi.")])
            .with_tools(vec![tool]);
        let out = translate_request(req);
        let tools = out.tools.expect("tools serialised");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[0].description, "Read a file by path");
        assert_eq!(tools[0].input_schema["type"], "object");
        assert!(tools[0].input_schema["properties"].is_object());
        assert_eq!(tools[0].input_schema["required"], json!(["path"]));
    }

    #[test]
    fn translate_serialises_to_expected_json_shape() {
        let req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::system("You are concise."),
                ChatMessage::user("Hi."),
            ],
        );
        let out = translate_request(req);
        let v = serde_json::to_value(&out).unwrap();
        assert_eq!(v["model"], "claude-opus-4-7");
        assert_eq!(v["system"], "You are concise.");
        assert_eq!(v["max_tokens"], 4096);
        assert!(v["messages"].is_array());
        assert_eq!(v["messages"][0]["role"], "user");
        assert_eq!(v["messages"][0]["content"][0]["type"], "text");
        assert_eq!(v["messages"][0]["content"][0]["text"], "Hi.");
    }

    // --- response translation ---

    fn make_resp(content: Vec<AnthropicResponseBlock>, stop: Option<&str>) -> AnthropicResponse {
        AnthropicResponse {
            id: "msg_x".to_string(),
            role: "assistant".to_string(),
            content,
            stop_reason: stop.map(str::to_string),
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 20,
            },
        }
    }

    #[test]
    fn translate_response_concatenates_text_blocks() {
        let resp = make_resp(
            vec![
                AnthropicResponseBlock::Text {
                    text: "Hello".to_string(),
                },
                AnthropicResponseBlock::Text {
                    text: "world".to_string(),
                },
            ],
            Some("end_turn"),
        );
        let out = translate_response(resp);
        let m = &out.choices[0].message;
        assert_eq!(m.content.as_deref(), Some("Hello\nworld"));
        assert!(m.tool_calls.is_none());
    }

    #[test]
    fn translate_response_tool_use_blocks_become_tool_calls() {
        let resp = make_resp(
            vec![AnthropicResponseBlock::ToolUse {
                id: "toolu_xyz".to_string(),
                name: "read_file".to_string(),
                input: json!({"path": "x"}),
            }],
            Some("tool_use"),
        );
        let out = translate_response(resp);
        let m = &out.choices[0].message;
        assert!(m.content.is_none());
        let calls = m.tool_calls.as_ref().expect("tool_calls populated");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_xyz");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, json!({"path": "x"}));
        assert_eq!(out.choices[0].finish_reason, Some(FinishReason::ToolCalls));
    }

    #[test]
    fn translate_response_stop_reason_mapping() {
        let cases = [
            ("end_turn", FinishReason::Stop),
            ("stop_sequence", FinishReason::Stop),
            ("max_tokens", FinishReason::Length),
            ("tool_use", FinishReason::ToolCalls),
            ("unknown_future", FinishReason::Stop),
        ];
        for (anthropic, expected) in cases {
            let r = make_resp(
                vec![AnthropicResponseBlock::Text {
                    text: "x".to_string(),
                }],
                Some(anthropic),
            );
            assert_eq!(
                translate_response(r).choices[0].finish_reason.clone(),
                Some(expected),
                "stop_reason {anthropic} should map"
            );
        }
    }

    #[test]
    fn translate_response_usage_populated() {
        let resp = make_resp(
            vec![AnthropicResponseBlock::Text {
                text: "x".to_string(),
            }],
            Some("end_turn"),
        );
        let out = translate_response(resp);
        let usage = out.usage.expect("usage populated");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 20);
        assert_eq!(usage.total_tokens, 30);
    }

    #[test]
    fn translate_response_text_and_tool_use_coexist() {
        let resp = make_resp(
            vec![
                AnthropicResponseBlock::Text {
                    text: "I'll read it.".to_string(),
                },
                AnthropicResponseBlock::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "read_file".to_string(),
                    input: json!({"path": "x"}),
                },
            ],
            Some("tool_use"),
        );
        let out = translate_response(resp);
        let m = &out.choices[0].message;
        assert_eq!(m.content.as_deref(), Some("I'll read it."));
        assert_eq!(m.tool_calls.as_ref().unwrap().len(), 1);
    }

    // --- end-to-end round trip without HTTP ---

    #[test]
    fn round_trip_request_then_response_preserves_tool_id_chain() {
        // Send a tool_use block, parse a tool_result back, send the result
        // in the next turn. The id must survive the translation.
        let outgoing = translate_response(make_resp(
            vec![AnthropicResponseBlock::ToolUse {
                id: "toolu_42".to_string(),
                name: "search".to_string(),
                input: json!({"q": "rust"}),
            }],
            Some("tool_use"),
        ));
        let assistant_msg = outgoing.choices.into_iter().next().unwrap().message;
        let next_req = ChatRequest::new(
            "claude-opus-4-7",
            vec![
                ChatMessage::user("Search rust."),
                assistant_msg,
                ChatMessage::tool_response("toolu_42", "rust is a language"),
            ],
        );
        let translated = translate_request(next_req);
        assert_eq!(translated.messages.len(), 3);
        match &translated.messages[2].content[0] {
            AnthropicContent::ToolResult { tool_use_id, .. } => {
                assert_eq!(tool_use_id, "toolu_42");
            }
            _ => panic!("expected ToolResult"),
        }
    }

    // --- live integration test, gated behind env var ---

    #[tokio::test]
    #[ignore = "requires ANTHROPIC_API_KEY env var; run with `cargo test --features anthropic -- --ignored`"]
    async fn live_chat_returns_response() {
        let provider = AnthropicProvider::from_env().expect("ANTHROPIC_API_KEY set");
        let req = ChatRequest::new(
            "claude-haiku-4-5-20251001",
            vec![ChatMessage::user("Reply with just the word ok.")],
        )
        .with_max_tokens(16);
        let resp = provider.chat(req).await.expect("live chat ok");
        let text = resp.choices[0]
            .message
            .content
            .as_deref()
            .unwrap_or_default();
        assert!(!text.is_empty(), "got non-empty content");
        assert!(resp.usage.is_some(), "usage populated");
    }
}
