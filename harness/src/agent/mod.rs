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
use model::types::{ChatMessage, ChatRequest, FinishReason, MessageRole};
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
    last_finish_reason: Option<FinishReason>,
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
            last_finish_reason: None,
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
                        self.transition_to(AgentState::Performing);
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
                    self.transition_to(AgentState::Planning);
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

    async fn plan(&mut self, _context: &AgentContext) -> AgentResult<()> {
        if self.conversation_history.is_empty()
            || self.conversation_history[0].role != MessageRole::System
        {
            self.conversation_history
                .insert(0, ChatMessage::system(&self.config.system_prompt));
        }

        let request = ChatRequest::new(&self.config.model_name, self.conversation_history.clone())
            .with_tools(self.tool_registry.get_definitions());

        let response = self.model_provider.chat(request).await?;
        let choice = &response.choices[0];

        self.last_finish_reason = choice.finish_reason.clone();
        self.conversation_history.push(choice.message.clone());

        Ok(())
    }

    async fn check_task_complete(&self, _context: &AgentContext) -> AgentResult<bool> {
        if self.conversation_history.is_empty() {
            return Ok(false);
        }

        let last_message = self.conversation_history.last().unwrap();

        if let Some(FinishReason::Stop) = &self.last_finish_reason {
            if last_message.tool_calls.is_none()
                || last_message.tool_calls.as_ref().unwrap().is_empty()
            {
                return Ok(true);
            }
        }

        Ok(false)
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

    async fn perform(&mut self, _context: &AgentContext) -> AgentResult<()> {
        let tool_calls = self
            .conversation_history
            .last()
            .and_then(|msg| msg.tool_calls.clone());

        if let Some(tool_calls) = tool_calls {
            if tool_calls.is_empty() {
                return Ok(());
            }

            for tool_call in &tool_calls {
                match self
                    .tool_registry
                    .execute(
                        &tool_call.function.name,
                        tool_call.function.arguments.clone(),
                    )
                    .await
                {
                    Ok(result) => {
                        self.conversation_history.push(ChatMessage::tool_response(
                            &tool_call.id,
                            result.to_string(),
                        ));
                    }
                    Err(e) => {
                        self.conversation_history.push(ChatMessage::tool_response(
                            &tool_call.id,
                            format!("Error: {}", e),
                        ));
                    }
                }
            }
        }

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
        ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, MessageRole,
        ModelInfo, ToolCall,
    };
    use model::{ModelError, ModelResult};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    use crate::tools::{EchoTool, ToolRegistry};

    struct MockProvider;

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            Ok(make_stop_response("Mock response"))
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

    struct CapturingMockProvider {
        response: ChatResponse,
        captured_requests: Arc<Mutex<Vec<ChatRequest>>>,
    }

    impl CapturingMockProvider {
        fn new(response: ChatResponse) -> (Self, Arc<Mutex<Vec<ChatRequest>>>) {
            let captured = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    response,
                    captured_requests: captured.clone(),
                },
                captured,
            )
        }
    }

    #[async_trait]
    impl ModelProvider for CapturingMockProvider {
        async fn chat(&self, request: ChatRequest) -> ModelResult<ChatResponse> {
            self.captured_requests.lock().unwrap().push(request);
            Ok(self.response.clone())
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "capturing_mock"
        }
    }

    struct FailingMockProvider;

    #[async_trait]
    impl ModelProvider for FailingMockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            Err(ModelError::ServiceUnavailable {
                message: "mock error".to_string(),
            })
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "failing_mock"
        }
    }

    struct SequenceMockProvider {
        responses: Mutex<Vec<ChatResponse>>,
    }

    impl SequenceMockProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for SequenceMockProvider {
        async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
            let mut responses = self.responses.lock().unwrap();
            Ok(responses.remove(0))
        }

        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }

        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }

        fn provider_name(&self) -> &'static str {
            "sequence_mock"
        }
    }

    fn make_agent(config: AgentConfig) -> AgentLoop {
        AgentLoop::new(config, Box::new(MockProvider), ToolRegistry::new())
    }

    fn make_context() -> AgentContext {
        AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        }
    }

    fn make_tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args,
            },
        }
    }

    fn make_stop_response(content: &str) -> ChatResponse {
        ChatResponse {
            choices: vec![Choice {
                message: ChatMessage::assistant(content),
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        }
    }

    // --- Existing tests (updated where necessary) ---

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
        let context = make_context();

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

        let context = make_context();

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
        assert!(agent.conversation_history().len() >= 4);
        assert_eq!(agent.conversation_history()[0].role, MessageRole::System);
        assert_eq!(agent.conversation_history()[1].role, MessageRole::User);
        assert_eq!(agent.conversation_history()[2].role, MessageRole::Assistant);
        assert_eq!(agent.conversation_history()[3].role, MessageRole::Assistant);
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

    // --- check_task_complete tests ---

    #[tokio::test]
    async fn test_check_complete_with_stop_and_no_tools() {
        let mut agent = make_agent(AgentConfig::default());
        agent.last_finish_reason = Some(FinishReason::Stop);
        agent
            .conversation_history
            .push(ChatMessage::assistant("Done"));
        let context = make_context();
        assert!(agent.check_task_complete(&context).await.unwrap());
    }

    #[tokio::test]
    async fn test_check_incomplete_with_tool_calls() {
        let mut agent = make_agent(AgentConfig::default());
        agent.last_finish_reason = Some(FinishReason::ToolCalls);
        agent
            .conversation_history
            .push(ChatMessage::assistant_with_tools(
                None,
                vec![make_tool_call("c1", "echo", json!({"message": "hi"}))],
            ));
        let context = make_context();
        assert!(!agent.check_task_complete(&context).await.unwrap());
    }

    #[tokio::test]
    async fn test_check_incomplete_with_stop_but_has_tools() {
        let mut agent = make_agent(AgentConfig::default());
        agent.last_finish_reason = Some(FinishReason::Stop);
        agent
            .conversation_history
            .push(ChatMessage::assistant_with_tools(
                Some("thinking".to_string()),
                vec![make_tool_call("c1", "echo", json!({"message": "hi"}))],
            ));
        let context = make_context();
        assert!(!agent.check_task_complete(&context).await.unwrap());
    }

    #[tokio::test]
    async fn test_check_incomplete_with_length_finish() {
        let mut agent = make_agent(AgentConfig::default());
        agent.last_finish_reason = Some(FinishReason::Length);
        agent
            .conversation_history
            .push(ChatMessage::assistant("truncated"));
        let context = make_context();
        assert!(!agent.check_task_complete(&context).await.unwrap());
    }

    #[tokio::test]
    async fn test_check_incomplete_with_content_filter() {
        let mut agent = make_agent(AgentConfig::default());
        agent.last_finish_reason = Some(FinishReason::ContentFilter);
        agent
            .conversation_history
            .push(ChatMessage::assistant("filtered"));
        let context = make_context();
        assert!(!agent.check_task_complete(&context).await.unwrap());
    }

    #[tokio::test]
    async fn test_check_incomplete_with_no_finish_reason() {
        let mut agent = make_agent(AgentConfig::default());
        agent.last_finish_reason = None;
        agent
            .conversation_history
            .push(ChatMessage::assistant("no reason"));
        let context = make_context();
        assert!(!agent.check_task_complete(&context).await.unwrap());
    }

    #[tokio::test]
    async fn test_check_complete_with_empty_conversation() {
        let agent = make_agent(AgentConfig::default());
        let context = make_context();
        assert!(!agent.check_task_complete(&context).await.unwrap());
    }

    // --- plan tests ---

    #[tokio::test]
    async fn test_plan_calls_model_with_system_prompt() {
        let (provider, captured) = CapturingMockProvider::new(make_stop_response("response"));
        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Box::new(provider),
            ToolRegistry::new(),
        );
        let context = make_context();
        agent.plan(&context).await.unwrap();

        let requests = captured.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages[0].role, MessageRole::System);
        assert_eq!(
            requests[0].messages[0].content.as_deref(),
            Some(AgentConfig::default().system_prompt.as_str()),
        );
    }

    #[tokio::test]
    async fn test_plan_appends_assistant_response() {
        let (provider, _) = CapturingMockProvider::new(make_stop_response("assistant says hello"));
        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Box::new(provider),
            ToolRegistry::new(),
        );
        let context = make_context();
        agent.plan(&context).await.unwrap();

        let last = agent.conversation_history().last().unwrap();
        assert_eq!(last.role, MessageRole::Assistant);
        assert_eq!(last.content.as_deref(), Some("assistant says hello"));
    }

    #[tokio::test]
    async fn test_plan_with_empty_conversation_history() {
        let (provider, captured) = CapturingMockProvider::new(make_stop_response("response"));
        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Box::new(provider),
            ToolRegistry::new(),
        );
        let context = make_context();
        agent.plan(&context).await.unwrap();

        let requests = captured.lock().unwrap();
        assert_eq!(requests[0].messages[0].role, MessageRole::System);
    }

    #[tokio::test]
    async fn test_plan_preserves_existing_system_prompt() {
        let (provider, captured) = CapturingMockProvider::new(make_stop_response("response"));
        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Box::new(provider),
            ToolRegistry::new(),
        );
        agent
            .conversation_history
            .push(ChatMessage::system("existing system prompt"));
        let context = make_context();
        agent.plan(&context).await.unwrap();

        let requests = captured.lock().unwrap();
        let system_count = requests[0]
            .messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .count();
        assert_eq!(system_count, 1);
        assert_eq!(
            requests[0].messages[0].content.as_deref(),
            Some("existing system prompt"),
        );
    }

    #[tokio::test]
    async fn test_plan_includes_tool_definitions() {
        let (provider, captured) = CapturingMockProvider::new(make_stop_response("response"));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let mut agent = AgentLoop::new(AgentConfig::default(), Box::new(provider), registry);
        let context = make_context();
        agent.plan(&context).await.unwrap();

        let requests = captured.lock().unwrap();
        assert!(requests[0].tools.is_some());
        assert_eq!(requests[0].tools.as_ref().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn test_plan_propagates_model_error() {
        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Box::new(FailingMockProvider),
            ToolRegistry::new(),
        );
        let context = make_context();
        let result = agent.plan(&context).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AgentError::ModelError(_)));
    }

    // --- perform tests ---

    #[tokio::test]
    async fn test_perform_executes_single_tool_call() {
        let mut agent = make_agent(AgentConfig::default());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        agent.tool_registry = registry;

        agent
            .conversation_history
            .push(ChatMessage::assistant_with_tools(
                None,
                vec![make_tool_call(
                    "call_1",
                    "echo",
                    json!({"message": "hello"}),
                )],
            ));

        let context = make_context();
        agent.perform(&context).await.unwrap();

        let last = agent.conversation_history().last().unwrap();
        assert_eq!(last.role, MessageRole::Tool);
        assert_eq!(last.tool_call_id.as_deref(), Some("call_1"));
        assert!(last.content.as_ref().unwrap().contains("hello"));
    }

    #[tokio::test]
    async fn test_perform_executes_multiple_tool_calls() {
        let mut agent = make_agent(AgentConfig::default());
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        agent.tool_registry = registry;

        agent
            .conversation_history
            .push(ChatMessage::assistant_with_tools(
                None,
                vec![
                    make_tool_call("call_1", "echo", json!({"message": "first"})),
                    make_tool_call("call_2", "echo", json!({"message": "second"})),
                ],
            ));

        let context = make_context();
        agent.perform(&context).await.unwrap();

        let tool_responses: Vec<_> = agent
            .conversation_history()
            .iter()
            .filter(|m| m.role == MessageRole::Tool)
            .collect();
        assert_eq!(tool_responses.len(), 2);
    }

    #[tokio::test]
    async fn test_perform_handles_tool_execution_error() {
        let mut agent = make_agent(AgentConfig::default());

        agent
            .conversation_history
            .push(ChatMessage::assistant_with_tools(
                None,
                vec![make_tool_call("call_1", "nonexistent", json!({}))],
            ));

        let context = make_context();
        agent.perform(&context).await.unwrap();

        let last = agent.conversation_history().last().unwrap();
        assert_eq!(last.role, MessageRole::Tool);
        assert!(last.content.as_ref().unwrap().contains("Error"));
    }

    #[tokio::test]
    async fn test_perform_with_no_tool_calls() {
        let mut agent = make_agent(AgentConfig::default());
        agent
            .conversation_history
            .push(ChatMessage::assistant("no tools"));

        let context = make_context();
        let history_len = agent.conversation_history().len();
        agent.perform(&context).await.unwrap();
        assert_eq!(agent.conversation_history().len(), history_len);
    }

    #[tokio::test]
    async fn test_perform_with_empty_tool_calls_vec() {
        let mut agent = make_agent(AgentConfig::default());
        agent
            .conversation_history
            .push(ChatMessage::assistant_with_tools(
                Some("thinking".to_string()),
                vec![],
            ));

        let context = make_context();
        let history_len = agent.conversation_history().len();
        agent.perform(&context).await.unwrap();
        assert_eq!(agent.conversation_history().len(), history_len);
    }

    #[tokio::test]
    async fn test_perform_with_no_assistant_message() {
        let mut agent = make_agent(AgentConfig::default());

        let context = make_context();
        agent.perform(&context).await.unwrap();
        assert!(agent.conversation_history().is_empty());
    }

    // --- State machine / integration tests ---

    #[tokio::test]
    async fn test_agent_loop_skips_deciding_state() {
        let tool_call_response = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage::assistant_with_tools(
                    None,
                    vec![make_tool_call("call_1", "echo", json!({"message": "test"}))],
                ),
                finish_reason: Some(FinishReason::ToolCalls),
            }],
            usage: None,
        };
        let stop_response = make_stop_response("Done");

        let provider = SequenceMockProvider::new(vec![tool_call_response, stop_response]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));

        let mut agent = AgentLoop::new(
            AgentConfig {
                max_iterations: 20,
                ..AgentConfig::default()
            },
            Box::new(provider),
            registry,
        );

        let context = make_context();
        let result = agent.run(context).await.unwrap();
        assert!(result.task_completed);
    }

    #[tokio::test]
    async fn test_agent_completes_without_tools() {
        let (provider, _) = CapturingMockProvider::new(make_stop_response("Done"));
        let mut agent = AgentLoop::new(
            AgentConfig {
                max_iterations: 10,
                ..AgentConfig::default()
            },
            Box::new(provider),
            ToolRegistry::new(),
        );

        let context = make_context();
        let result = agent.run(context).await.unwrap();
        assert!(result.task_completed);
    }
}
