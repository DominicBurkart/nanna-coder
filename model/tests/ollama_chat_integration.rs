use model::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, FunctionDefinition, JsonSchema,
    ModelProvider, OllamaConfig, OllamaProvider, PropertySchema, SchemaType, ToolDefinition,
};
use std::collections::HashMap;
use std::time::Duration;

const MODEL: &str = "qwen3:0.6b";
const TIMEOUT: Duration = Duration::from_secs(120);

fn make_provider() -> OllamaProvider {
    OllamaProvider::new(OllamaConfig::default().with_timeout(TIMEOUT)).expect("provider creation")
}

fn assert_valid_response(response: &ChatResponse) {
    assert!(!response.choices.is_empty(), "choices must not be empty");
    let choice = &response.choices[0];
    assert!(
        choice.finish_reason.is_some(),
        "finish_reason must be present"
    );
}

#[tokio::test]
#[ignore]
async fn test_health_check() {
    let provider = make_provider();

    let result = tokio::time::timeout(TIMEOUT, provider.health_check()).await;
    let health = result.expect("health_check timed out");
    health.expect("health_check failed");

    let models = tokio::time::timeout(TIMEOUT, provider.list_models())
        .await
        .expect("list_models timed out")
        .expect("list_models failed");

    assert!(!models.is_empty(), "model list must not be empty");
    assert!(
        models.iter().any(|m| m.name.starts_with("qwen3")),
        "qwen3 model must be present, found: {:?}",
        models.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
#[ignore]
async fn test_basic_chat() {
    let provider = make_provider();
    let request = ChatRequest::new(MODEL, vec![ChatMessage::user("What is 2+2?")]);

    let response = tokio::time::timeout(TIMEOUT, provider.chat(request))
        .await
        .expect("chat timed out")
        .expect("chat failed");

    assert_eq!(response.choices.len(), 1);

    let choice = &response.choices[0];
    assert!(
        matches!(choice.finish_reason, Some(FinishReason::Stop)),
        "expected Stop, got {:?}",
        choice.finish_reason
    );

    let content = choice.message.content.as_ref().expect("content must exist");
    assert!(!content.is_empty(), "content must not be empty");

    let usage = response.usage.as_ref().expect("usage must exist");
    assert!(usage.prompt_tokens > 0, "prompt_tokens must be > 0");
    assert!(usage.completion_tokens > 0, "completion_tokens must be > 0");
}

#[tokio::test]
#[ignore]
async fn test_chat_with_system_message() {
    let provider = make_provider();
    let request = ChatRequest::new(
        MODEL,
        vec![
            ChatMessage::system(
                "You are a helpful assistant. Always respond in exactly one sentence.",
            ),
            ChatMessage::user("What is the capital of France?"),
        ],
    );

    let response = tokio::time::timeout(TIMEOUT, provider.chat(request))
        .await
        .expect("chat timed out")
        .expect("chat failed");

    assert_valid_response(&response);

    let content = response.choices[0]
        .message
        .content
        .as_ref()
        .expect("content must exist");
    assert!(!content.is_empty(), "content must not be empty");
}

#[tokio::test]
#[ignore]
async fn test_chat_with_tool_definitions() {
    let provider = make_provider();

    let mut properties = HashMap::new();
    properties.insert(
        "location".to_string(),
        PropertySchema {
            schema_type: SchemaType::String,
            description: Some("The city and state, e.g. San Francisco, CA".to_string()),
            items: None,
        },
    );

    let tool = ToolDefinition {
        function: FunctionDefinition {
            name: "get_weather".to_string(),
            description: "Get the current weather in a given location".to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: Some(properties),
                required: Some(vec!["location".to_string()]),
            },
        },
    };

    let request = ChatRequest::new(
        MODEL,
        vec![ChatMessage::user(
            "What is the current weather in San Francisco?",
        )],
    )
    .with_tools(vec![tool]);

    let response = tokio::time::timeout(TIMEOUT, provider.chat(request))
        .await
        .expect("chat timed out")
        .expect("chat failed");

    assert_valid_response(&response);
    let choice = &response.choices[0];

    match &choice.finish_reason {
        Some(FinishReason::ToolCalls) => {
            let tool_calls = choice
                .message
                .tool_calls
                .as_ref()
                .expect("tool_calls must exist when finish_reason is ToolCalls");
            assert!(!tool_calls.is_empty(), "tool_calls must not be empty");
            for tc in tool_calls {
                assert!(
                    !tc.function.name.is_empty(),
                    "tool call function name must not be empty"
                );
            }
        }
        Some(FinishReason::Stop) => {
            let content = choice
                .message
                .content
                .as_ref()
                .expect("content must exist when model answers directly");
            assert!(!content.is_empty(), "content must not be empty");
        }
        other => panic!("unexpected finish_reason: {:?}", other),
    }
}

#[tokio::test]
#[ignore]
async fn test_chat_response_structure() {
    let provider = make_provider();
    let request = ChatRequest::new(MODEL, vec![ChatMessage::user("Say hello.")]);

    let response = tokio::time::timeout(TIMEOUT, provider.chat(request))
        .await
        .expect("chat timed out")
        .expect("chat failed");

    assert!(!response.choices.is_empty(), "choices must not be empty");

    let usage = response.usage.as_ref().expect("usage must be present");
    assert_eq!(
        usage.total_tokens,
        usage.prompt_tokens + usage.completion_tokens,
        "total_tokens must equal prompt_tokens + completion_tokens"
    );
}

#[tokio::test]
#[ignore]
async fn test_invalid_model_returns_error() {
    let provider = make_provider();
    let request = ChatRequest::new("nonexistent-model-xyz", vec![ChatMessage::user("Hello")]);

    let result = tokio::time::timeout(TIMEOUT, provider.chat(request))
        .await
        .expect("chat timed out");

    assert!(result.is_err(), "expected error for nonexistent model");
}
