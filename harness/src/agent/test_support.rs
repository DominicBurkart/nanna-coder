use crate::tools::ToolRegistry;
use async_trait::async_trait;
use model::provider::{ModelError, ModelProvider, ModelResult};
use model::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
    ModelInfo, ToolCall,
};
use std::sync::{Arc, Mutex};

pub(crate) struct MockProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl MockProvider {
    pub(crate) fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses),
        })
    }
}

#[async_trait]
impl ModelProvider for MockProvider {
    async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err(ModelError::Unknown {
                message: "No more responses".to_string(),
            });
        }
        Ok(responses.remove(0))
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> ModelResult<()> {
        Ok(())
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }
}

pub(crate) fn plain_response(content: &str) -> ChatResponse {
    ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: MessageRole::Assistant,
                content: Some(content.to_string()),
                tool_calls: None,
                tool_call_id: None,
            },
            finish_reason: Some(FinishReason::Stop),
        }],
        usage: None,
    }
}

pub(crate) fn tool_call_response(tool_name: &str, args: serde_json::Value) -> ChatResponse {
    ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_0".to_string(),
                    function: FunctionCall {
                        name: tool_name.to_string(),
                        arguments: args,
                    },
                }]),
                tool_call_id: None,
            },
            finish_reason: Some(FinishReason::ToolCalls),
        }],
        usage: None,
    }
}

pub(crate) fn scoped_write_registry(root: &std::path::Path) -> ToolRegistry {
    let mut identity = crate::identity::example();
    identity.scope.max_effect = crate::effects::EffectClass::Workspace;
    identity.scope.tools = vec!["write_file".parse().unwrap()];
    crate::tools::create_tool_registry_for(root, &identity).unwrap()
}
