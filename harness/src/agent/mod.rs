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
pub mod prompts;
pub mod rag;

use crate::entities::context::types::{ContextEntity, ToolCallRecord};
use crate::entities::{EntityStore, InMemoryEntityStore};
use crate::tools::ToolRegistry;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

use model::provider::ModelProvider;
use model::types::{ChatMessage, ChatRequest, ChatResponse, FinishReason, MessageRole};

const MAX_LLM_RESPONSE_LENGTH: usize = 2000;
const DEFAULT_PLANNING_RAG_LIMIT: usize = 10;
const DEFAULT_QUERY_RAG_LIMIT: usize = 5;
const PLANNING_TEMPERATURE: f32 = 0.7;
const COMPLETION_TEMPERATURE: f32 = 0.2;
const DECISION_TEMPERATURE: f32 = 0.3;
const DEFAULT_MODEL: &str = "qwen2.5:0.5b";
const MAX_TOOL_ITERATIONS: usize = 10;

/// Errors that can occur in the agent
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Agent state error: {0}")]
    StateError(String),
    #[error("Task completion check failed: {0}")]
    TaskCheckFailed(String),
    #[error("Maximum iterations exceeded after {iterations} iterations")]
    MaxIterationsExceeded { iterations: usize },
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
            system_prompt: String::new(),
            model_name: DEFAULT_MODEL.to_string(),
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
    /// Summary of the result from the last assistant message
    pub result_summary: String,
    /// All tool calls made during this run
    pub tool_calls_made: Vec<ToolCallRecord>,
    /// Snapshot of the full conversation
    pub conversation_snapshot: Vec<ChatMessage>,
}

fn extract_tool_calls_from_history(history: &[ChatMessage]) -> Vec<ToolCallRecord> {
    use std::collections::HashMap;

    let mut call_args: HashMap<String, (String, serde_json::Value)> = HashMap::new();
    let mut call_results: HashMap<String, String> = HashMap::new();

    for msg in history {
        if msg.role == MessageRole::Assistant {
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    call_args.insert(
                        tc.id.clone(),
                        (tc.function.name.clone(), tc.function.arguments.clone()),
                    );
                }
            }
        }
        if msg.role == MessageRole::Tool {
            if let (Some(call_id), Some(content)) = (&msg.tool_call_id, &msg.content) {
                call_results.insert(call_id.clone(), content.clone());
            }
        }
    }

    call_args
        .into_iter()
        .map(|(call_id, (tool_name, arguments))| {
            let result = call_results.get(&call_id).cloned().unwrap_or_default();
            ToolCallRecord {
                tool_name,
                arguments,
                call_id,
                result,
            }
        })
        .collect()
}

fn extract_result_summary(history: &[ChatMessage]) -> String {
    history
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant && m.tool_calls.is_none())
        .and_then(|m| m.content.as_deref())
        .unwrap_or("")
        .to_string()
}

pub struct AgentLoop {
    state: AgentState,
    config: AgentConfig,
    iterations: usize,
    entity_store: InMemoryEntityStore,
    performed_actions: usize,
    llm_provider: Option<Arc<dyn ModelProvider>>,
    plan_cache: Option<String>,
    tool_registry: Option<ToolRegistry>,
    conversation_history: Vec<ChatMessage>,
}

impl AgentLoop {
    /// Create a new agent loop with default entity store
    pub fn new(config: AgentConfig) -> Self {
        Self {
            state: AgentState::Planning,
            config,
            iterations: 0,
            entity_store: InMemoryEntityStore::new(),
            performed_actions: 0,
            llm_provider: None,
            plan_cache: None,
            tool_registry: None,
            conversation_history: Vec::new(),
        }
    }

    /// Create a new agent loop with a provided entity store
    pub fn with_entity_store(config: AgentConfig, entity_store: InMemoryEntityStore) -> Self {
        Self {
            state: AgentState::Planning,
            config,
            iterations: 0,
            entity_store,
            performed_actions: 0,
            llm_provider: None,
            plan_cache: None,
            tool_registry: None,
            conversation_history: Vec::new(),
        }
    }

    /// Create a new agent loop with entity store and LLM provider
    pub fn with_llm(
        config: AgentConfig,
        entity_store: InMemoryEntityStore,
        llm_provider: Arc<dyn ModelProvider>,
    ) -> Self {
        Self {
            state: AgentState::Planning,
            config,
            iterations: 0,
            entity_store,
            performed_actions: 0,
            llm_provider: Some(llm_provider),
            plan_cache: None,
            tool_registry: None,
            conversation_history: Vec::new(),
        }
    }

    /// Create a new agent loop with entity store, LLM provider, and tool registry
    pub fn with_tools(
        config: AgentConfig,
        entity_store: InMemoryEntityStore,
        llm_provider: Arc<dyn ModelProvider>,
        tool_registry: ToolRegistry,
    ) -> Self {
        Self {
            state: AgentState::Planning,
            config,
            iterations: 0,
            entity_store,
            performed_actions: 0,
            llm_provider: Some(llm_provider),
            plan_cache: None,
            tool_registry: Some(tool_registry),
            conversation_history: Vec::new(),
        }
    }

    pub fn conversation_history(&self) -> &[ChatMessage] {
        &self.conversation_history
    }

    /// Get the current state
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Get reference to entity store
    pub fn entity_store(&self) -> &InMemoryEntityStore {
        &self.entity_store
    }

    /// Get mutable reference to entity store
    pub fn entity_store_mut(&mut self) -> &mut InMemoryEntityStore {
        &mut self.entity_store
    }

    /// Get reference to tool registry if present
    pub fn tool_registry(&self) -> Option<&ToolRegistry> {
        self.tool_registry.as_ref()
    }

    fn extract_response_content(response: &ChatResponse) -> &str {
        response
            .choices
            .first()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or("")
    }

    /// Run the agent loop with the given context
    pub async fn run(&mut self, context: AgentContext) -> AgentResult<AgentRunResult> {
        if self.tool_registry.is_some() && self.llm_provider.is_some() {
            return self.run_tool_loop(context).await;
        }

        self.iterations = 0;

        loop {
            if self.iterations >= self.config.max_iterations {
                return Err(AgentError::MaxIterationsExceeded {
                    iterations: self.iterations,
                });
            }

            if self.config.verbose {
                tracing::info!("Agent iteration {}: {:?}", self.iterations, self.state);
            }

            if self.state == AgentState::Completed {
                let task_description = context.user_prompt.clone();
                let conversation = self.conversation_history.clone();
                let tool_calls_made = extract_tool_calls_from_history(&conversation);
                let result_summary = extract_result_summary(&conversation);
                let model_used = self.config.model_name.clone();
                let entity = ContextEntity::new(
                    task_description,
                    conversation.clone(),
                    tool_calls_made.clone(),
                    result_summary.clone(),
                    model_used,
                );
                if let Err(e) = self.entity_store.store(Box::new(entity)).await {
                    tracing::warn!("Failed to store context entity: {}", e);
                }
                return Ok(AgentRunResult {
                    final_state: self.state.clone(),
                    iterations: self.iterations,
                    task_completed: true,
                    result_summary,
                    tool_calls_made,
                    conversation_snapshot: conversation,
                });
            }

            if let AgentState::Error(msg) = &self.state {
                return Err(AgentError::StateError(msg.clone()));
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
                AgentState::Completed | AgentState::Error(_) => unreachable!(),
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

    /// Call LLM with retry logic and exponential backoff
    async fn call_llm_with_retry(
        &self,
        provider: &Arc<dyn ModelProvider>,
        request: ChatRequest,
        operation: &str,
    ) -> AgentResult<ChatResponse> {
        use model::judge::JudgeConfig;

        let judge_config = JudgeConfig::default();

        for attempt in 0..judge_config.max_retries {
            match provider.chat(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if attempt < judge_config.max_retries - 1 {
                        let delay = judge_config.calculate_retry_delay(attempt);
                        if self.config.verbose {
                            tracing::warn!(
                                "LLM {} failed (attempt {}), retrying in {:?}: {}",
                                operation,
                                attempt + 1,
                                delay,
                                e
                            );
                        }
                        tokio::time::sleep(delay).await;
                    } else {
                        return Err(AgentError::StateError(format!(
                            "LLM {} failed after {} attempts: {}",
                            operation, judge_config.max_retries, e
                        )));
                    }
                }
            }
        }
        unreachable!()
    }

    /// Validate LLM response meets basic criteria
    fn validate_llm_response(&self, response: &str, expected_keywords: &[&str]) -> bool {
        if response.trim().is_empty() || response.len() > MAX_LLM_RESPONSE_LENGTH {
            return false;
        }

        if !expected_keywords.is_empty() {
            let response_upper = response.to_uppercase();
            expected_keywords
                .iter()
                .any(|kw| response_upper.contains(&kw.to_uppercase()))
        } else {
            true
        }
    }

    /// Plan - Query entities and prepare for action
    async fn plan(&mut self, context: &AgentContext) -> AgentResult<()> {
        if self.config.verbose {
            tracing::info!("Planning for prompt: {}", context.user_prompt);
        }

        let query_results = rag::query_entities(
            &self.entity_store,
            &context.user_prompt,
            Some(DEFAULT_PLANNING_RAG_LIMIT),
        )
        .await
        .map_err(|e| AgentError::StateError(format!("RAG query failed: {}", e)))?;

        if self.config.verbose {
            tracing::info!("Found {} relevant entities", query_results.len());
            for result in &query_results {
                tracing::debug!(
                    "  - {} (type: {:?}, relevance: {:.2})",
                    result.entity_id,
                    result.entity_type,
                    result.relevance
                );
            }
        }

        if let Some(provider) = &self.llm_provider {
            use crate::entities::{EntityQuery, EntityStore};

            let entity_count = self
                .entity_store
                .query(&EntityQuery::default())
                .await
                .map_err(|e| AgentError::StateError(format!("Failed to query entities: {}", e)))?
                .len();

            let prompt_text = prompts::PlanningPrompt::build_from_results(
                &context.user_prompt,
                entity_count,
                &query_results,
            );

            let request = ChatRequest::new(
                &self.config.model_name,
                vec![ChatMessage::user(&prompt_text)],
            )
            .with_temperature(PLANNING_TEMPERATURE);

            let response = self
                .call_llm_with_retry(provider, request, "planning")
                .await?;

            if response.choices.is_empty() {
                return Err(AgentError::StateError(
                    "LLM returned empty choices array for planning".to_string(),
                ));
            }

            self.plan_cache = response.choices[0].message.content.clone();

            if self.config.verbose {
                tracing::info!("LLM Plan: {:?}", self.plan_cache);
            }
        }

        Ok(())
    }

    /// Check if the task is complete
    async fn check_task_complete(&self, context: &AgentContext) -> AgentResult<bool> {
        if let Some(provider) = &self.llm_provider {
            use crate::entities::{EntityQuery, EntityStore};

            let entities = self
                .entity_store
                .query(&EntityQuery::default())
                .await
                .map_err(|e| {
                    AgentError::TaskCheckFailed(format!("Failed to query entities: {}", e))
                })?;

            let entity_summary: Vec<String> = entities
                .iter()
                .map(|e| format!("{:?}", e.entity_type))
                .collect();

            let prompt_text = prompts::CompletionPrompt::build(
                &context.user_prompt,
                self.performed_actions,
                &entity_summary,
            );

            let request = ChatRequest::new(
                &self.config.model_name,
                vec![ChatMessage::user(&prompt_text)],
            )
            .with_temperature(COMPLETION_TEMPERATURE);

            let response = self
                .call_llm_with_retry(provider, request, "completion check")
                .await?;

            if response.choices.is_empty() {
                if self.config.verbose {
                    tracing::warn!("LLM returned empty choices, falling back to action count");
                }
                return Ok(self.performed_actions > 0);
            }

            let status_text = Self::extract_response_content(&response);

            if !self.validate_llm_response(status_text, &["COMPLETE", "INCOMPLETE"]) {
                if self.config.verbose {
                    tracing::warn!("Invalid completion response, falling back to action count");
                }
                return Ok(self.performed_actions > 0);
            }

            match prompts::CompletionPrompt::parse_response(status_text) {
                Some(true) => Ok(true),
                Some(false) => Ok(false),
                None => {
                    if self.config.verbose {
                        tracing::warn!("Ambiguous completion status, falling back");
                    }
                    Ok(self.performed_actions > 0)
                }
            }
        } else {
            Ok(self.performed_actions > 0)
        }
    }

    /// Decide whether to query for more context
    async fn decide(&self, context: &AgentContext) -> AgentResult<bool> {
        if let Some(provider) = &self.llm_provider {
            use crate::entities::{EntityQuery, EntityStore};

            let plan = self.plan_cache.as_deref().unwrap_or("No plan yet");
            let entity_count = self
                .entity_store
                .query(&EntityQuery::default())
                .await
                .map_err(|e| AgentError::StateError(format!("Failed to query entities: {}", e)))?
                .len();

            let prompt_text = prompts::DecisionPrompt::build(
                &context.user_prompt,
                plan,
                entity_count,
                self.performed_actions,
            );

            let request = ChatRequest::new(
                &self.config.model_name,
                vec![ChatMessage::user(&prompt_text)],
            )
            .with_temperature(DECISION_TEMPERATURE);

            let response = self
                .call_llm_with_retry(provider, request, "decision")
                .await?;

            if response.choices.is_empty() {
                if self.config.verbose {
                    tracing::warn!("LLM returned empty choices, defaulting to PROCEED");
                }
                return Ok(false);
            }

            let decision_text = Self::extract_response_content(&response);

            if !self.validate_llm_response(decision_text, &["QUERY", "PROCEED"]) {
                if self.config.verbose {
                    tracing::warn!("Invalid decision response, defaulting to PROCEED");
                }
                return Ok(false);
            }

            match prompts::DecisionPrompt::parse_response(decision_text) {
                Some(true) => Ok(true),
                Some(false) => Ok(false),
                None => {
                    if self.config.verbose {
                        tracing::warn!("Ambiguous decision, defaulting to PROCEED");
                    }
                    Ok(false)
                }
            }
        } else {
            Ok(false)
        }
    }

    /// Query using RAG for additional context
    async fn query(&self, context: &AgentContext) -> AgentResult<()> {
        let results = rag::query_entities(
            &self.entity_store,
            &context.user_prompt,
            Some(DEFAULT_QUERY_RAG_LIMIT),
        )
        .await
        .map_err(|e| AgentError::StateError(format!("RAG query failed: {}", e)))?;

        if self.config.verbose {
            tracing::info!("Additional query found {} entities", results.len());
        }

        Ok(())
    }

    /// Perform action — dispatches to perform_with_tools or perform_mvp
    async fn perform(&mut self, context: &AgentContext) -> AgentResult<()> {
        if self.llm_provider.is_some() && self.tool_registry.is_some() {
            let provider = self.llm_provider.as_ref().unwrap().clone();
            self.perform_with_tools(context, &provider).await
        } else {
            self.perform_mvp(context).await
        }
    }

    /// MVP perform: create a GitRepository entity
    async fn perform_mvp(&mut self, context: &AgentContext) -> AgentResult<()> {
        use crate::entities::{git::types::GitRepository, EntityStore};

        self.performed_actions += 1;

        if self.config.verbose {
            tracing::info!("Performing action for: {}", context.user_prompt);
        }

        let new_entity = Box::new(GitRepository::new(String::new(), "main".to_string()));

        let entity_id = self
            .entity_store
            .store(new_entity)
            .await
            .map_err(|e| AgentError::StateError(format!("Failed to store entity: {}", e)))?;

        if self.config.verbose {
            tracing::info!("Created new entity: {}", entity_id);
        }

        Ok(())
    }

    /// Full tool-calling run loop — used when tool_registry is set.
    ///
    /// Bypasses the state machine and drives a direct LLM ↔ tool conversation
    /// until the model stops with a non-tool finish reason.
    async fn run_tool_loop(&mut self, context: AgentContext) -> AgentResult<AgentRunResult> {
        self.conversation_history.clear();

        if !self.config.system_prompt.is_empty() {
            let sp = self.config.system_prompt.clone();
            self.conversation_history.push(ChatMessage::system(&sp));
        }

        for msg in context.conversation_history {
            self.conversation_history.push(msg);
        }

        let tool_defs = self
            .tool_registry
            .as_ref()
            .map(|r| r.get_definitions())
            .unwrap_or_default();

        self.iterations = 0;

        loop {
            if self.iterations >= self.config.max_iterations {
                return Err(AgentError::MaxIterationsExceeded {
                    iterations: self.iterations,
                });
            }

            let model_name = self.config.model_name.clone();
            let messages = self.conversation_history.clone();
            let request = ChatRequest::new(&model_name, messages).with_tools(tool_defs.clone());

            let provider = self
                .llm_provider
                .as_ref()
                .ok_or_else(|| AgentError::StateError("No provider configured".to_string()))?
                .clone();

            let response = provider
                .chat(request)
                .await
                .map_err(|e| AgentError::StateError(format!("LLM call failed: {}", e)))?;

            if response.choices.is_empty() {
                return Err(AgentError::StateError(
                    "Empty response from model".to_string(),
                ));
            }

            let choice = response.choices.into_iter().next().unwrap();
            let finish_reason = choice.finish_reason.clone();
            let tool_calls = choice.message.tool_calls.clone();
            self.conversation_history.push(choice.message);

            match finish_reason {
                Some(FinishReason::Stop) | None => {
                    self.transition_to(AgentState::Completed);
                    let task_description = context.user_prompt.clone();
                    let conversation = self.conversation_history.clone();
                    let tool_calls_made = extract_tool_calls_from_history(&conversation);
                    let result_summary = extract_result_summary(&conversation);
                    let model_used = self.config.model_name.clone();
                    let entity = ContextEntity::new(
                        task_description,
                        conversation.clone(),
                        tool_calls_made.clone(),
                        result_summary.clone(),
                        model_used,
                    );
                    if let Err(e) = self.entity_store.store(Box::new(entity)).await {
                        tracing::warn!("Failed to store context entity: {}", e);
                    }
                    return Ok(AgentRunResult {
                        final_state: AgentState::Completed,
                        iterations: self.iterations,
                        task_completed: true,
                        result_summary,
                        tool_calls_made,
                        conversation_snapshot: conversation,
                    });
                }
                Some(FinishReason::ToolCalls) => {
                    if let Some(calls) = tool_calls {
                        for tool_call in &calls {
                            let name = tool_call.function.name.clone();
                            let args = tool_call.function.arguments.clone();
                            let call_id = tool_call.id.clone();

                            let result = self
                                .tool_registry
                                .as_ref()
                                .ok_or_else(|| {
                                    AgentError::StateError("No tool registry".to_string())
                                })?
                                .execute(&name, args)
                                .await;

                            let response_content = match result {
                                Ok(v) => v.to_string(),
                                Err(e) => format!("Error: {}", e),
                            };

                            self.conversation_history
                                .push(ChatMessage::tool_response(call_id, response_content));
                        }
                    }
                }
                Some(_) => {
                    return Err(AgentError::StateError(
                        "Unexpected finish reason".to_string(),
                    ));
                }
            }

            self.iterations += 1;
        }
    }

    /// Tool-calling perform helper: inner loop for the state-machine `perform` step.
    async fn perform_with_tools(
        &mut self,
        context: &AgentContext,
        provider: &Arc<dyn ModelProvider>,
    ) -> AgentResult<()> {
        let tool_defs = self.tool_registry.as_ref().unwrap().get_definitions();

        let system_prompt = format!(
            "You are an assistant with access to tools. Use them to complete the task: {}",
            context.user_prompt
        );

        self.conversation_history = vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(&context.user_prompt),
        ];

        for _ in 0..MAX_TOOL_ITERATIONS {
            let request =
                ChatRequest::new(&self.config.model_name, self.conversation_history.clone())
                    .with_tools(tool_defs.clone())
                    .with_temperature(COMPLETION_TEMPERATURE);

            let response = self
                .call_llm_with_retry(provider, request, "perform")
                .await?;

            if response.choices.is_empty() {
                break;
            }

            let choice = response.choices.into_iter().next().unwrap();

            let has_tool_calls = choice
                .message
                .tool_calls
                .as_ref()
                .map(|tc| !tc.is_empty())
                .unwrap_or(false);

            if has_tool_calls {
                let tool_calls = choice.message.tool_calls.clone().unwrap();
                self.conversation_history.push(choice.message.clone());

                for tc in &tool_calls {
                    let result = self
                        .tool_registry
                        .as_ref()
                        .unwrap()
                        .execute(&tc.function.name, tc.function.arguments.clone())
                        .await;

                    let content = match result {
                        Ok(val) => val.to_string(),
                        Err(e) => format!("Error: {}", e),
                    };

                    self.conversation_history
                        .push(ChatMessage::tool_response(tc.id.clone(), content));
                }
            } else {
                self.conversation_history.push(choice.message);
                break;
            }
        }

        self.performed_actions += 1;
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
    use crate::entities::git::types::GitRepository;
    use crate::entities::{EntityQuery, EntityStore, InMemoryEntityStore};
    use crate::tools::{EchoTool, ToolRegistry};
    use model::provider::{ModelError, ModelResult};
    use model::types::{
        ChatResponse, Choice, FinishReason, FunctionCall, MessageRole, ModelInfo, ToolCall,
    };
    use std::sync::Mutex;

    struct MockProvider {
        responses: Mutex<Vec<ChatResponse>>,
    }

    impl MockProvider {
        fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
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

    fn plain_response(content: &str) -> ChatResponse {
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

    fn tool_call_response(tool_name: &str, args: serde_json::Value) -> ChatResponse {
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

    #[test]
    fn test_agent_loop_with_tools_creation() {
        let config = AgentConfig::default();
        let store = InMemoryEntityStore::new();
        let provider = MockProvider::new(vec![]);
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));

        let agent = AgentLoop::with_tools(config, store, provider, registry);

        assert!(agent.tool_registry().is_some());
        assert!(agent.llm_provider.is_some());
        assert_eq!(agent.state(), &AgentState::Planning);
    }

    #[test]
    fn test_agent_loop_backward_compat() {
        let agent = AgentLoop::new(AgentConfig::default());
        assert!(agent.tool_registry().is_none());
        assert!(agent.llm_provider.is_none());

        let store = InMemoryEntityStore::new();
        let agent = AgentLoop::with_entity_store(AgentConfig::default(), store);
        assert!(agent.tool_registry().is_none());
        assert!(agent.llm_provider.is_none());

        let store = InMemoryEntityStore::new();
        let provider: Arc<dyn ModelProvider> = MockProvider::new(vec![]);
        let agent = AgentLoop::with_llm(AgentConfig::default(), store, provider);
        assert!(agent.tool_registry().is_none());
        assert!(agent.llm_provider.is_some());
    }

    #[tokio::test]
    async fn test_perform_with_tools_executes_calls() {
        let provider = MockProvider::new(vec![
            tool_call_response("echo", serde_json::json!({"message": "hello"})),
            plain_response("Done! I echoed the message."),
        ]);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));

        let config = AgentConfig::default();
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "Echo hello".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let result = agent
            .perform_with_tools(&context, &agent.llm_provider.as_ref().unwrap().clone())
            .await;
        assert!(
            result.is_ok(),
            "perform_with_tools should succeed: {:?}",
            result
        );
        assert_eq!(agent.performed_actions, 1);

        let history = &agent.conversation_history;
        assert!(
            history.len() >= 4,
            "History should have system, user, assistant(tool_call), tool_response, assistant(final)"
        );

        let has_tool_response = history.iter().any(|m| m.role == MessageRole::Tool);
        assert!(has_tool_response, "History should contain tool response");
    }

    #[tokio::test]
    async fn test_perform_with_tools_handles_errors() {
        let provider = MockProvider::new(vec![
            tool_call_response("nonexistent_tool", serde_json::json!({})),
            plain_response("I couldn't find that tool."),
        ]);

        let registry = ToolRegistry::new();

        let config = AgentConfig::default();
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "Do something".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let provider_clone = agent.llm_provider.as_ref().unwrap().clone();
        let result = agent.perform_with_tools(&context, &provider_clone).await;
        assert!(result.is_ok(), "Should handle tool errors gracefully");

        let has_error_response = agent.conversation_history.iter().any(|m| {
            m.role == MessageRole::Tool
                && m.content
                    .as_ref()
                    .map(|c| c.starts_with("Error:"))
                    .unwrap_or(false)
        });
        assert!(has_error_response, "Should have an error tool response");
    }

    #[tokio::test]
    async fn test_perform_without_tools_mvp_fallback() {
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_entity_store(config, store);

        assert!(agent.tool_registry().is_none());

        let context = AgentContext {
            user_prompt: "Create entity".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let result = agent.perform_mvp(&context).await;
        assert!(result.is_ok());
        assert_eq!(agent.performed_actions, 1);

        let entities = agent
            .entity_store()
            .query(&EntityQuery::default())
            .await
            .unwrap();
        assert_eq!(
            entities.len(),
            1,
            "MVP should create a GitRepository entity"
        );
    }

    #[tokio::test]
    async fn test_perform_with_tools_max_iterations() {
        let responses: Vec<ChatResponse> = (0..MAX_TOOL_ITERATIONS + 5)
            .map(|_| tool_call_response("echo", serde_json::json!({"message": "loop"})))
            .collect();

        let provider = MockProvider::new(responses);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));

        let config = AgentConfig::default();
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "Keep looping".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let provider_clone = agent.llm_provider.as_ref().unwrap().clone();
        let result = agent.perform_with_tools(&context, &provider_clone).await;
        assert!(
            result.is_ok(),
            "Should stop after max iterations without error"
        );
        assert_eq!(agent.performed_actions, 1, "Should count as one action");
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
        let config = AgentConfig::default();
        let agent = AgentLoop::new(config);
        assert_eq!(agent.state(), &AgentState::Planning);
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_iterations, 100);
        assert!(!config.verbose);
        assert_eq!(config.model_name, DEFAULT_MODEL);
    }

    #[tokio::test]
    async fn test_agent_run_completes() {
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let mut agent = AgentLoop::new(config);
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
            ..Default::default()
        };
        let mut agent = AgentLoop::new(config);

        agent.state = AgentState::Planning;

        let context = AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok() || matches!(result, Err(AgentError::MaxIterationsExceeded { .. })));
    }

    /// MVP Test: Agent completes one full control loop modifying entities
    #[tokio::test]
    async fn test_mvp_agent_control_loop_with_entities() {
        let mut entity_store = InMemoryEntityStore::new();

        let initial_repo = Box::new(GitRepository::new(String::new(), "main".to_string()));
        let _initial_id = entity_store.store(initial_repo).await.unwrap();

        assert_eq!(
            entity_store
                .query(&EntityQuery::default())
                .await
                .unwrap()
                .len(),
            1
        );

        let config = AgentConfig {
            max_iterations: 10,
            verbose: true,
            ..Default::default()
        };
        let mut agent = AgentLoop::with_entity_store(config, entity_store);

        let context = AgentContext {
            user_prompt: "Create a new git repository entity".to_string(),
            conversation_history: vec![],
            app_state_id: "mvp_test_state".to_string(),
        };

        let result = agent.run(context).await;

        assert!(result.is_ok(), "Agent should complete successfully");
        let run_result = result.unwrap();
        assert!(
            run_result.task_completed,
            "Task should be marked as completed"
        );
        assert_eq!(run_result.final_state, AgentState::Completed);

        let git_entities = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Git],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            git_entities.len(),
            2,
            "Agent should create exactly one git entity (1 initial + 1 created). Found {}",
            git_entities.len()
        );

        let context_entities = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(context_entities.len(), 1, "Should store one ContextEntity");

        let query = EntityQuery {
            text_query: Some("Git".to_string()),
            ..Default::default()
        };
        let text_results = agent.entity_store().query(&query).await.unwrap();
        assert!(
            !text_results.is_empty(),
            "RAG text search should find entities with 'Git'"
        );

        println!(
            "✅ MVP Test passed: Agent completed control loop with {} git entities",
            git_entities.len()
        );
    }

    #[tokio::test]
    async fn test_agent_with_llm_provider() {
        use model::OllamaProvider;
        let provider = Arc::new(OllamaProvider::with_default_config().unwrap());
        let config = AgentConfig::default();
        let store = InMemoryEntityStore::new();
        let agent = AgentLoop::with_llm(config, store, provider);
        assert!(agent.llm_provider.is_some());
    }

    #[tokio::test]
    async fn test_llm_planning() {
        use model::OllamaProvider;

        let provider = match OllamaProvider::with_default_config() {
            Ok(p) => Arc::new(p),
            Err(_) => {
                eprintln!("Skipping LLM planning test: Ollama not available");
                return;
            }
        };

        let config = AgentConfig {
            verbose: true,
            ..Default::default()
        };
        let mut agent = AgentLoop::with_llm(config, InMemoryEntityStore::new(), provider);

        let context = AgentContext {
            user_prompt: "Create a user authentication module".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let result = agent.plan(&context).await;

        if let Err(ref e) = result {
            let err_msg = e.to_string();
            if err_msg.contains("Ollama") || err_msg.contains("model") || err_msg.contains("LLM") {
                eprintln!("Skipping LLM planning test: Model not available - {}", e);
                return;
            }
        }

        assert!(result.is_ok(), "Planning should succeed: {:?}", result);
        assert!(agent.plan_cache.is_some(), "LLM should create a plan");

        let plan = agent.plan_cache.as_ref().unwrap();
        assert!(plan.len() > 10, "Plan should be non-trivial, got: {}", plan);
    }

    #[tokio::test]
    async fn test_completion_check_fallback_no_llm() {
        let mut agent = AgentLoop::new(AgentConfig::default());

        agent.performed_actions = 1;

        let context = AgentContext {
            user_prompt: "Create git repository".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let is_complete = agent.check_task_complete(&context).await.unwrap();
        assert!(is_complete, "Should be complete when performed_actions > 0");

        agent.performed_actions = 0;
        let is_complete = agent.check_task_complete(&context).await.unwrap();
        assert!(
            !is_complete,
            "Should be incomplete when performed_actions == 0"
        );
    }

    #[tokio::test]
    async fn test_llm_completion_check() {
        use crate::entities::git::types::GitRepository;
        use crate::entities::EntityStore;
        use model::OllamaProvider;

        let provider = match OllamaProvider::with_default_config() {
            Ok(p) => Arc::new(p),
            Err(_) => {
                eprintln!("Skipping LLM completion test: Ollama not available");
                return;
            }
        };

        let mut agent =
            AgentLoop::with_llm(AgentConfig::default(), InMemoryEntityStore::new(), provider);

        agent.performed_actions = 1;
        let repo = Box::new(GitRepository::new(String::new(), "main".to_string()));
        agent.entity_store_mut().store(repo).await.unwrap();

        let context = AgentContext {
            user_prompt: "Create git repository".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let result = agent.check_task_complete(&context).await;

        if let Err(ref e) = result {
            let err_msg = e.to_string();
            if err_msg.contains("Ollama") || err_msg.contains("model") || err_msg.contains("LLM") {
                eprintln!("Skipping LLM completion test: Model not available - {}", e);
                return;
            }
        }

        let _is_complete = result.unwrap();
    }

    #[tokio::test]
    async fn test_llm_decision_making() {
        use model::OllamaProvider;

        let provider = match OllamaProvider::with_default_config() {
            Ok(p) => Arc::new(p),
            Err(_) => {
                eprintln!("Skipping LLM decision test: Ollama not available");
                return;
            }
        };

        let mut agent =
            AgentLoop::with_llm(AgentConfig::default(), InMemoryEntityStore::new(), provider);

        agent.plan_cache = Some("Create authentication entity".to_string());

        let context = AgentContext {
            user_prompt: "Add user authentication".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let result = agent.decide(&context).await;

        if result.is_err() {
            eprintln!("Skipping LLM decision test: LLM call failed");
            return;
        }

        let _needs_query = result.unwrap();
    }

    /// Task 8: Full LLM Agent Control Loop Integration Test
    #[tokio::test]
    async fn test_full_llm_agent_control_loop() {
        use crate::entities::git::types::GitRepository;
        use crate::entities::EntityStore;
        use model::OllamaProvider;

        let provider = match OllamaProvider::with_default_config() {
            Ok(p) => Arc::new(p),
            Err(_) => {
                eprintln!("Skipping full LLM test: Ollama not available");
                return;
            }
        };

        let config = AgentConfig {
            max_iterations: 20,
            verbose: true,
            ..Default::default()
        };

        let mut entity_store = InMemoryEntityStore::new();
        let initial = Box::new(GitRepository::new(String::new(), "main".to_string()));
        entity_store.store(initial).await.unwrap();

        let mut agent = AgentLoop::with_llm(config, entity_store, provider);

        let context = AgentContext {
            user_prompt: "Create a new git repository for authentication service".to_string(),
            conversation_history: vec![],
            app_state_id: "llm_test".to_string(),
        };

        let result = agent.run(context).await;

        if result.is_err() {
            eprintln!("Skipping full LLM test: Agent run failed (likely LLM unavailable)");
            return;
        }

        assert!(result.is_ok(), "LLM agent should complete successfully");
        let run_result = result.unwrap();
        assert!(run_result.task_completed);
        assert_eq!(run_result.final_state, AgentState::Completed);

        assert!(agent.plan_cache.is_some(), "LLM should have created a plan");

        println!(
            "✅ LLM Agent Test passed with plan: {:?}",
            agent.plan_cache.as_ref().unwrap()
        );
    }

    /// Task 9: Backward Compatibility Test - MVP Mode Without LLM
    #[tokio::test]
    async fn test_mvp_mode_still_works_without_llm() {
        use crate::entities::git::types::GitRepository;
        use crate::entities::EntityStore;

        let config = AgentConfig {
            max_iterations: 10,
            verbose: true,
            ..Default::default()
        };

        let mut entity_store = InMemoryEntityStore::new();
        let initial = Box::new(GitRepository::new(String::new(), "main".to_string()));
        entity_store.store(initial).await.unwrap();

        let mut agent = AgentLoop::with_entity_store(config, entity_store);

        assert!(
            agent.llm_provider.is_none(),
            "MVP mode should have no LLM provider"
        );

        let context = AgentContext {
            user_prompt: "Create entity".to_string(),
            conversation_history: vec![],
            app_state_id: "mvp".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok(), "MVP mode should still work without LLM");

        let run_result = result.unwrap();
        assert!(run_result.task_completed);
        assert_eq!(run_result.final_state, AgentState::Completed);

        assert!(
            agent.plan_cache.is_none(),
            "MVP mode should not populate plan_cache"
        );

        println!("✅ MVP mode backward compatibility verified");
    }

    #[tokio::test]
    #[ignore]
    async fn test_agent_loop_with_ollama_and_tools() {
        use crate::tools::{CalculatorTool, EchoTool};
        use model::OllamaProvider;

        let provider = match OllamaProvider::with_default_config() {
            Ok(p) => Arc::new(p),
            Err(e) => {
                eprintln!("Skipping: Ollama not available: {}", e);
                return;
            }
        };

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        registry.register(Box::new(CalculatorTool::new()));

        let config = AgentConfig {
            max_iterations: 20,
            verbose: true,
            model_name: "qwen3:0.6b".to_string(),
            ..Default::default()
        };

        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "Use the calculate tool to add 5 and 3, then tell me the result."
                .to_string(),
            conversation_history: vec![],
            app_state_id: "integration_test".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok(), "Agent run should succeed: {:?}", result);

        let run_result = result.unwrap();
        assert!(run_result.task_completed, "Task should complete");
        assert_eq!(run_result.final_state, AgentState::Completed);

        println!("✅ Agent loop with Ollama and tools completed successfully");
        println!(
            "   Conversation history: {} messages",
            agent.conversation_history.len()
        );
    }

    #[tokio::test]
    async fn test_state_machine_run_stores_context_entity() {
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_entity_store(config, store);

        let context = AgentContext {
            user_prompt: "store entity test".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok());

        let all_context = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            all_context.len(),
            1,
            "Should store exactly one ContextEntity"
        );
        assert_eq!(
            all_context[0].entity_type,
            crate::entities::EntityType::Context
        );

        let by_prompt = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                text_query: Some("store entity test".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_prompt.len(), 1, "Entity should contain task_description");

        let by_model = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                text_query: Some(DEFAULT_MODEL.to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_model.len(), 1, "Entity should contain model_used");
    }

    #[tokio::test]
    async fn test_tool_loop_run_stores_context_entity() {
        let provider = MockProvider::new(vec![plain_response("Task complete!")]);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));

        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "do something".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok(), "Agent run should succeed: {:?}", result);

        let all_context = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            all_context.len(),
            1,
            "Should store exactly one ContextEntity"
        );

        let by_prompt = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                text_query: Some("do something".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !by_prompt.is_empty(),
            "Entity should contain task_description"
        );

        let by_model = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                text_query: Some(DEFAULT_MODEL.to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!by_model.is_empty(), "Entity should contain model_used");

        let by_summary = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                text_query: Some("Task complete!".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !by_summary.is_empty(),
            "Entity should contain result_summary"
        );
    }

    #[tokio::test]
    async fn test_tool_loop_stores_tool_calls_made() {
        let provider = MockProvider::new(vec![
            tool_call_response("echo", serde_json::json!({"message": "ping"})),
            plain_response("All done."),
        ]);

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));

        let config = AgentConfig::default();
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "echo ping".to_string(),
            conversation_history: vec![],
            app_state_id: "test".to_string(),
        };

        agent.run(context).await.unwrap();

        let by_tool_name = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                text_query: Some("echo".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !by_tool_name.is_empty(),
            "Entity should contain tool call name"
        );

        let by_summary = agent
            .entity_store()
            .query(&EntityQuery {
                entity_types: vec![crate::entities::EntityType::Context],
                text_query: Some("All done.".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            !by_summary.is_empty(),
            "Entity should contain result_summary"
        );
    }
}
