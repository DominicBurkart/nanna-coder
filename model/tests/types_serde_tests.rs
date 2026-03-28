//! Serialization round-trip and edge-case tests for model types.
//!
//! The model crate's types form the API contract between the harness and
//! LLM providers. These tests verify that every variant serializes correctly
//! and that the builder API produces the expected wire format.

use model::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, FunctionDefinition,
    JsonSchema, MessageRole, PropertySchema, SchemaType, ToolCall, ToolChoice, ToolDefinition,
    Usage,
};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// MessageRole serde
// ---------------------------------------------------------------------------

#[test]
fn message_role_serializes_lowercase() {
    let json = serde_json::to_string(&MessageRole::System).unwrap();
    assert_eq!(json, r#""system""#);

    let json = serde_json::to_string(&MessageRole::Tool).unwrap();
    assert_eq!(json, r#""tool""#);
}

#[test]
fn message_role_round_trip_all_variants() {
    for role in [MessageRole::System, MessageRole::User, MessageRole::Assistant, MessageRole::Tool] {
        let json = serde_json::to_string(&role).unwrap();
        let back: MessageRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, back);
    }
}

// ---------------------------------------------------------------------------
// ToolChoice serde
// ---------------------------------------------------------------------------

#[test]
fn tool_choice_auto_round_trip() {
    let tc = ToolChoice::Auto;
    let json = serde_json::to_string(&tc).unwrap();
    let back: ToolChoice = serde_json::from_str(&json).unwrap();
    assert_eq!(tc, back);
}

#[test]
fn tool_choice_none_round_trip() {
    let tc = ToolChoice::None;
    let json = serde_json::to_string(&tc).unwrap();
    let back: ToolChoice = serde_json::from_str(&json).unwrap();
    assert_eq!(tc, back);
}

#[test]
fn tool_choice_required_round_trip() {
    let tc = ToolChoice::Required;
    let json = serde_json::to_string(&tc).unwrap();
    let back: ToolChoice = serde_json::from_str(&json).unwrap();
    assert_eq!(tc, back);
}

#[test]
fn tool_choice_specific_round_trip() {
    let tc = ToolChoice::Specific("my_tool".into());
    let json = serde_json::to_string(&tc).unwrap();
    let back: ToolChoice = serde_json::from_str(&json).unwrap();
    assert_eq!(tc, back);
    assert!(json.contains("my_tool"));
}

#[test]
fn tool_choice_default_is_auto() {
    assert_eq!(ToolChoice::default(), ToolChoice::Auto);
}

// ---------------------------------------------------------------------------
// ChatMessage constructors
// ---------------------------------------------------------------------------

#[test]
fn assistant_with_tools_constructor() {
    let tool_calls = vec![ToolCall {
        id: "call_1".into(),
        function: FunctionCall {
            name: "echo".into(),
            arguments: serde_json::json!({"msg": "hi"}),
        },
    }];
    let msg = ChatMessage::assistant_with_tools(Some("thinking...".into()), tool_calls.clone());
    assert_eq!(msg.role, MessageRole::Assistant);
    assert_eq!(msg.content, Some("thinking...".into()));
    assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
    assert_eq!(msg.tool_calls.unwrap()[0].id, "call_1");
}

#[test]
fn assistant_with_tools_none_content() {
    let msg = ChatMessage::assistant_with_tools(None, vec![]);
    assert!(msg.content.is_none());
    assert!(msg.tool_calls.unwrap().is_empty());
}

#[test]
fn tool_response_constructor_round_trip() {
    let msg = ChatMessage::tool_response("call_99", "result data");
    let json = serde_json::to_string(&msg).unwrap();
    let back: ChatMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(back.role, MessageRole::Tool);
    assert_eq!(back.tool_call_id, Some("call_99".into()));
    assert_eq!(back.content, Some("result data".into()));
}

// ---------------------------------------------------------------------------
// ChatRequest builder
// ---------------------------------------------------------------------------

#[test]
fn chat_request_with_tools_sets_auto_choice() {
    let tools = vec![ToolDefinition {
        function: FunctionDefinition {
            name: "calc".into(),
            description: "Calculator".into(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: None,
                required: None,
            },
        },
    }];
    let req = ChatRequest::new("model", vec![ChatMessage::user("hi")]).with_tools(tools);
    assert_eq!(req.tool_choice, Some(ToolChoice::Auto));
    assert_eq!(req.tools.as_ref().unwrap().len(), 1);
}

#[test]
fn chat_request_defaults_are_none() {
    let req = ChatRequest::new("m", vec![]);
    assert!(req.tools.is_none());
    assert!(req.tool_choice.is_none());
    assert!(req.temperature.is_none());
    assert!(req.max_tokens.is_none());
}

// ---------------------------------------------------------------------------
// FinishReason serde
// ---------------------------------------------------------------------------

#[test]
fn finish_reason_all_variants_round_trip() {
    for reason in [
        FinishReason::Stop,
        FinishReason::ToolCalls,
        FinishReason::Length,
        FinishReason::ContentFilter,
    ] {
        let json = serde_json::to_string(&reason).unwrap();
        let back: FinishReason = serde_json::from_str(&json).unwrap();
        assert_eq!(reason, back);
    }
}

#[test]
fn finish_reason_snake_case_format() {
    let json = serde_json::to_string(&FinishReason::ToolCalls).unwrap();
    assert_eq!(json, r#""tool_calls""#);

    let json = serde_json::to_string(&FinishReason::ContentFilter).unwrap();
    assert_eq!(json, r#""content_filter""#);
}

// ---------------------------------------------------------------------------
// Usage round-trip
// ---------------------------------------------------------------------------

#[test]
fn usage_round_trip() {
    let usage = Usage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
    };
    let json = serde_json::to_string(&usage).unwrap();
    let back: Usage = serde_json::from_str(&json).unwrap();
    assert_eq!(back.prompt_tokens, 100);
    assert_eq!(back.completion_tokens, 50);
    assert_eq!(back.total_tokens, 150);
}

// ---------------------------------------------------------------------------
// ChatResponse with multiple choices
// ---------------------------------------------------------------------------

#[test]
fn chat_response_multiple_choices_round_trip() {
    let response = ChatResponse {
        choices: vec![
            Choice {
                message: ChatMessage::assistant("answer A"),
                finish_reason: Some(FinishReason::Stop),
            },
            Choice {
                message: ChatMessage::assistant("answer B"),
                finish_reason: Some(FinishReason::Length),
            },
        ],
        usage: Some(Usage {
            prompt_tokens: 10,
            completion_tokens: 20,
            total_tokens: 30,
        }),
    };

    let json = serde_json::to_string(&response).unwrap();
    let back: ChatResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(back.choices.len(), 2);
    assert_eq!(back.choices[0].message.content, Some("answer A".into()));
    assert_eq!(back.choices[1].finish_reason, Some(FinishReason::Length));
    assert_eq!(back.usage.unwrap().total_tokens, 30);
}

// ---------------------------------------------------------------------------
// ToolCall and FunctionCall serde
// ---------------------------------------------------------------------------

#[test]
fn tool_call_round_trip() {
    let tc = ToolCall {
        id: "call_abc".into(),
        function: FunctionCall {
            name: "search".into(),
            arguments: serde_json::json!({"query": "rust async", "limit": 5}),
        },
    };
    let json = serde_json::to_string(&tc).unwrap();
    let back: ToolCall = serde_json::from_str(&json).unwrap();
    assert_eq!(back.id, "call_abc");
    assert_eq!(back.function.name, "search");
    assert_eq!(back.function.arguments["limit"], 5);
}

// ---------------------------------------------------------------------------
// SchemaType serde
// ---------------------------------------------------------------------------

#[test]
fn schema_type_all_variants_serialize_lowercase() {
    let types = vec![
        (SchemaType::Object, "object"),
        (SchemaType::String, "string"),
        (SchemaType::Number, "number"),
        (SchemaType::Integer, "integer"),
        (SchemaType::Boolean, "boolean"),
        (SchemaType::Array, "array"),
    ];
    for (variant, expected) in types {
        let json = serde_json::to_string(&variant).unwrap();
        assert_eq!(json, format!(r#""{}""#, expected));
    }
}

// ---------------------------------------------------------------------------
// PropertySchema with nested items
// ---------------------------------------------------------------------------

#[test]
fn property_schema_with_array_items_round_trip() {
    let schema = PropertySchema {
        schema_type: SchemaType::Array,
        description: Some("list of names".into()),
        items: Some(Box::new(PropertySchema {
            schema_type: SchemaType::String,
            description: None,
            items: None,
        })),
    };
    let json = serde_json::to_string(&schema).unwrap();
    let back: PropertySchema = serde_json::from_str(&json).unwrap();
    assert!(back.items.is_some());
}
