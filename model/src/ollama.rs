use crate::config::OllamaConfig;
use crate::provider::{ModelError, ModelProvider, ModelResult};
use crate::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole, ModelInfo, Usage,
};
use async_trait::async_trait;
use ollama_rs::{
    Ollama,
    generation::chat::{ChatMessage as OllamaChatMessage, MessageRole as OllamaRole},
};
use tracing::{debug, error, info};

pub struct OllamaProvider {
    client: Ollama,
    #[allow(dead_code)]
    config: OllamaConfig,
}

impl OllamaProvider {
    pub fn new(config: OllamaConfig) -> ModelResult<Self> {
        config
            .validate()
            .map_err(|msg| ModelError::InvalidConfig { message: msg })?;

        let host = if config.base_url.ends_with("/v1") {
            config.base_url[..config.base_url.len() - 3].to_string()
        } else {
            config.base_url.clone()
        };

        let client = Ollama::new(host, 11434);

        Ok(Self { client, config })
    }

    pub fn with_default_config() -> ModelResult<Self> {
        Self::new(OllamaConfig::default())
    }

    fn convert_message_role(role: &MessageRole) -> OllamaRole {
        match role {
            MessageRole::System => OllamaRole::System,
            MessageRole::User => OllamaRole::User,
            MessageRole::Assistant => OllamaRole::Assistant,
            MessageRole::Tool => OllamaRole::Tool,
        }
    }

    #[allow(dead_code)]
    fn convert_message_from_ollama(msg: &OllamaChatMessage) -> ChatMessage {
        let role = match msg.role {
            OllamaRole::System => MessageRole::System,
            OllamaRole::User => MessageRole::User,
            OllamaRole::Assistant => MessageRole::Assistant,
            OllamaRole::Tool => MessageRole::Tool,
        };

        ChatMessage {
            role,
            content: Some(msg.content.clone()),
            tool_calls: None, // Tool calls will be handled separately for now
            tool_call_id: None,
        }
    }

    fn convert_message_to_ollama(msg: &ChatMessage) -> OllamaChatMessage {
        let role = Self::convert_message_role(&msg.role);

        OllamaChatMessage {
            role,
            content: msg.content.clone().unwrap_or_default(),
            images: None,
            tool_calls: vec![], // Empty for now
        }
    }

    fn handle_ollama_error(err: ollama_rs::error::OllamaError) -> ModelError {
        match err {
            ollama_rs::error::OllamaError::ReqwestError(e) => {
                if e.is_timeout() {
                    ModelError::ServiceUnavailable {
                        message: "Request timeout".to_string(),
                    }
                } else if e.is_connect() {
                    ModelError::ServiceUnavailable {
                        message: "Cannot connect to Ollama service".to_string(),
                    }
                } else {
                    // Convert to the right reqwest error type
                    ModelError::Unknown {
                        message: format!("Network error: {}", e),
                    }
                }
            }
            ollama_rs::error::OllamaError::JsonError(e) => ModelError::Serialization(e),
            _ => ModelError::Unknown {
                message: format!("Ollama error: {}", err),
            },
        }
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        debug!("Starting chat request with model: {}", request.model);

        let ollama_messages: Vec<OllamaChatMessage> = request
            .messages
            .iter()
            .map(Self::convert_message_to_ollama)
            .collect();

        // For now, use the completion API since it's simpler
        let prompt = ollama_messages
            .iter()
            .map(|msg| msg.content.clone())
            .collect::<Vec<_>>()
            .join("\n");

        let generation_request = ollama_rs::generation::completion::request::GenerationRequest::new(
            request.model.clone(),
            prompt,
        );

        let response = self
            .client
            .generate(generation_request)
            .await
            .map_err(Self::handle_ollama_error)?;

        let message = ChatMessage::assistant(response.response);

        let choice = Choice {
            message,
            finish_reason: Some(FinishReason::Stop),
        };

        let usage = Some(Usage {
            prompt_tokens: response.prompt_eval_count.unwrap_or(0) as u32,
            completion_tokens: response.eval_count.unwrap_or(0) as u32,
            total_tokens: (response.prompt_eval_count.unwrap_or(0)
                + response.eval_count.unwrap_or(0)) as u32,
        });

        info!("Chat request completed successfully");

        Ok(ChatResponse {
            choices: vec![choice],
            usage,
        })
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        debug!("Listing available models");

        let models = self
            .client
            .list_local_models()
            .await
            .map_err(Self::handle_ollama_error)?;

        let model_infos: Vec<ModelInfo> = models
            .into_iter()
            .map(|model| ModelInfo {
                name: model.name,
                size: Some(model.size),
                digest: None, // Not available in this API
                modified_at: Some(model.modified_at),
            })
            .collect();

        info!("Retrieved {} models", model_infos.len());
        Ok(model_infos)
    }

    async fn health_check(&self) -> ModelResult<()> {
        debug!("Performing health check");

        match self.list_models().await {
            Ok(_) => {
                info!("Health check passed");
                Ok(())
            }
            Err(e) => {
                error!("Health check failed: {}", e);
                Err(e)
            }
        }
    }

    fn provider_name(&self) -> &'static str {
        "ollama"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_conversion() {
        let our_message = ChatMessage::user("Hello world");
        let ollama_message = OllamaProvider::convert_message_to_ollama(&our_message);
        let converted_back = OllamaProvider::convert_message_from_ollama(&ollama_message);

        assert_eq!(our_message.role, converted_back.role);
        assert_eq!(our_message.content, converted_back.content);
    }

    #[tokio::test]
    async fn test_provider_creation() {
        let config = OllamaConfig::default();
        let provider = OllamaProvider::new(config);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().provider_name(), "ollama");
    }
}
