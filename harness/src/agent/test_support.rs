use crate::tools::ToolRegistry;
use async_trait::async_trait;
use model::provider::{ModelError, ModelProvider, ModelResult};
use model::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
    ModelInfo, ToolCall,
};
use std::sync::{Arc, Mutex};

pub(crate) use scripted::ScriptedTool;

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

pub(crate) struct DelayedProvider {
    delay: std::time::Duration,
    inner: Arc<MockProvider>,
}

impl DelayedProvider {
    pub(crate) fn new(delay: std::time::Duration, responses: Vec<ChatResponse>) -> Arc<Self> {
        Arc::new(Self {
            delay,
            inner: MockProvider::new(responses),
        })
    }
}

#[async_trait]
impl ModelProvider for DelayedProvider {
    async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
        tokio::time::sleep(self.delay).await;
        self.inner.chat(request).await
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> ModelResult<()> {
        Ok(())
    }

    fn provider_name(&self) -> &'static str {
        "delayed"
    }
}

pub(crate) fn scripted_registry(tools: Vec<ScriptedTool>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for tool in tools {
        registry.register(Box::new(tool));
    }
    registry
}

pub(crate) fn multi_call_response(calls: Vec<(&str, serde_json::Value)>) -> ChatResponse {
    ChatResponse {
        choices: vec![Choice {
            message: ChatMessage {
                role: MessageRole::Assistant,
                content: None,
                tool_calls: Some(
                    calls
                        .into_iter()
                        .enumerate()
                        .map(|(index, (name, args))| ToolCall {
                            id: format!("call_{index}"),
                            function: FunctionCall {
                                name: name.to_string(),
                                arguments: args,
                            },
                        })
                        .collect(),
                ),
                tool_call_id: None,
            },
            finish_reason: Some(FinishReason::ToolCalls),
        }],
        usage: None,
    }
}

#[cfg(test)]
mod scripted {
    use super::*;

    pub(crate) struct ScriptedTool {
        name: String,
        class: crate::effects::EffectClass,
        results: Mutex<std::collections::VecDeque<crate::tools::ToolResult<serde_json::Value>>>,
        on_run: Option<Box<dyn Fn() + Send + Sync>>,
    }

    impl ScriptedTool {
        pub(crate) fn new(
            name: &str,
            class: crate::effects::EffectClass,
            results: Vec<crate::tools::ToolResult<serde_json::Value>>,
        ) -> Self {
            Self {
                name: name.to_string(),
                class,
                results: Mutex::new(results.into()),
                on_run: None,
            }
        }

        pub(crate) fn on_run(mut self, hook: impl Fn() + Send + Sync + 'static) -> Self {
            self.on_run = Some(Box::new(hook));
            self
        }
    }

    #[async_trait]
    impl crate::tools::Tool for ScriptedTool {
        fn definition(&self) -> model::types::ToolDefinition {
            model::types::ToolDefinition {
                function: model::types::FunctionDefinition {
                    name: self.name.clone(),
                    description: format!("scripted {}", self.name),
                    raw_parameters: Some(serde_json::json!({"type": "object"})),
                    parameters: model::types::JsonSchema {
                        schema_type: model::types::SchemaType::Object,
                        properties: None,
                        required: None,
                    },
                },
            }
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> crate::tools::ToolResult<serde_json::Value> {
            if let Some(hook) = &self.on_run {
                hook();
            }
            self.results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(serde_json::json!({"ok": true})))
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn effect_class(&self) -> crate::effects::EffectClass {
            self.class
        }
    }
}
