use crate::types::{ChatRequest, ChatResponse, ModelInfo};
use async_trait::async_trait;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ModelError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Model not found: {model}")]
    ModelNotFound { model: String },

    #[error("Invalid configuration: {message}")]
    InvalidConfig { message: String },

    #[error("Service unavailable: {message}")]
    ServiceUnavailable { message: String },

    #[error("Rate limit exceeded")]
    RateLimit,

    #[error("Authentication failed")]
    Authentication,

    #[error("Unknown error: {message}")]
    Unknown { message: String },
}

pub type ModelResult<T> = Result<T, ModelError>;

#[async_trait]
pub trait ModelProvider: Send + Sync {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse>;

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>>;

    async fn health_check(&self) -> ModelResult<()>;

    fn provider_name(&self) -> &'static str;
}

#[async_trait]
pub trait StreamingModelProvider: ModelProvider {
    type StreamItem;
    type StreamError;

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> ModelResult<impl futures::Stream<Item = Result<Self::StreamItem, Self::StreamError>>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, MessageRole};

    struct MockProvider;

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            Ok(ChatResponse {
                choices: vec![crate::types::Choice {
                    message: ChatMessage {
                        role: MessageRole::Assistant,
                        content: Some("Mock response".to_string()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    finish_reason: Some(crate::types::FinishReason::Stop),
                }],
                usage: None,
            })
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![ModelInfo {
                name: "mock-model".to_string(),
                size: Some(1024),
                digest: Some("mock-digest".to_string()),
                modified_at: None,
            }])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "mock"
        }
    }

    #[tokio::test]
    async fn test_mock_provider() {
        let provider = MockProvider;

        let request = ChatRequest::new("mock-model", vec![ChatMessage::user("Hello")]);

        let response = provider.chat(request).await.unwrap();
        assert_eq!(response.choices.len(), 1);
        assert_eq!(response.choices[0].message.role, MessageRole::Assistant);

        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "mock-model");

        provider.health_check().await.unwrap();
        assert_eq!(provider.provider_name(), "mock");
    }
}
