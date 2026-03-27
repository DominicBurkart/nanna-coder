//! Extended tests for model::types
//!
//! The existing tests in types.rs cover basic message constructors,
//! ChatRequest::new builder, and simple serialization. These tests
//! expand coverage to ChatRequest::with_tools, assistant_with_tools,
//! ToolChoice serde round-trips, and edge cases.

#[cfg(test)]
mod tests {
    use crate::types::*;
    use std::collections::HashMap;

    // ── ChatMessage::assistant_with_tools ────────────────────────────────

    #[test]
    fn assistant_with_tools_sets_role_and_tool_calls() {
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
            },
        };
        let msg = ChatMessage::assistant_with_tools(Some("thinking...".into()), vec![tool_call]);

        assert_eq!(msg.role, MessageRole::Assistant);
        assert_eq!(msg.content, Some("thinking...".to_string()));
        assert!(msg.tool_calls.is_some());
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
        assert!(msg.tool_call_id.is_none());
    }

    #[test]
    fn assistant_with_tools_allows_none_content() {
        let msg = ChatMessage::assistant_with_tools(None, vec![]);
        assert!(msg.content.is_none());
        assert!(msg.tool_calls.unwrap().is_empty());
    }

    // ── ChatRequest::with_tools ──────────────────────────────────────────

    #[test]
    fn with_tools_sets_tools_and_auto_choice() {
        let tool = ToolDefinition {
            function: FunctionDefinition {
                name: "calculator".to_string(),
                description: "Perform arithmetic".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: None,
                    required: None,
                },
            },
        };
        let request = ChatRequest::new("model", vec![ChatMessage::user("hi")])
            .with_tools(vec![tool]);

        assert!(request.tools.is_some());
        assert_eq!(request.tools.as_ref().unwrap().len(), 1);
        assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
    }

    // ── ToolChoice serde round-trips ─────────────────────────────────────

    #[test]
    fn tool_choice_auto_round_trip() {
        let choice = ToolChoice::Auto;
        let json = serde_json::to_string(&choice).unwrap();
        let back: ToolChoice = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ToolChoice::Auto);
    }

    #[test]
    fn tool_choice_none_round_trip() {
        let choice = ToolChoice::None;
        let json = serde_json::to_string(&choice).unwrap();
        let back: ToolChoice = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ToolChoice::None);
    }

    #[test]
    fn tool_choice_required_round_trip() {
        let choice = ToolChoice::Required;
        let json = serde_json::to_string(&choice).unwrap();
        let back: ToolChoice = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ToolChoice::Required);
    }

    #[test]
    fn tool_choice_specific_round_trip() {
        let choice = ToolChoice::Specific("my_tool".to_string());
        let json = serde_json::to_string(&choice).unwrap();
        let back: ToolChoice = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ToolChoice::Specific("my_tool".to_string()));
    }

    #[test]
    fn tool_choice_default_is_auto() {
        assert_eq!(ToolChoice::default(), ToolChoice::Auto);
    }

    // ── MessageRole serde ────────────────────────────────────────────────

    #[test]
    fn message_role_serializes_lowercase() {
        let json = serde_json::to_string(&MessageRole::System).unwrap();
        assert_eq!(json, "\"system\"");

        let json = serde_json::to_string(&MessageRole::Tool).unwrap();
        assert_eq!(json, "\"tool\"");
    }

    // ── FinishReason serde ────────────────────────────────────────────────

    #[test]
    fn finish_reason_round_trips() {
        for reason in &[
            FinishReason::Stop,
            FinishReason::ToolCalls,
            FinishReason::Length,
            FinishReason::ContentFilter,
        ] {
            let json = serde_json::to_string(reason).unwrap();
            let back: FinishReason = serde_json::from_str(&json).unwrap();
            assert_eq!(&back, reason);
        }
    }

    // ── SchemaType serde ─────────────────────────────────────────────────

    #[test]
    fn schema_type_serializes_lowercase() {
        let json = serde_json::to_string(&SchemaType::Object).unwrap();
        assert_eq!(json, "\"object\"");
        let json = serde_json::to_string(&SchemaType::Boolean).unwrap();
        assert_eq!(json, "\"boolean\"");
    }

    // ── ToolCall / FunctionCall serde ─────────────────────────────────────

    #[test]
    fn tool_call_round_trip() {
        let tc = ToolCall {
            id: "call_42".to_string(),
            function: FunctionCall {
                name: "echo".to_string(),
                arguments: serde_json::json!({"msg": "hello"}),
            },
        };

        let json = serde_json::to_string(&tc).unwrap();
        let back: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "call_42");
        assert_eq!(back.function.name, "echo");
        assert_eq!(back.function.arguments["msg"], "hello");
    }

    // ── ChatResponse / Usage serde ────────────────────────────────────────

    #[test]
    fn chat_response_with_usage_round_trip() {
        let response = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage::assistant("Hello!"),
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        };

        let json = serde_json::to_string(&response).unwrap();
        let back: ChatResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.choices.len(), 1);
        let usage = back.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    // ── ChatRequest with_max_tokens + with_temperature combined ───────────

    #[test]
    fn chat_request_chained_builder() {
        let request = ChatRequest::new("gpt-4", vec![ChatMessage::user("Go")])
            .with_temperature(0.0)
            .with_max_tokens(1)
            .with_tools(vec![]);

        assert_eq!(request.temperature, Some(0.0));
        assert_eq!(request.max_tokens, Some(1));
        assert!(request.tools.unwrap().is_empty());
        assert_eq!(request.tool_choice, Some(ToolChoice::Auto));
    }

    // ── ModelInfo serde ───────────────────────────────────────────────────

    #[test]
    fn model_info_round_trip_with_optional_fields() {
        let info = ModelInfo {
            name: "llama3:8b".to_string(),
            size: Some(4_000_000_000),
            digest: Some("abc123".to_string()),
            modified_at: Some("2025-01-01T00:00:00Z".to_string()),
        };

        let json = serde_json::to_string(&info).unwrap();
        let back: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "llama3:8b");
        assert_eq!(back.size, Some(4_000_000_000));
    }

    #[test]
    fn model_info_round_trip_without_optional_fields() {
        let info = ModelInfo {
            name: "tiny".to_string(),
            size: None,
            digest: None,
            modified_at: None,
        };

        let json = serde_json::to_string(&info).unwrap();
        let back: ModelInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "tiny");
        assert!(back.size.is_none());
    }

    // ── ToolDefinition / FunctionDefinition full round-trip ───────────────

    #[test]
    fn tool_definition_round_trip() {
        let mut props = HashMap::new();
        props.insert(
            "path".to_string(),
            PropertySchema {
                schema_type: SchemaType::String,
                description: Some("File path".to_string()),
                items: None,
            },
        );

        let tool = ToolDefinition {
            function: FunctionDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some(props),
                    required: Some(vec!["path".to_string()]),
                },
            },
        };

        let json = serde_json::to_string(&tool).unwrap();
        let back: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(back.function.name, "read_file");
        assert!(back.function.parameters.properties.is_some());
        assert!(back
            .function
            .parameters
            .properties
            .unwrap()
            .contains_key("path"));
        assert_eq!(
            back.function.parameters.required,
            Some(vec!["path".to_string()])
        );
    }

    // ── PropertySchema with nested items ──────────────────────────────────

    #[test]
    fn property_schema_with_array_items_round_trip() {
        let prop = PropertySchema {
            schema_type: SchemaType::Array,
            description: Some("List of tags".to_string()),
            items: Some(Box::new(PropertySchema {
                schema_type: SchemaType::String,
                description: None,
                items: None,
            })),
        };

        let json = serde_json::to_string(&prop).unwrap();
        let back: PropertySchema = serde_json::from_str(&json).unwrap();
        assert!(back.items.is_some());
    }
}
