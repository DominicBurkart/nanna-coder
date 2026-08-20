use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant_with_tools(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool_response(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Specific(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub function: FunctionDefinition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: JsonSchema,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonSchema {
    #[serde(rename = "type")]
    pub schema_type: SchemaType,
    pub properties: Option<HashMap<String, PropertySchema>>,
    pub required: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchemaType {
    Object,
    String,
    Number,
    Integer,
    Boolean,
    Array,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropertySchema {
    #[serde(rename = "type")]
    pub schema_type: SchemaType,
    pub description: Option<String>,
    pub items: Option<Box<PropertySchema>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub tool_choice: Option<ToolChoice>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: None,
            tool_choice: None,
            temperature: None,
            max_tokens: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = Some(tools);
        self.tool_choice = Some(ToolChoice::Auto);
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<Choice>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub message: ChatMessage,
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub size: Option<u64>,
    pub digest: Option<String>,
    pub modified_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_constructors() {
        let sys_msg = ChatMessage::system("You are helpful");
        assert_eq!(sys_msg.role, MessageRole::System);
        assert_eq!(sys_msg.content, Some("You are helpful".to_string()));

        let user_msg = ChatMessage::user("Hello");
        assert_eq!(user_msg.role, MessageRole::User);
        assert_eq!(user_msg.content, Some("Hello".to_string()));

        let tool_response = ChatMessage::tool_response("call_123", "Result");
        assert_eq!(tool_response.role, MessageRole::Tool);
        assert_eq!(tool_response.tool_call_id, Some("call_123".to_string()));
    }

    #[test]
    fn test_chat_request_builder() {
        let messages = vec![ChatMessage::user("Hello")];
        let request = ChatRequest::new("llama3.1:8b", messages)
            .with_temperature(0.7)
            .with_max_tokens(1000);

        assert_eq!(request.model, "llama3.1:8b");
        assert_eq!(request.temperature, Some(0.7));
        assert_eq!(request.max_tokens, Some(1000));
    }

    #[test]
    fn test_serialization() {
        let message = ChatMessage::user("Hello world");
        let json = serde_json::to_string(&message).unwrap();
        let deserialized: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(message.content, deserialized.content);
        assert_eq!(message.role, deserialized.role);
    }

    // -----------------------------------------------------------------------
    // ChatMessage::assistant and assistant_with_tools
    // -----------------------------------------------------------------------

    #[test]
    fn test_assistant_message_constructor() {
        let msg = ChatMessage::assistant("I can help with that.");
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.content, Some("I can help with that.".to_string()));
        assert!(msg.tool_calls.is_none());
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn test_assistant_with_tools_constructor() {
        let tool_call = ToolCall {
            id: "call_abc".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: serde_json::json!({"location": "Paris"}),
            },
        };
        let msg = ChatMessage::assistant_with_tools(
            Some("Using tool".to_string()),
            vec![tool_call.clone()],
        );
        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.content, Some("Using tool".to_string()));
        let calls = msg.tool_calls.expect("tool_calls should be present");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].function.name, "get_weather");

        // None content variant
        let msg_no_content = ChatMessage::assistant_with_tools(None, vec![]);
        assert!(msg_no_content.content.is_none());
        assert_eq!(msg_no_content.role, MessageRole::Assistant);
    }

    // -----------------------------------------------------------------------
    // ChatRequest::with_tools
    // -----------------------------------------------------------------------

    #[test]
    fn test_chat_request_with_tools() {
        let tool = ToolDefinition {
            function: FunctionDefinition {
                name: "search".to_string(),
                description: "Searches the web".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut map = HashMap::new();
                        map.insert(
                            "query".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("The search query".to_string()),
                                items: None,
                            },
                        );
                        map
                    }),
                    required: Some(vec!["query".to_string()]),
                },
            },
        };
        let messages = vec![ChatMessage::user("Search for Rust")];
        let request = ChatRequest::new("llama3.1:8b", messages).with_tools(vec![tool]);

        assert!(request.tools.is_some());
        assert_eq!(request.tools.as_ref().unwrap().len(), 1);
        assert_eq!(request.tools.as_ref().unwrap()[0].function.name, "search");
        assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
    }

    // -----------------------------------------------------------------------
    // ToolChoice serialization (all variants)
    // -----------------------------------------------------------------------

    #[test]
    fn test_tool_choice_serialization() {
        assert_eq!(
            serde_json::to_string(&ToolChoice::Auto).unwrap(),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::None).unwrap(),
            "\"none\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Required).unwrap(),
            "\"required\""
        );

        // Specific variant
        let specific = ToolChoice::Specific("my_fn".to_string());
        let json = serde_json::to_string(&specific).unwrap();
        let roundtripped: ToolChoice = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtripped, specific);
    }

    #[test]
    fn test_tool_choice_default() {
        let choice = ToolChoice::default();
        assert_eq!(choice, ToolChoice::Auto);
    }

    // -----------------------------------------------------------------------
    // FinishReason serialization (all variants)
    // -----------------------------------------------------------------------

    #[test]
    fn test_finish_reason_serialization() {
        let cases = [
            (FinishReason::Stop, "\"stop\""),
            (FinishReason::ToolCalls, "\"tool_calls\""),
            (FinishReason::Length, "\"length\""),
            (FinishReason::ContentFilter, "\"content_filter\""),
        ];
        for (reason, expected_json) in cases {
            let json = serde_json::to_string(&reason).unwrap();
            assert_eq!(json, expected_json, "wrong JSON for {:?}", reason);
            let roundtripped: FinishReason = serde_json::from_str(&json).unwrap();
            assert_eq!(roundtripped, reason);
        }
    }

    // -----------------------------------------------------------------------
    // Tool and schema types serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_tool_call_serialization() {
        let tool_call = ToolCall {
            id: "call_xyz".to_string(),
            function: FunctionCall {
                name: "do_something".to_string(),
                arguments: serde_json::json!({"key": "value"}),
            },
        };
        let json = serde_json::to_string(&tool_call).unwrap();
        let roundtripped: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtripped.id, tool_call.id);
        assert_eq!(roundtripped.function.name, tool_call.function.name);
        assert_eq!(
            roundtripped.function.arguments,
            tool_call.function.arguments
        );
    }

    #[test]
    fn test_schema_type_serialization() {
        let types = [
            (SchemaType::Object, "\"object\""),
            (SchemaType::String, "\"string\""),
            (SchemaType::Number, "\"number\""),
            (SchemaType::Integer, "\"integer\""),
            (SchemaType::Boolean, "\"boolean\""),
            (SchemaType::Array, "\"array\""),
        ];
        for (schema_type, expected) in types {
            let json = serde_json::to_string(&schema_type).unwrap();
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn test_json_schema_with_array_property() {
        let schema = JsonSchema {
            schema_type: SchemaType::Object,
            properties: Some({
                let mut map = HashMap::new();
                map.insert(
                    "tags".to_string(),
                    PropertySchema {
                        schema_type: SchemaType::Array,
                        description: Some("List of tags".to_string()),
                        items: Some(Box::new(PropertySchema {
                            schema_type: SchemaType::String,
                            description: None,
                            items: None,
                        })),
                    },
                );
                map
            }),
            required: None,
        };
        let json = serde_json::to_string(&schema).unwrap();
        let roundtripped: JsonSchema = serde_json::from_str(&json).unwrap();
        let props = roundtripped.properties.unwrap();
        let tags = props.get("tags").unwrap();
        assert!(matches!(tags.schema_type, SchemaType::Array));
        assert!(tags.items.is_some());
    }

    // -----------------------------------------------------------------------
    // Usage struct
    // -----------------------------------------------------------------------

    #[test]
    fn test_usage_serialization() {
        let usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let roundtripped: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtripped.prompt_tokens, usage.prompt_tokens);
        assert_eq!(roundtripped.completion_tokens, usage.completion_tokens);
        assert_eq!(roundtripped.total_tokens, usage.total_tokens);
    }

    // -----------------------------------------------------------------------
    // ChatResponse and Choice
    // -----------------------------------------------------------------------

    #[test]
    fn test_chat_response_serialization() {
        let response = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage::assistant("Hello!"),
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: Some(Usage {
                prompt_tokens: 5,
                completion_tokens: 3,
                total_tokens: 8,
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        let roundtripped: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtripped.choices.len(), 1);
        assert_eq!(
            roundtripped.choices[0].message.content,
            Some("Hello!".to_string())
        );
        assert_eq!(
            roundtripped.choices[0].finish_reason,
            Some(FinishReason::Stop)
        );
        let u = roundtripped.usage.unwrap();
        assert_eq!(u.total_tokens, 8);
    }

    // -----------------------------------------------------------------------
    // ModelInfo
    // -----------------------------------------------------------------------

    #[test]
    fn test_model_info_serialization() {
        let info = ModelInfo {
            name: "llama3.1:8b".to_string(),
            size: Some(4_000_000_000),
            digest: Some("sha256:abc123".to_string()),
            modified_at: Some("2024-01-01T00:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&info).unwrap();
        let roundtripped: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtripped.name, info.name);
        assert_eq!(roundtripped.size, info.size);
        assert_eq!(roundtripped.digest, info.digest);
        assert_eq!(roundtripped.modified_at, info.modified_at);
    }
}
