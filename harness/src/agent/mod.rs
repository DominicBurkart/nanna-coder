//! Agent architecture implementation
//!
//! This module implements the main agent control loop following the architecture:
//! 1. Application State → User Prompt
//! 2. Plan
//! 3. Task Complete? decision
//! 4. If No → Decision → Query (RAG) or Plan
//! 5. Plan → Perform → back to check
//! 6. If Yes → Application State 2 (completed)

pub mod decision;
pub mod rag;

use async_trait::async_trait;
use model::types::ChatMessage;
use model::{ModelError, ModelProvider};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::tools::{ToolError, ToolRegistry};

/// Errors that can occur in the agent
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Agent state error: {0}")]
    StateError(String),
    #[error("Task completion check failed: {0}")]
    TaskCheckFailed(String),
    #[error("Maximum iterations exceeded")]
    MaxIterationsExceeded,
    #[error("Model error: {0}")]
    ModelError(#[from] ModelError),
    #[error("Tool error: {0}")]
    ToolError(#[from] ToolError),
}

pub type AgentResult<T> = Result<T, AgentError>;

/// State of the agent in the control loop
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    /// Planning
    Planning,
    /// Querying using RAG
    Querying,
    /// Deciding what to do
    Deciding,
    /// Performing action
    Performing,
    /// Checking if task is complete
    CheckingCompletion,
    /// Task completed successfully
    Completed,
    /// Error state
    Error(String),
}

/// Configuration for the agent
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub max_iterations: usize,
    pub verbose: bool,
    pub system_prompt: String,
    pub model_name: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_iterations: 100,
            verbose: false,
            system_prompt: "You are a helpful coding assistant.".to_string(),
            model_name: "llama3.1:8b".to_string(),
        }
    }
}

/// Context for the agent's execution
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub user_prompt: String,
    pub conversation_history: Vec<ChatMessage>,
    pub app_state_id: String,
}

/// Result of running the agent
#[derive(Debug, Clone)]
pub struct AgentRunResult {
    /// Final state of the agent
    pub final_state: AgentState,
    /// Number of iterations executed
    pub iterations: usize,
    /// Whether the task was completed successfully
    pub task_completed: bool,
}

pub struct AgentLoop {
    state: AgentState,
    config: AgentConfig,
    iterations: usize,
    model_provider: Box<dyn ModelProvider>,
    tool_registry: ToolRegistry,
    conversation_history: Vec<ChatMessage>,
}

impl AgentLoop {
    pub fn new(
        config: AgentConfig,
        model_provider: Box<dyn ModelProvider>,
        tool_registry: ToolRegistry,
    ) -> Self {
        Self {
            state: AgentState::Planning,
            config,
            iterations: 0,
            model_provider,
            tool_registry,
            conversation_history: Vec::new(),
        }
    }

    pub fn state(&self) -> &AgentState {
        &self.state
    }

    pub fn model_provider(&self) -> &dyn ModelProvider {
        self.model_provider.as_ref()
    }

    pub fn tool_registry(&self) -> &ToolRegistry {
        &self.tool_registry
    }

    pub fn conversation_history(&self) -> &[ChatMessage] {
        &self.conversation_history
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Run the agent loop with the given context
    pub async fn run(&mut self, context: AgentContext) -> AgentResult<AgentRunResult> {
        self.conversation_history = context.conversation_history.clone();
        self.iterations = 0;

        loop {
            if self.iterations >= self.config.max_iterations {
                return Err(AgentError::MaxIterationsExceeded);
            }

            if self.config.verbose {
                tracing::info!("Agent iteration {}: {:?}", self.iterations, self.state);
            }

            match &self.state {
                AgentState::Planning => {
                    self.plan(&context).await?;
                    self.transition_to(AgentState::CheckingCompletion);
                }
                AgentState::CheckingCompletion => {
                    if self.check_task_complete(&context).await? {
                        self.transition_to(AgentState::Completed);
                    } else {
                        self.transition_to(AgentState::Deciding);
                    }
                }
                AgentState::Deciding => {
                    let needs_query = self.decide(&context).await?;
                    if needs_query {
                        self.transition_to(AgentState::Querying);
                    } else {
                        self.transition_to(AgentState::Performing);
                    }
                }
                AgentState::Querying => {
                    self.query(&context).await?;
                    self.transition_to(AgentState::Planning);
                }
                AgentState::Performing => {
                    self.perform(&context).await?;
                    self.transition_to(AgentState::CheckingCompletion);
                }
                AgentState::Completed => {
                    return Ok(AgentRunResult {
                        final_state: self.state.clone(),
                        iterations: self.iterations,
                        task_completed: true,
                    });
                }
                AgentState::Error(msg) => {
                    return Err(AgentError::StateError(msg.clone()));
                }
            }

            self.iterations += 1;
        }
    }

    /// Transition to a new state
    fn transition_to(&mut self, new_state: AgentState) {
        if self.config.verbose {
            tracing::debug!("State transition: {:?} → {:?}", self.state, new_state);
        }
        self.state = new_state;
    }

    /// Plan (stub)
    async fn plan(&mut self, _context: &AgentContext) -> AgentResult<()> {
        // Planning logic would go here
        Ok(())
    }

    /// Check if the task is complete
    async fn check_task_complete(&self, _context: &AgentContext) -> AgentResult<bool> {
        // For now, complete after first iteration to avoid infinite loop
        // Real implementation would check if user's requirements are met
        Ok(self.iterations > 0)
    }

    /// Decide (stub - returns whether to query)
    async fn decide(&self, _context: &AgentContext) -> AgentResult<bool> {
        // Decision logic would go here
        Ok(false) // Don't query by default
    }

    /// Query using RAG (stub - calls unimplemented RAG logic)
    async fn query(&self, _context: &AgentContext) -> AgentResult<()> {
        // This will panic when called due to unimplemented!() in rag module
        rag::query().map_err(|e| AgentError::StateError(e.to_string()))
    }

    /// Perform action (stub)
    async fn perform(&mut self, _context: &AgentContext) -> AgentResult<()> {
        // Perform logic would go here
        Ok(())
    }
}

/// Trait for components that can interact with the agent
#[async_trait]
pub trait AgentComponent: Send + Sync {
    /// Initialize the component
    async fn initialize(&mut self) -> AgentResult<()>;

    /// Process a step in the agent loop
    async fn process(&mut self, state: &AgentState) -> AgentResult<()>;

    /// Cleanup the component
    async fn cleanup(&mut self) -> AgentResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::types::{
        ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, MessageRole, ModelInfo,
    };
    use model::ModelResult;

    use crate::tools::{EchoTool, ToolRegistry};

    struct MockProvider;

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            Ok(ChatResponse {
                choices: vec![Choice {
                    message: ChatMessage {
                        role: MessageRole::Assistant,
                        content: Some("Mock response".to_string()),
                        tool_calls: None,
                        tool_call_id: None,
                    },
                    finish_reason: Some(FinishReason::Stop),
                }],
                usage: None,
            })
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

    fn make_agent(config: AgentConfig) -> AgentLoop {
        AgentLoop::new(config, Box::new(MockProvider), ToolRegistry::new())
    }

    #[test]
    fn test_agent_state_transitions() {
        let state = AgentState::Planning;
        assert_eq!(state, AgentState::Planning);

        let state = AgentState::Querying;
        assert_eq!(state, AgentState::Querying);

        let state = AgentState::Completed;
        assert_eq!(state, AgentState::Completed);
    }

    #[test]
    fn test_agent_loop_creation() {
        let agent = make_agent(AgentConfig::default());
        assert_eq!(agent.state(), &AgentState::Planning);
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_iterations, 100);
        assert!(!config.verbose);
    }

    #[tokio::test]
    async fn test_agent_run_completes() {
        let config = AgentConfig {
            max_iterations: 10,
            ..AgentConfig::default()
        };
        let mut agent = make_agent(config);
        let context = AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(result.task_completed);
    }

    #[tokio::test]
    async fn test_agent_max_iterations() {
        let config = AgentConfig {
            max_iterations: 2,
            ..AgentConfig::default()
        };
        let mut agent = make_agent(config);

        agent.state = AgentState::Planning;

        let context = AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok() || matches!(result, Err(AgentError::MaxIterationsExceeded)));
    }

    #[test]
    fn test_agent_stores_model_provider() {
        let agent = make_agent(AgentConfig::default());
        assert_eq!(agent.model_provider().provider_name(), "mock");
    }

    #[test]
    fn test_agent_stores_tool_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let agent = AgentLoop::new(AgentConfig::default(), Box::new(MockProvider), registry);
        assert_eq!(agent.tool_registry().list_tools().len(), 1);
    }

    #[test]
    fn test_agent_conversation_history_starts_empty() {
        let agent = make_agent(AgentConfig::default());
        assert!(agent.conversation_history().is_empty());
    }

    #[tokio::test]
    async fn test_agent_run_seeds_conversation_history() {
        let mut agent = make_agent(AgentConfig {
            max_iterations: 10,
            ..AgentConfig::default()
        });
        let context = AgentContext {
            user_prompt: "test".to_string(),
            conversation_history: vec![
                ChatMessage::user("hello"),
                ChatMessage::assistant("hi there"),
            ],
            app_state_id: "s1".to_string(),
        };

        let _ = agent.run(context).await;
        assert_eq!(agent.conversation_history().len(), 2);
        assert_eq!(agent.conversation_history()[0].role, MessageRole::User);
        assert_eq!(agent.conversation_history()[1].role, MessageRole::Assistant);
    }

    #[test]
    fn test_agent_config_fields() {
        let config = AgentConfig {
            system_prompt: "custom prompt".to_string(),
            model_name: "custom-model".to_string(),
            ..AgentConfig::default()
        };
        let agent = make_agent(config);
        assert_eq!(agent.config().system_prompt, "custom prompt");
        assert_eq!(agent.config().model_name, "custom-model");
    }

    #[test]
    fn test_agent_tool_registry_definitions() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let agent = AgentLoop::new(AgentConfig::default(), Box::new(MockProvider), registry);
        assert_eq!(agent.tool_registry().get_definitions().len(), 1);
    }
}
