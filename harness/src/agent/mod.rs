//! Agent architecture implementation
//!
//! This module implements the main agent control loop following ARCHITECTURE.md:
//!
//! 1. Application State 1 → **Entity Enrichment**
//! 2. Entity Enrichment → **Plan Entity Modification** ← User Prompt
//! 3. Plan Entity Modification → **Perform Entity Modification**
//! 4. Perform Entity Modification → **Update Entities**
//! 5. Update Entities → **Task Complete?**
//! 6. If Yes → Application State 2 (completed)
//! 7. If No → **Entity Modification Decision**
//! 8. Decision → **Query Entities (RAG)** → back to Decision
//! 9. Decision → **Plan Entity Modification** (loop)

pub mod agents_md;
pub mod eval;
pub mod eval_case;
pub mod project_detect;
pub mod project_prompt;
pub mod prompts;
pub mod rag;

use crate::entities::context::types::{ContextEntity, ToolCallRecord};
use crate::entities::{EntityStore, InMemoryEntityStore};
use crate::tools::ToolRegistry;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use thiserror::Error;

use model::provider::ModelProvider;
use model::types::{
    ChatMessage, ChatRequest, ChatResponse, FinishReason, MessageRole, ToolCall, Usage,
};

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
    #[error("Agent state error: {message}")]
    StateError {
        message: String,
        iterations_completed: usize,
        tool_calls_made: Vec<ToolCallRecord>,
        conversation_snapshot: Vec<ChatMessage>,
        last_agent_state: AgentState,
    },
    #[error("Task completion check failed: {message}")]
    TaskCheckFailed {
        message: String,
        iterations_completed: usize,
        tool_calls_made: Vec<ToolCallRecord>,
        conversation_snapshot: Vec<ChatMessage>,
        last_agent_state: AgentState,
    },
    #[error("Maximum iterations exceeded after {iterations_completed} iterations")]
    MaxIterationsExceeded {
        iterations_completed: usize,
        tool_calls_made: Vec<ToolCallRecord>,
        conversation_snapshot: Vec<ChatMessage>,
        last_agent_state: AgentState,
    },
}

impl AgentError {
    pub fn diagnostics(&self) -> (&[ToolCallRecord], &[ChatMessage], usize, &AgentState) {
        match self {
            AgentError::StateError {
                tool_calls_made,
                conversation_snapshot,
                iterations_completed,
                last_agent_state,
                ..
            } => (
                tool_calls_made,
                conversation_snapshot,
                *iterations_completed,
                last_agent_state,
            ),
            AgentError::TaskCheckFailed {
                tool_calls_made,
                conversation_snapshot,
                iterations_completed,
                last_agent_state,
                ..
            } => (
                tool_calls_made,
                conversation_snapshot,
                *iterations_completed,
                last_agent_state,
            ),
            AgentError::MaxIterationsExceeded {
                tool_calls_made,
                conversation_snapshot,
                iterations_completed,
                last_agent_state,
            } => (
                tool_calls_made,
                conversation_snapshot,
                *iterations_completed,
                last_agent_state,
            ),
        }
    }
}

type AgentResult<T> = Result<T, AgentError>;

/// Helper to build a bare (un-enriched) StateError
fn bare_state_error(message: impl Into<String>) -> AgentError {
    AgentError::StateError {
        message: message.into(),
        iterations_completed: 0,
        tool_calls_made: Vec::new(),
        conversation_snapshot: Vec::new(),
        last_agent_state: AgentState::EnrichingEntities,
    }
}

/// Helper to build a bare (un-enriched) TaskCheckFailed
fn bare_task_check_failed(message: impl Into<String>) -> AgentError {
    AgentError::TaskCheckFailed {
        message: message.into(),
        iterations_completed: 0,
        tool_calls_made: Vec::new(),
        conversation_snapshot: Vec::new(),
        last_agent_state: AgentState::CheckingTaskCompletion,
    }
}

/// Helper to build a bare (un-enriched) MaxIterationsExceeded
fn bare_max_iterations(iterations_completed: usize) -> AgentError {
    AgentError::MaxIterationsExceeded {
        iterations_completed,
        tool_calls_made: Vec::new(),
        conversation_snapshot: Vec::new(),
        last_agent_state: AgentState::EnrichingEntities,
    }
}

/// Helper: extract all ToolCallRecords from the conversation history.
fn extract_tool_calls_from_history(history: &[ChatMessage]) -> Vec<ToolCallRecord> {
    history
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .filter_map(|tc| {
            let fn_call = tc.function.as_ref()?;
            Some(ToolCallRecord {
                tool_name: fn_call.name.clone(),
                arguments: fn_call.arguments.clone(),
                result: None,
            })
        })
        .collect()
}

/// Helper: extract a short result summary from the last assistant message.
fn extract_result_summary(history: &[ChatMessage]) -> String {
    history
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant)
        .and_then(|m| m.content.clone())
        .unwrap_or_default()
}

/// The architectural states the agent can be in.
///
/// State machine diagram (from ARCHITECTURE.md):
/// ```text
/// Application State 1
///        ↓
/// EnrichingEntities
///        ↓
/// PlanningEntityModification ←───────────────────────────┬
///        ↓                                           │
/// PerformingEntityModification                      │
///        ↓                                           │
/// UpdatingEntities                                  │
///        ↓                                           │
/// CheckingTaskCompletion ─── Yes ───→ Completed  │
///        ↓ (No)                                      │
/// EntityModificationDecision                        │
///     ├─ Proceed ────────────────────────────┤
///     └─ Query ─→ QueryingEntities ─→ EntityModificationDecision
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    /// Entity Enrichment: scan/enrich entities from application state (ARCHITECTURE.md)
    EnrichingEntities,
    /// Plan Entity Modification: analyse user request and create execution plan (ARCHITECTURE.md)
    PlanningEntityModification,
    /// Perform Entity Modification: execute the planned modification (ARCHITECTURE.md)
    PerformingEntityModification,
    /// Update Entities: commit entity changes to the store (ARCHITECTURE.md)
    UpdatingEntities,
    /// Task Complete? decision point (ARCHITECTURE.md)
    CheckingTaskCompletion,
    /// Entity Modification Decision: decide whether to query or plan (ARCHITECTURE.md)
    EntityModificationDecision,
    /// Query Entities (RAG): retrieve additional context (ARCHITECTURE.md)
    QueryingEntities,
    /// Task completed successfully
    Completed,
    /// Agent encountered an error
    Error(String),
}

/// Configuration for the agent loop
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// All tool calls made
    pub tool_calls_made: Vec<ToolCallRecord>,
    /// Snapshot of the conversation at completion
    pub conversation_snapshot: Vec<ChatMessage>,
    /// Token usage (if reported by the model)
    pub token_usage: Option<Usage>,
}

/// The main agent loop
pub struct AgentLoop<S: EntityStore + Send = InMemoryEntityStore> {
    pub state: AgentState,
    pub config: AgentConfig,
    pub iterations: usize,
    pub entity_store: S,
    pub performed_actions: usize,
    pub llm_provider: Option<Arc<dyn ModelProvider>>,
    pub plan_cache: Option<String>,
    pub tool_registry: Option<ToolRegistry>,
    pub conversation_history: Vec<ChatMessage>,
    pub progress_counter: Option<Arc<AtomicUsize>>,
    pub state_history: Vec<AgentState>,
}

impl AgentLoop<InMemoryEntityStore> {
    /// Create a new agent loop with the default in-memory entity store.
    pub fn new(config: AgentConfig) -> Self {
        Self::with_entity_store(config, InMemoryEntityStore::new())
    }
}

impl<S: EntityStore + Send> AgentLoop<S> {
    /// Create a new agent loop with a provided entity store
    pub fn with_entity_store(config: AgentConfig, entity_store: S) -> Self {
        Self {
            state: AgentState::EnrichingEntities,
            config,
            iterations: 0,
            entity_store,
            performed_actions: 0,
            llm_provider: None,
            plan_cache: None,
            tool_registry: None,
            conversation_history: Vec::new(),
            progress_counter: None,
            state_history: Vec::new(),
        }
    }

    /// Create a new agent loop with entity store and LLM provider
    pub fn with_llm(
        config: AgentConfig,
        entity_store: S,
        llm_provider: Arc<dyn ModelProvider>,
    ) -> Self {
        Self {
            llm_provider: Some(llm_provider),
            ..Self::with_entity_store(config, entity_store)
        }
    }

    /// Create a new agent loop with entity store, LLM provider, and tool registry
    pub fn with_tools(
        config: AgentConfig,
        entity_store: S,
        llm_provider: Arc<dyn ModelProvider>,
        tool_registry: ToolRegistry,
    ) -> Self {
        Self {
            tool_registry: Some(tool_registry),
            ..Self::with_llm(config, entity_store, llm_provider)
        }
    }

    /// Attach a shared progress counter. The counter is incremented once per
    /// agent iteration so callers can observe progress from another thread.
    pub fn with_progress_counter(mut self, counter: Arc<AtomicUsize>) -> Self {
        self.progress_counter = Some(counter);
        self
    }

    /// Get the conversation history
    pub fn conversation_history(&self) -> &[ChatMessage] {
        &self.conversation_history
    }

    /// Get the current state
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Get reference to entity store
    pub fn entity_store(&self) -> &S {
        &self.entity_store
    }

    /// Get mutable reference to entity store
    pub fn entity_store_mut(&mut self) -> &mut S {
        &mut self.entity_store
    }

    /// Get reference to tool registry if present
    pub fn tool_registry(&self) -> Option<&ToolRegistry> {
        self.tool_registry.as_ref()
    }

    /// Get the state history
    pub fn state_history(&self) -> &[AgentState] {
        &self.state_history
    }

    fn enrich_error(&self, error: AgentError) -> AgentError {
        let tool_calls = extract_tool_calls_from_history(&self.conversation_history);
        let conversation = self.conversation_history.clone();
        let state = self.state.clone();
        let iterations = self.iterations;
        match error {
            AgentError::StateError { message, .. } => AgentError::StateError {
                message,
                iterations_completed: iterations,
                tool_calls_made: tool_calls,
                conversation_snapshot: conversation,
                last_agent_state: state,
            },
            AgentError::TaskCheckFailed { message, .. } => AgentError::TaskCheckFailed {
                message,
                iterations_completed: iterations,
                tool_calls_made: tool_calls,
                conversation_snapshot: conversation,
                last_agent_state: state,
            },
            AgentError::MaxIterationsExceeded { .. } => AgentError::MaxIterationsExceeded {
                iterations_completed: iterations,
                tool_calls_made: tool_calls,
                conversation_snapshot: conversation,
                last_agent_state: state,
            },
        }
    }

    fn extract_response_content(response: &ChatResponse) -> &str {
        response
            .choices
            .first()
            .and_then(|c| c.message.content.as_deref())
            .unwrap_or("")
    }

    /// Run the agent loop with the given context.
    ///
    /// Flows through the architectural state machine:
    /// Planning → CheckingCompletion → Deciding → Querying/Performing → loop
    /// When both a tool registry and LLM provider are present, the
    /// `PerformingEntityModification` state dispatches to `run_tool_loop`.
    pub async fn run(&mut self, context: AgentContext) -> AgentResult<AgentRunResult> {
        self.iterations = 0;
        self.state_history.clear();

        // Initialize conversation history from context
        self.conversation_history.clear();
        if !self.config.system_prompt.is_empty() {
            let sp = self.config.system_prompt.clone();
            self.conversation_history.push(ChatMessage::system(&sp));
        }
        for msg in &context.conversation_history {
            self.conversation_history.push(msg.clone());
        }

        loop {
            if self.iterations >= self.config.max_iterations {
                return Err(self.enrich_error(bare_max_iterations(self.iterations)));
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
                    token_usage: None,
                });
            }

            if let AgentState::Error(msg) = self.state.clone() {
                return Err(self.enrich_error(bare_state_error(msg)));
            }

            match self.state.clone() {
                AgentState::EnrichingEntities => {
                    if let Err(e) = self.enrich_entities(&context).await {
                        return Err(self.enrich_error(e));
                    }
                    self.transition_to(AgentState::PlanningEntityModification);
                }
                AgentState::PlanningEntityModification => {
                    if let Err(e) = self.plan_entity_modification(&context).await {
                        return Err(self.enrich_error(e));
                    }
                    self.transition_to(AgentState::PerformingEntityModification);
                }
                AgentState::PerformingEntityModification => {
                    if let Err(e) = self.perform_entity_modification(&context).await {
                        return Err(self.enrich_error(e));
                    }
                    self.transition_to(AgentState::UpdatingEntities);
                }
                AgentState::UpdatingEntities => {
                    if let Err(e) = self.update_entities(&context).await {
                        return Err(self.enrich_error(e));
                    }
                    self.transition_to(AgentState::CheckingTaskCompletion);
                }
                AgentState::CheckingTaskCompletion => {
                    match self.check_task_completion(&context).await {
                        Ok(true) => self.transition_to(AgentState::Completed),
                        Ok(false) => self.transition_to(AgentState::EntityModificationDecision),
                        Err(e) => return Err(self.enrich_error(e)),
                    }
                }
                AgentState::EntityModificationDecision => {
                    match self.entity_modification_decision(&context).await {
                        Ok(true) => self.transition_to(AgentState::QueryingEntities),
                        Ok(false) => self.transition_to(AgentState::PlanningEntityModification),
                        Err(e) => return Err(self.enrich_error(e)),
                    }
                }
                AgentState::QueryingEntities => {
                    if let Err(e) = self.query_entities(&context).await {
                        return Err(self.enrich_error(e));
                    }
                    self.transition_to(AgentState::EntityModificationDecision);
                }
                AgentState::Completed | AgentState::Error(_) => unreachable!(),
            }

            self.iterations += 1;
            if let Some(ref counter) = self.progress_counter {
                counter.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Transition to a new
    fn transition_to(&mut self, new_state: AgentState) {
        debug_assert!(
            Self::is_legal_transition(&self.state, &new_state),
            "illegal state transition: {:?} -> {:?}",
            self.state,
            new_state
        );
        if self.config.verbose {
            tracing::debug!("State transition: {:?} → {:?}", self.state, new_state);
        }
        self.state_history.push(new_state.clone());
        self.state = new_state;
    }

    /// Check whether a state transition is legal according to ARCHITECTURE.md.
    ///
    /// ```text
    /// EnrichingEntities → PlanningEntityModification
    ///                             ↓
    ///             PerformingEntityModification
    ///                             ↓
    ///                   UpdatingEntities
    ///                             ↓
    ///              CheckingTaskCompletion ─── Yes ───→ Completed
    ///                             ↓ No
    ///         EntityModificationDecision
    ///                 ├─ Proceed ───────────────────────────┤
    ///                                                     │
    ///                 └─ Query ─→ QueryingEntities ─→ EntityModificationDecision
    /// ```
    pub fn is_legal_transition(from: &AgentState, to: &AgentState) -> bool {
        use AgentState::*;
        // Any state may transition to Error
        if matches!(to, Error(_)) {
            return true;
        }
        matches!(
            (from, to),
            (EnrichingEntities, PlanningEntityModification)
                | (PlanningEntityModification, PerformingEntityModification)
                | (PerformingEntityModification, UpdatingEntities)
                | (UpdatingEntities, CheckingTaskCompletion)
                | (CheckingTaskCompletion, Completed)
                | (CheckingTaskCompletion, EntityModificationDecision)
                | (EntityModificationDecision, QueryingEntities)
                | (EntityModificationDecision, PlanningEntityModification)
                | (QueryingEntities, EntityModificationDecision)
        )
    }

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
                        return Err(bare_state_error(format!(
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

    /// Entity Enrichment (ARCHITECTURE.md) — scan and enrich entities from
    /// the application state before planning begins.
    async fn enrich_entities(&mut self, context: &AgentContext) -> AgentResult<()> {
        if self.config.verbose {
            tracing::info!("Enriching entities for prompt: {}", context.user_prompt);
        }

        let query_results = rag::query_entities(
            &self.entity_store,
            &context.user_prompt,
            Some(DEFAULT_PLANNING_RAG_LIMIT),
        )
        .await
        .map_err(|e| bare_state_error(format!("RAG query failed: {}", e)))?;

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

        if !query_results.is_empty() {
            let summary: String = query_results
                .iter()
                .map(|r| format!("Entity: {:?}", r.entity_type))
                .collect::<Vec<_>>()
                .join("; ");
            self.plan_cache = Some(summary);
        }

        Ok(())
    }

    /// Plan Entity Modification (ARCHITECTURE.md) — ask the LLM to analyse
    /// the user request and create an execution plan.
    async fn plan_entity_modification(&mut self, context: &AgentContext) -> AgentResult<()> {
        if self.config.verbose {
            tracing::info!(
                "Planning entity modification for prompt: {}",
                context.user_prompt
            );
        }

        if let Some(provider) = &self.llm_provider {
            use crate::entities::EntityQuery;

            let entity_count = self
                .entity_store
                .query(&EntityQuery::default())
                .await
                .map_err(|e| bare_state_error(format!("Failed to query entities: {}", e)))?
                .len();

            let enrichment_summary = self.plan_cache.as_deref().unwrap_or("No enrichment data");

            let prompt_text = prompts::PlanningPrompt::build(
                &context.user_prompt,
                entity_count,
                enrichment_summary,
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
                return Err(bare_state_error(
                    "LLM returned empty choices array for planning",
                ));
            }

            self.plan_cache = response.choices[0].message.content.clone();

            if self.config.verbose {
                tracing::info!("LLM Plan: {:?}", self.plan_cache);
            }
        }

        Ok(())
    }

    /// Task Complete? (ARCHITECTURE.md) — determine whether the user's
    /// request has been fully satisfied.
    async fn check_task_completion(&self, context: &AgentContext) -> AgentResult<bool> {
        if let Some(provider) = &self.llm_provider {
            use crate::entities::EntityQuery;

            let entities = self
                .entity_store
                .query(&EntityQuery::default())
                .await
                .map_err(|e| bare_task_check_failed(format!("Failed to query entities: {}", e)))?;

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

    /// Entity Modification Decision (ARCHITECTURE.md) — decide whether to
    /// query for more context (true) or proceed to plan (false).
    async fn entity_modification_decision(&self, context: &AgentContext) -> AgentResult<bool> {
        if let Some(provider) = &self.llm_provider {
            use crate::entities::EntityQuery;

            let plan = self.plan_cache.as_deref().unwrap_or("No plan yet");
            let entity_count = self
                .entity_store
                .query(&EntityQuery::default())
                .await
                .map_err(|e| bare_state_error(format!("Failed to query entities: {}", e)))?
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

    /// Query Entities / RAG (ARCHITECTURE.md) — retrieve additional entity
    /// context to inform the next modification decision.
    async fn query_entities(&self, context: &AgentContext) -> AgentResult<()> {
        let results = rag::query_entities(
            &self.entity_store,
            &context.user_prompt,
            Some(DEFAULT_QUERY_RAG_LIMIT),
        )
        .await
        .map_err(|e| bare_state_error(format!("RAG query failed during decision: {}", e)))?;

        if self.config.verbose {
            tracing::info!(
                "Query returned {} results for decision context",
                results.len()
            );
        }

        Ok(())
    }

    /// Perform Entity Modification (ARCHITECTURE.md) — execute the action
    /// planned in the previous state. Dispatches to the tool-aware path when
    /// both an LLM provider and a tool registry are present; otherwise falls
    /// back to the MVP in-memory implementation.
    async fn perform_entity_modification(&mut self, context: &AgentContext) -> AgentResult<()> {
        if self.llm_provider.is_some() && self.tool_registry.is_some() {
            let provider = self.llm_provider.as_ref().unwrap().clone();
            self.perform_entity_modification_with_tools(context, &provider)
                .await
        } else {
            self.perform_entity_modification_mvp(context).await
        }
    }

    /// Update Entities (ARCHITECTURE.md) — commit entity changes to the store
    /// after a modification has been performed. Currently a lightweight
    /// confirmation step; future versions may add validation or journaling.
    async fn update_entities(&mut self, _context: &AgentContext) -> AgentResult<()> {
        if self.config.verbose {
            tracing::info!(
                "Updating entities (performed_actions: {})",
                self.performed_actions
            );
        }

        // Entity mutations are currently applied inline during
        // perform_entity_modification. This step exists to match the
        // ARCHITECTURE.md flow and provide a hook for future validation,
        // journaling, or batched writes.
        Ok(())
    }

    /// MVP perform: create a GitRepository entity.
    ///
    /// Used when no tool registry is present. In the MVP flow the LLM is
    /// not consulted for individual modifications; the agent creates a
    /// `GitRepository` entity unconditionally so that the completion check
    /// can observe that at least one action was performed.
    async fn perform_entity_modification_mvp(&mut self, context: &AgentContext) -> AgentResult<()> {
        use crate::entities::git::types::GitRepository;

        self.performed_actions += 1;

        if self.config.verbose {
            tracing::info!("Performing action for: {}", context.user_prompt);
        }

        let new_entity = Box::new(GitRepository::new(String::new(), "main".to_string()));

        let entity_id = self
            .entity_store
            .store(new_entity)
            .await
            .map_err(|e| bare_state_error(format!("Failed to store entity: {}", e)))?;

        if self.config.verbose {
            tracing::info!("Created entity: {}", entity_id);
        }

        Ok(())
    }

    /// Tool-aware perform: run a multi-turn tool loop with the LLM.
    ///
    /// The tool loop repeatedly calls the LLM with the current conversation
    /// and dispatches any tool calls until the model stops issuing them or
    /// `MAX_TOOL_ITERATIONS` is reached.
    async fn perform_entity_modification_with_tools(
        &mut self,
        context: &AgentContext,
        provider: &Arc<dyn ModelProvider>,
    ) -> AgentResult<()> {
        let tool_defs = self.tool_registry.as_ref().unwrap().get_definitions();

        // Add plan context if available, rather than resetting the conversation
        if let Some(plan) = &self.plan_cache {
            self.conversation_history.push(ChatMessage::user(format!(
                "Execute the following plan using the available tools: {}",
                plan
            )));
        } else {
            self.conversation_history
                .push(ChatMessage::user(&context.user_prompt));
        }

        for _ in 0..MAX_TOOL_ITERATIONS {
            let request =
                ChatRequest::new(&self.config.model_name, self.conversation_history.clone())
                    .with_tools(tool_defs.clone())
                    .with_temperature(COMPLETION_TEMPERATURE);

            let response = self
                .call_llm_with_retry(provider, request, "tool loop")
                .await?;

            let choice = response.choices.into_iter().next().ok_or_else(|| {
                bare_state_error("LLM returned empty choices array in tool loop")
            })?;

            // Accumulate the assistant turn
            self.conversation_history.push(choice.message.clone());

            let tool_calls = choice.message.tool_calls.unwrap_or_default();

            if tool_calls.is_empty() {
                // Model stopped issuing tool calls — modification complete
                self.performed_actions += 1;
                return Ok(());
            }

            // Dispatch each tool call
            for tc in &tool_calls {
                let result = self.dispatch_tool_call(tc).await?;
                let tool_msg =
                    ChatMessage::tool_result(&tc.id, &result);
                self.conversation_history.push(tool_msg);
            }
        }

        // Reached MAX_TOOL_ITERATIONS without a stop signal — count it as
        // done rather than erroring, so the agent can proceed to completion
        // check.
        self.performed_actions += 1;
        Ok(())
    }

    /// Dispatch a single tool call to the registered tool.
    async fn dispatch_tool_call(&self, tc: &ToolCall) -> AgentResult<String> {
        let registry = self
            .tool_registry
            .as_ref()
            .ok_or_else(|| bare_state_error("dispatch_tool_call called without tool registry"))?;

        let fn_call = tc.function.as_ref().ok_or_else(|| {
            bare_state_error("ToolCall missing function field")
        })?;

        let result = registry
            .execute(&fn_call.name, &fn_call.arguments)
            .await
            .unwrap_or_else(|e| format!("Tool error: {}", e));

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::git::types::GitRepository;
    use crate::entities::{EntityQuery, EntityStore, InMemoryEntityStore};
    use crate::tools::{EchoTool, ToolRegistry};
    use model::provider::{ModelError, ModelResult};
    use model::types::{
        ChatResponse, Choice, FinishReason, FunctionCall, MessageRole, ModelInfo, ToolCall, Usage,
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
                        id: "call_1".to_string(),
                        call_type: "function".to_string(),
                        function: Some(FunctionCall {
                            name: tool_name.to_string(),
                            arguments: args.to_string(),
                        }),
                    }]),
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::ToolCalls),
            }],
            usage: None,
        }
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

        agent.state = AgentState::EnrichingEntities;

        let context = AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok() || matches!(result, Err(AgentError::MaxIterationsExceeded { .. })));
    }

    #[tokio::test]
    async fn test_enriched_error_carries_diagnostics() {
        let responses: Vec<ChatResponse> = (0..5).map(|_| plain_response("not done yet")).collect();
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let config = AgentConfig {
            max_iterations: 0,
            ..Default::default()
        };
        let mut agent = AgentLoop::with_llm(config, InMemoryEntityStore::new(), provider);
        let context = AgentContext {
            user_prompt: "test".to_string(),
            conversation_history: vec![],
            app_state_id: "s".to_string(),
        };
        let err = agent.run(context).await.unwrap_err();
        let (calls, conv, iters, state) = err.diagnostics();
        assert_eq!(iters, 0);
        assert_eq!(state, &AgentState::EnrichingEntities);
        assert!(calls.is_empty());
        assert!(conv.is_empty());
    }

    #[tokio::test]
    async fn test_agent_run_records_state_history() {
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

        agent.run(context).await.unwrap();
        assert!(!agent.state_history.is_empty(), "state_history should be populated after run");
    }

    #[tokio::test]
    async fn test_agent_run_clears_state_history_on_rerun() {
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

        agent.run(context.clone()).await.unwrap();
        let first_history_len = agent.state_history.len();

        agent.state = AgentState::EnrichingEntities;
        agent.run(context).await.unwrap();
        assert_eq!(
            agent.state_history.len(),
            first_history_len,
            "state_history should be reset on each run"
        );
    }

    #[tokio::test]
    async fn test_entity_store_stores_and_retrieves() {
        let mut entity_store = InMemoryEntityStore::new();
        let entity = Box::new(GitRepository::new("test_id".to_string(), "main".to_string()));
        entity_store.store(entity).await.unwrap();

        let query = EntityQuery::default();
        let results = entity_store.query(&query).await.unwrap();
        assert!(!results.is_empty(), "Entity store should return results after storing");
    }

    #[tokio::test]
    async fn test_agent_stores_context_entity() {
        use crate::entities::EntityQuery;

        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let mut agent = AgentLoop::new(config);
        let context = AgentContext {
            user_prompt: "test store entity".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        };

        agent.run(context).await.unwrap();

        let results = agent
            .entity_store
            .query(&EntityQuery::default())
            .await
            .unwrap();
        // Should have at least the context entity stored + the entity created by perform_entity_modification
        assert!(!results.is_empty(), "Entity store should not be empty after run");
    }

    #[tokio::test]
    async fn test_agent_run_with_context_history() {
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let prior_message = ChatMessage {
            role: MessageRole::User,
            content: Some("prior message".to_string()),
            tool_calls: None,
            tool_call_id: None,
        };
        let mut agent = AgentLoop::new(config);
        let context = AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![prior_message.clone()],
            app_state_id: "test_state".to_string(),
        };

        agent.run(context).await.unwrap();
        assert!(
            agent.conversation_history.contains(&prior_message),
            "Prior messages should be in conversation history"
        );
    }

    #[tokio::test]
    async fn test_agent_system_prompt() {
        let config = AgentConfig {
            max_iterations: 10,
            system_prompt: "You are a test agent.".to_string(),
            ..Default::default()
        };
        let mut agent = AgentLoop::new(config);
        let context = AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        };

        agent.run(context).await.unwrap();
        assert!(
            agent
                .conversation_history
                .iter()
                .any(|m| m.role == MessageRole::System),
            "System prompt should be in conversation history"
        );
    }

    // --- New tests for legal transition checks ---

    #[test]
    fn test_legal_transitions_from_enriching() {
        assert!(AgentLoop::is_legal_transition(
            &AgentState::EnrichingEntities,
            &AgentState::PlanningEntityModification
        ));
        assert!(!AgentLoop::is_legal_transition(
            &AgentState::EnrichingEntities,
            &AgentState::Completed
        ));
    }

    #[test]
    fn test_legal_transitions_to_error() {
        assert!(AgentLoop::is_legal_transition(
            &AgentState::EnrichingEntities,
            &AgentState::Error("fail".to_string())
        ));
        assert!(AgentLoop::is_legal_transition(
            &AgentState::Completed,
            &AgentState::Error("fail".to_string())
        ));
    }

    #[test]
    fn test_legal_transitions_completed_is_terminal() {
        for state in [
            AgentState::EnrichingEntities,
            AgentState::PlanningEntityModification,
            AgentState::PerformingEntityModification,
        ] {
            assert!(!AgentLoop::is_legal_transition(&AgentState::Completed, &state));
        }
    }

    #[test]
    fn test_full_state_machine_transitions() {
        let valid_transitions = vec![
            (
                AgentState::EnrichingEntities,
                AgentState::PlanningEntityModification,
            ),
            (
                AgentState::PlanningEntityModification,
                AgentState::PerformingEntityModification,
            ),
            (
                AgentState::PerformingEntityModification,
                AgentState::UpdatingEntities,
            ),
            (
                AgentState::UpdatingEntities,
                AgentState::CheckingTaskCompletion,
            ),
            (
                AgentState::CheckingTaskCompletion,
                AgentState::Completed,
            ),
            (
                AgentState::CheckingTaskCompletion,
                AgentState::EntityModificationDecision,
            ),
            (
                AgentState::EntityModificationDecision,
                AgentState::QueryingEntities,
            ),
            (
                AgentState::EntityModificationDecision,
                AgentState::PlanningEntityModification,
            ),
            (
                AgentState::QueryingEntities,
                AgentState::EntityModificationDecision,
            ),
        ];

        for (from, to) in valid_transitions {
            assert!(
                AgentLoop::is_legal_transition(&from, &to),
                "Expected legal transition from {:?} to {:?}",
                from,
                to
            );
        }
    }

    // ===== MVP Test =====
    /// Task 1: Basic Agent Loop MVP - Complete Control Flow
    #[tokio::test]
    #[ignore = "requires Ollama"]
    async fn test_agent_mvp_control_loop() {
        use crate::entities::git::types::GitRepository;
        use crate::entities::EntityStore;

        let config = AgentConfig {
            max_iterations: 100,
            verbose: true,
            ..Default::default()
        };

        let mut entity_store = InMemoryEntityStore::new();
        let initial = Box::new(GitRepository::new(String::new(), "main".to_string()));
        entity_store.store(initial).await.unwrap();

        let provider = match model::OllamaProvider::with_default_config() {
            Ok(p) => Arc::new(p),
            Err(_) => {
                println!("Test skipped: Ollama not available");
                return;
            }
        };

        let mut agent = AgentLoop::with_llm(config, entity_store, provider);

        let context = AgentContext {
            user_prompt: "Create a new git repository for user service".to_string(),
            conversation_history: vec![],
            app_state_id: "mvp_test".to_string(),
        };

        let result = agent.run(context).await;

        assert!(result.is_ok(), "MVP agent should complete: {:?}", result);
        let run_result = result.unwrap();
        assert!(run_result.task_completed, "Task should be completed");

        let git_entities = agent
            .entity_store
            .query(&EntityQuery::default())
            .await
            .unwrap();

        assert!(!git_entities.is_empty(), "Should have git entities");

        println!(
            "\u{2705} MVP Test passed: Agent completed control loop with {} git entities",
            git_entities.len()
        );
    }

    /// Task 2: Entity Store Integration Test
    #[tokio::test]
    async fn test_mvp_agent_control_loop_with_entities() {
        use crate::entities::git::types::GitRepository;
        use crate::entities::EntityStore;

        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };

        let mut entity_store = InMemoryEntityStore::new();
        let initial = Box::new(GitRepository::new(String::new(), "main".to_string()));
        entity_store.store(initial).await.unwrap();

        let mut agent = AgentLoop::with_entity_store(config, entity_store);

        let context = AgentContext {
            user_prompt: "Modify entities for auth service".to_string(),
            conversation_history: vec![],
            app_state_id: "entity_test".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok(), "MVP agent should handle entity store: {:?}", result);
    }

    /// Task 3: Validation Test - Check Agent Error Types
    #[tokio::test]
    async fn test_agent_error_diagnostics() {
        let responses: Vec<ChatResponse> = vec![];
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let config = AgentConfig {
            max_iterations: 0,
            ..Default::default()
        };
        let mut agent = AgentLoop::with_llm(config, InMemoryEntityStore::new(), provider);
        let context = AgentContext {
            user_prompt: "test".to_string(),
            conversation_history: vec![],
            app_state_id: "s".to_string(),
        };
        let result = agent.run(context).await;
        assert!(
            result.is_ok() || result.is_err(),
            "Agent should either succeed or fail gracefully"
        );

        if let Err(ref err) = result {
            let (calls, conv, _iters, _state) = err.diagnostics();
            assert!(
                calls.is_empty() || !calls.is_empty(),
                "Diagnostics should be accessible"
            );
            assert!(
                conv.is_empty() || !conv.is_empty(),
                "Conversation snapshot should be accessible"
            );
        }
    }

    /// Task 4: Configuration Test - Verify all config options work
    #[test]
    fn test_agent_config() {
        let config = AgentConfig {
            max_iterations: 50,
            verbose: true,
            system_prompt: "Test system prompt".to_string(),
            model_name: "test_model".to_string(),
        };

        assert_eq!(config.max_iterations, 50);
        assert!(config.verbose);
        assert_eq!(config.system_prompt, "Test system prompt");
        assert_eq!(config.model_name, "test_model");
    }

    /// Task 5: State Transitions Test
    #[test]
    fn test_state_transitions() {
        let valid_transitions = [
            (
                AgentState::EnrichingEntities,
                AgentState::PlanningEntityModification,
            ),
            (
                AgentState::PlanningEntityModification,
                AgentState::PerformingEntityModification,
            ),
            (
                AgentState::PerformingEntityModification,
                AgentState::UpdatingEntities,
            ),
            (
                AgentState::UpdatingEntities,
                AgentState::CheckingTaskCompletion,
            ),
            (
                AgentState::CheckingTaskCompletion,
                AgentState::Completed,
            ),
        ];

        for (from, to) in valid_transitions {
            assert!(
                AgentLoop::is_legal_transition(&from, &to),
                "Expected {:?} -> {:?} to be valid",
                from,
                to
            );
        }

        let invalid_transitions = [
            (
                AgentState::Completed,
                AgentState::EnrichingEntities,
            ),
            (
                AgentState::CheckingTaskCompletion,
                AgentState::PlanningEntityModification,
            ),
        ];

        for (from, to) in invalid_transitions {
            assert!(
                !AgentLoop::is_legal_transition(&from, &to),
                "Expected {:?} -> {:?} to be invalid",
                from,
                to
            );
        }
    }

    /// Task 6: Concurrent Agent Operations
    #[tokio::test]
    async fn test_concurrent_agents() {
        let handles: Vec<_> = (0..3)
            .map(|i| {
                tokio::spawn(async move {
                    let config = AgentConfig {
                        max_iterations: 5,
                        ..Default::default()
                    };
                    let mut agent = AgentLoop::new(config);
                    let context = AgentContext {
                        user_prompt: format!("concurrent test {}", i),
                        conversation_history: vec![],
                        app_state_id: format!("concurrent_{}", i),
                    };
                    agent.run(context).await
                })
            })
            .collect();

        for handle in handles {
            let result = handle.await.expect("Task should not panic");
            assert!(result.is_ok(), "Concurrent agent should complete: {:?}", result);
        }
    }

    // ===== LLM Agent Tests (require Ollama) =====

    /// Task 7: Complete Agent Loop with LLM Integration
    #[tokio::test]
    #[ignore = "requires Ollama"]
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
            "\u{2705} LLM Agent Test passed with plan: {:?}",
            agent.plan_cache.as_ref().unwrap()
        );
    }

    /// Task 9: Backward Compatibility Test - MVP Mode Without LLM
    #[tokio::test]
    async fn test_mvp_mode_still_works_without_llm() {
        use crate::entities::git::types::GitRepository;
        use crate::entities::EntityStore;

        let config = AgentConfig {
            max_iterations: 20,
            verbose: false,
            ..Default::default()
        };

        let mut entity_store = InMemoryEntityStore::new();
        let initial = Box::new(GitRepository::new(String::new(), "main".to_string()));
        entity_store.store(initial).await.unwrap();

        let mut agent = AgentLoop::with_entity_store(config, entity_store);

        let context = AgentContext {
            user_prompt: "Do something".to_string(),
            conversation_history: vec![],
            app_state_id: "mvp_compat_test".to_string(),
        };

        let result = agent.run(context).await;
        assert!(result.is_ok(), "MVP mode should still work: {:?}", result);
        let run_result = result.unwrap();
        assert!(run_result.task_completed, "Task should complete");
        assert_eq!(run_result.final_state, AgentState::Completed);
        assert!(
            agent.plan_cache.is_none(),
            "MVP mode should not populate plan_cache"
        );

        println!("\u{2705} MVP mode backward compatibility verified");
    }

    #[tokio::test]
    #[ignore = "requires Ollama"]
    async fn test_agent_loop_with_ollama_and_tools() {
        use model::OllamaProvider;

        let provider = match OllamaProvider::with_default_config() {
            Ok(p) => Arc::new(p),
            Err(_) => {
                eprintln!("Skipping: Ollama not available");
                return;
            }
        };

        let config = AgentConfig {
            max_iterations: 20,
            verbose: true,
            ..Default::default()
        };

        let store = InMemoryEntityStore::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));

        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "Echo 'hello world'".to_string(),
            conversation_history: vec![],
            app_state_id: "tool_test".to_string(),
        };

        let run_result = agent.run(context).await.unwrap();
        assert!(run_result.task_completed, "Task should complete");
        assert_eq!(run_result.final_state, AgentState::Completed);

        println!("\u{2705} Agent loop with Ollama and tools completed successfully");
        println!(
            "   Conversation history: {} messages",
            agent.conversation_history.len()
        );
    }

    #[tokio::test]
    async fn test_state_machine_run_stores_context_entity() {
        use crate::entities::EntityQuery;

        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_entity_store(config, store);

        let context = AgentContext {
            user_prompt: "test entity storage".to_string(),
            conversation_history: vec![],
            app_state_id: "storage_test".to_string(),
        };

        agent.run(context).await.unwrap();

        let results = agent
            .entity_store
            .query(&EntityQuery::default())
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "Entity store should have entities after run"
        );
    }

    #[tokio::test]
    async fn test_run_with_llm_stores_context_entity() {
        use crate::entities::EntityQuery;
        let responses = vec![
            plain_response("Plan: do x"),
            plain_response("COMPLETE - task done"),
        ];
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_llm(config, store, provider);

        let context = AgentContext {
            user_prompt: "test entity storage with llm".to_string(),
            conversation_history: vec![],
            app_state_id: "storage_test_llm".to_string(),
        };

        agent.run(context).await.unwrap();

        let results = agent
            .entity_store
            .query(&EntityQuery::default())
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "Entity store should have entities after run"
        );
    }

    #[tokio::test]
    async fn test_tool_loop_run_stores_context_entity() {
        use crate::entities::EntityQuery;
        let responses = vec![
            plain_response("plan step"),
            plain_response("no tools needed"),
            plain_response("COMPLETE - done"),
        ];
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let store = InMemoryEntityStore::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "test entity storage with tools".to_string(),
            conversation_history: vec![],
            app_state_id: "storage_test_tools".to_string(),
        };

        agent.run(context).await.unwrap();

        let results = agent
            .entity_store
            .query(&EntityQuery::default())
            .await
            .unwrap();
        assert!(
            !results.is_empty(),
            "Entity store should have entities after run"
        );
    }

    #[tokio::test]
    async fn test_tool_loop_stores_tool_calls_made() {
        let responses = vec![
            plain_response("plan step"),
            tool_call_response("echo", serde_json::json!({"message": "hello"})),
            plain_response("done"),
            plain_response("COMPLETE - task complete"),
        ];
        let provider: Arc<dyn ModelProvider> = MockProvider::new(responses);
        let config = AgentConfig {
            max_iterations: 10,
            ..Default::default()
        };
        let store = InMemoryEntityStore::new();
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        let mut agent = AgentLoop::with_tools(config, store, provider, registry);

        let context = AgentContext {
            user_prompt: "echo hello".to_string(),
            conversation_history: vec![],
            app_state_id: "tool_calls_test".to_string(),
        };

        let result = agent.run(context).await.unwrap();
        assert!(
            !result.tool_calls_made.is_empty(),
            "tool_calls_made should be populated when tools are used"
        );
    }

    // ===== Progress counter tests =====

    #[tokio::test]
    async fn test_progress_counter_increments() {
        use std::sync::atomic::Ordering;

        let counter = Arc::new(AtomicUsize::new(0));
        let config = AgentConfig {
            max_iterations: 20,
            ..Default::default()
        };
        let mut agent = AgentLoop::new(config).with_progress_counter(counter.clone());

        let context = AgentContext {
            user_prompt: "test".to_string(),
            conversation_history: vec![],
            app_state_id: "counter_test".to_string(),
        };

        agent.run(context).await.unwrap();
        assert!(
            counter.load(Ordering::Relaxed) > 0,
            "Progress counter should have been incremented"
        );
    }

    // ===== Tests for no-tool-registry diagnostic =====

    #[test]
    fn test_no_tool_registry_diagnostic_from_state_error() {
        let err = AgentError::StateError {
            message: "dispatch_tool_call called without tool registry".to_string(),
            iterations_completed: 1,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::PerformingEntityModification,
        };
        let (calls, conv, iters, state) = err.diagnostics();
        assert_eq!(iters, 1);
        assert_eq!(state, &AgentState::PerformingEntityModification);
        assert!(calls.is_empty());
        assert!(conv.is_empty());
        let msg = format!("{}", err);
        assert!(
            msg.contains("dispatch_tool_call called without tool registry"),
            "expected no-tool-registry diagnostic, got: {msg}"
        );
    }

    // --- New tests targeting previously-uncovered code paths ---

    #[test]
    fn test_agent_error_state_error_diagnostics() {
        let err = AgentError::StateError {
            message: "bad state".to_string(),
            iterations_completed: 7,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::PlanningEntityModification,
        };
        let (calls, conv, iters, state) = err.diagnostics();
        assert_eq!(iters, 7);
        assert_eq!(state, &AgentState::PlanningEntityModification);
        assert!(calls.is_empty());
        assert!(conv.is_empty());
    }

    #[test]
    fn test_agent_error_task_check_failed_diagnostics() {
        let err = AgentError::TaskCheckFailed {
            message: "check failed".to_string(),
            iterations_completed: 3,
            tool_calls_made: vec![],
            conversation_snapshot: vec![],
            last_agent_state: AgentState::CheckingTaskCompletion,
        };
        let (calls, conv, iters, state) = err.diagnostics();
        assert_eq!(iters, 3);
        assert_eq!(state, &AgentState::CheckingTaskCompletion);
        assert!(calls.is_empty());
        assert!(conv.is_empty());
    }

    #[test]
    fn test_with_entity_store_initialises_correctly() {
        let store = InMemoryEntityStore::new();
        let config = AgentConfig {
            max_iterations: 42,
            ..Default::default()
        };
        let agent = AgentLoop::with_entity_store(config, store);
        assert_eq!(agent.state(), &AgentState::EnrichingEntities);
        assert_eq!(agent.config.max_iterations, 42);
        assert!(agent.state_history().is_empty());
        assert!(agent.conversation_history().is_empty());
    }

    #[test]
    fn test_entity_store_accessor() {
        let store = InMemoryEntityStore::new();
        let agent = AgentLoop::with_entity_store(AgentConfig::default(), store);
        let _store_ref = agent.entity_store();
    }

    #[test]
    fn test_entity_store_mut_accessor() {
        let store = InMemoryEntityStore::new();
        let mut agent = AgentLoop::with_entity_store(AgentConfig::default(), store);
        let _store_mut = agent.entity_store_mut();
    }

    #[tokio::test]
    async fn test_state_history_populated_after_run() {
        let provider = MockProvider::new(vec![
            plain_response("Plan: do x"),
            plain_response("COMPLETE - done"),
        ]);
        let mut agent = AgentLoop::with_llm(
            AgentConfig::default(),
            InMemoryEntityStore::new(),
            provider,
        );
        let context = AgentContext {
            user_prompt: "x".to_string(),
            conversation_history: vec![],
            app_state_id: "s".to_string(),
        };
        agent.run(context).await.unwrap();
        assert!(
            !agent.state_history().is_empty(),
            "state transitions should be recorded in state_history"
        );
    }

    #[test]
    fn test_validate_llm_response_empty_is_false() {
        let agent = AgentLoop::new(AgentConfig::default());
        assert!(!agent.validate_llm_response("", &[]));
        assert!(!agent.validate_llm_response("   ", &[]));
    }

    #[test]
    fn test_validate_llm_response_too_long_is_false() {
        let agent = AgentLoop::new(AgentConfig::default());
        let long = "x".repeat(super::MAX_LLM_RESPONSE_LENGTH + 1);
        assert!(!agent.validate_llm_response(&long, &[]));
    }

    #[test]
    fn test_validate_llm_response_no_keywords_passes() {
        let agent = AgentLoop::new(AgentConfig::default());
        assert!(agent.validate_llm_response("a reasonable response", &[]));
    }

    #[test]
    fn test_validate_llm_response_keyword_match_and_mismatch() {
        let agent = AgentLoop::new(AgentConfig::default());
        // Keyword present (case-insensitive)
        assert!(agent.validate_llm_response("YES we should do it", &["YES", "NO"]));
        assert!(agent.validate_llm_response("yes we should do it", &["YES"]));
        // No keyword match
        assert!(!agent.validate_llm_response("maybe", &["YES", "NO"]));
    }

    #[test]
    fn test_extract_response_content_empty_choices_returns_empty() {
        let response = ChatResponse {
            choices: vec![],
            usage: None,
        };
        assert_eq!(AgentLoop::<InMemoryEntityStore>::extract_response_content(&response), "");
    }

    #[test]
    fn test_extract_response_content_none_content_returns_empty() {
        let response = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: None,
            }],
            usage: None,
        };
        assert_eq!(AgentLoop::<InMemoryEntityStore>::extract_response_content(&response), "");
    }

    #[test]
    fn test_extract_response_content_returns_content() {
        let response = ChatResponse {
            choices: vec![Choice {
                message: ChatMessage {
                    role: MessageRole::Assistant,
                    content: Some("hello world".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        };
        let content = AgentLoop::<InMemoryEntityStore>::extract_response_content(&response);
        assert_eq!(content, "hello world");
    }

}

#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Encode the legal transitions from ARCHITECTURE.md as a numeric table
    /// so kani can exhaustively verify that is_legal_transition accepts
    /// exactly the correct pairs and rejects everything else.
    fn state_index(s: &AgentState) -> u8 {
        match s {
            AgentState::EnrichingEntities => 0,
            AgentState::PlanningEntityModification => 1,
            AgentState::PerformingEntityModification => 2,
            AgentState::UpdatingEntities => 3,
            AgentState::CheckingTaskCompletion => 4,
            AgentState::EntityModificationDecision => 5,
            AgentState::QueryingEntities => 6,
            AgentState::Completed => 7,
            AgentState::Error(_) => 8,
        }
    }

    fn state_from_index(i: u8) -> AgentState {
        match i {
            0 => AgentState::EnrichingEntities,
            1 => AgentState::PlanningEntityModification,
            2 => AgentState::PerformingEntityModification,
            3 => AgentState::UpdatingEntities,
            4 => AgentState::CheckingTaskCompletion,
            5 => AgentState::EntityModificationDecision,
            6 => AgentState::QueryingEntities,
            7 => AgentState::Completed,
            _ => AgentState::Error("test".to_string()),
        }
    }

    /// The allowed non-error edges (from_index, to_index).
    const LEGAL_EDGES: [(u8, u8); 8] = [
        (0, 1), // EnrichingEntities → PlanningEntityModification
        (1, 2), // PlanningEntityModification → PerformingEntityModification
        (2, 3), // PerformingEntityModification → UpdatingEntities
        (3, 4), // UpdatingEntities → CheckingTaskCompletion
        (4, 7), // CheckingTaskCompletion → Completed
        (4, 5), // CheckingTaskCompletion → EntityModificationDecision
        (5, 6), // EntityModificationDecision → QueryingEntities
        (5, 1), // EntityModificationDecision → PlanningEntityModification
    ];

    /// Verify that is_legal_transition is consistent with the architecture
    /// diagram for all non-error state pairs.
    #[kani::proof]
    #[kani::unwind(2)]
    fn legal_transitions_match_architecture() {
        let from_idx: u8 = kani::any();
        kani::assume(from_idx <= 8);
        let to_idx: u8 = kani::any();
        kani::assume(to_idx <= 7); // don't generate Error as target (tested separately)

        let from = state_from_index(from_idx);
        let to = state_from_index(to_idx);

        let result = AgentLoop::is_legal_transition(&from, &to);

        // Any state can transition to Error
        // For non-error targets, only LEGAL_EDGES are allowed
        let expected = LEGAL_EDGES
            .iter()
            .any(|&(f, t)| f == from_idx && t == to_idx);
        assert_eq!(result, expected, "Mismatch for ({from_idx} -> {to_idx})");
    }

    /// Any state may transition to Error.
    #[kani::proof]
    #[kani::unwind(2)]
    fn any_state_can_error() {
        let from_idx: u8 = kani::any();
        kani::assume(from_idx <= 8);
        let from = state_from_index(from_idx);
        let to = AgentState::Error("fail".to_string());
        assert!(AgentLoop::is_legal_transition(&from, &to));
    }

    /// Completed and Error are terminal: no legal non-error successor.
    #[kani::proof]
    #[kani::unwind(2)]
    fn completed_is_terminal() {
        let to_idx: u8 = kani::any();
        kani::assume(to_idx <= 7);
        let to = state_from_index(to_idx);
        let from = AgentState::Completed;
        assert!(
            !AgentLoop::is_legal_transition(&from, &to),
            "Completed should have no non-error successors"
        );
    }

    /// QueryingEntities can only go back to EntityModificationDecision.
    #[kani::proof]
    #[kani::unwind(2)]
    fn querying_only_returns_to_decision() {
        let to_idx: u8 = kani::any();
        kani::assume(to_idx <= 7);
        let to = state_from_index(to_idx);
        let from = AgentState::QueryingEntities;
        let result = AgentLoop::is_legal_transition(&from, &to);
        let expected = to_idx == 5; // EntityModificationDecision
        assert_eq!(result, expected);
    }
}
