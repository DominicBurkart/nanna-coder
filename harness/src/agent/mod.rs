//! Agent architecture implementation
//!
//! This module implements the main agent control loop following the architecture:
//! 1. Application State → Entity Enrichment
//! 2. User Prompt → Plan Entity Modification
//! 3. Task Complete? decision
//! 4. If No → Entity Modification Decision → Query Entities (RAG) or Plan
//! 5. Plan → Perform Entity Modification → Update Entities → back to check
//! 6. If Yes → Application State 2 (completed)

pub mod decision;
pub mod enrichment;
pub mod entity;
pub mod modification;
pub mod rag;

use async_trait::async_trait;
use decision::{DecisionContext, ModificationDecision};
use enrichment::{EnrichmentConfig, EnrichmentResult};
use entity::EntityGraph;
use modification::ExecutionResult;
use rag::{QueryContext, QueryResult};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur in the agent
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("Entity error: {0}")]
    Entity(#[from] entity::EntityError),
    #[error("Agent state error: {0}")]
    StateError(String),
    #[error("Task completion check failed: {0}")]
    TaskCheckFailed(String),
    #[error("Maximum iterations exceeded")]
    MaxIterationsExceeded,
}

pub type AgentResult<T> = Result<T, AgentError>;

/// State of the agent in the control loop
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    /// Initial state - enriching entities
    Enriching,
    /// Planning entity modifications
    Planning,
    /// Querying entities using RAG
    QueryingEntities,
    /// Deciding what modification to make
    DecidingModification,
    /// Performing entity modification
    Modifying,
    /// Updating entities after modification
    UpdatingEntities,
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
    /// Configuration for entity enrichment
    pub enrichment: EnrichmentConfig,
    /// Maximum number of iterations before stopping
    pub max_iterations: usize,
    /// Enable verbose logging
    pub verbose: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enrichment: EnrichmentConfig::default(),
            max_iterations: 100,
            verbose: false,
        }
    }
}

/// Context for the agent's execution
#[derive(Debug, Clone)]
pub struct AgentContext {
    /// User's prompt/request
    pub user_prompt: String,
    /// Conversation history
    pub conversation_history: Vec<String>,
    /// Application state identifier
    pub app_state_id: String,
}

/// Result of running the agent
#[derive(Debug, Clone)]
pub struct AgentRunResult {
    /// Final state of the agent
    pub final_state: AgentState,
    /// Number of iterations executed
    pub iterations: usize,
    /// Entities that were modified
    pub modified_entities: Vec<String>,
    /// Whether the task was completed successfully
    pub task_completed: bool,
}

/// Main agent loop implementation
pub struct AgentLoop {
    /// Current state
    state: AgentState,
    /// Entity graph
    entities: EntityGraph,
    /// Configuration
    config: AgentConfig,
    /// Iteration counter
    iterations: usize,
}

impl AgentLoop {
    /// Create a new agent loop
    pub fn new(config: AgentConfig) -> Self {
        Self {
            state: AgentState::Enriching,
            entities: EntityGraph::new(),
            config,
            iterations: 0,
        }
    }

    /// Get the current state
    pub fn state(&self) -> &AgentState {
        &self.state
    }

    /// Get the entity graph
    pub fn entities(&self) -> &EntityGraph {
        &self.entities
    }

    /// Get a mutable reference to the entity graph
    pub fn entities_mut(&mut self) -> &mut EntityGraph {
        &mut self.entities
    }

    /// Run the agent loop with the given context
    pub async fn run(&mut self, context: AgentContext) -> AgentResult<AgentRunResult> {
        self.iterations = 0;

        loop {
            if self.iterations >= self.config.max_iterations {
                return Err(AgentError::MaxIterationsExceeded);
            }

            if self.config.verbose {
                tracing::info!("Agent iteration {}: {:?}", self.iterations, self.state);
            }

            match &self.state {
                AgentState::Enriching => {
                    self.enrich_entities().await?;
                    self.transition_to(AgentState::Planning);
                }
                AgentState::Planning => {
                    self.plan_modification(&context).await?;
                    self.transition_to(AgentState::CheckingCompletion);
                }
                AgentState::CheckingCompletion => {
                    if self.check_task_complete(&context).await? {
                        self.transition_to(AgentState::Completed);
                    } else {
                        self.transition_to(AgentState::DecidingModification);
                    }
                }
                AgentState::DecidingModification => {
                    let decision = self.decide_modification(&context).await?;
                    if matches!(decision, ModificationDecision::None) {
                        // Need more information, query entities
                        self.transition_to(AgentState::QueryingEntities);
                    } else {
                        self.transition_to(AgentState::Modifying);
                    }
                }
                AgentState::QueryingEntities => {
                    self.query_entities(&context).await?;
                    self.transition_to(AgentState::Planning);
                }
                AgentState::Modifying => {
                    self.perform_modification(&context).await?;
                    self.transition_to(AgentState::UpdatingEntities);
                }
                AgentState::UpdatingEntities => {
                    self.update_entities().await?;
                    self.transition_to(AgentState::CheckingCompletion);
                }
                AgentState::Completed => {
                    return Ok(AgentRunResult {
                        final_state: self.state.clone(),
                        iterations: self.iterations,
                        modified_entities: vec![], // TODO: Track modifications
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

    /// Enrich entities (stub - calls unimplemented enrichment logic)
    async fn enrich_entities(&mut self) -> AgentResult<EnrichmentResult> {
        // This will panic when called due to unimplemented!() in enrichment module
        enrichment::enrich_entities(&mut self.entities, &self.config.enrichment)
            .map_err(AgentError::Entity)
    }

    /// Plan modification (stub)
    async fn plan_modification(&mut self, context: &AgentContext) -> AgentResult<()> {
        let _decision_context = DecisionContext {
            user_prompt: context.user_prompt.clone(),
            conversation_history: context.conversation_history.clone(),
            recent_modifications: vec![],
        };
        // Planning logic would go here
        Ok(())
    }

    /// Check if the task is complete
    async fn check_task_complete(&self, _context: &AgentContext) -> AgentResult<bool> {
        // For now, complete after first iteration to avoid infinite loop
        // Real implementation would check if user's requirements are met
        Ok(self.iterations > 0)
    }

    /// Decide what modification to perform (stub - calls unimplemented decision logic)
    async fn decide_modification(
        &self,
        context: &AgentContext,
    ) -> AgentResult<ModificationDecision> {
        let decision_context = DecisionContext {
            user_prompt: context.user_prompt.clone(),
            conversation_history: context.conversation_history.clone(),
            recent_modifications: vec![],
        };
        // This will panic when called due to unimplemented!() in decision module
        decision::decide_modification(&self.entities, &decision_context).map_err(AgentError::Entity)
    }

    /// Query entities using RAG (stub - calls unimplemented RAG logic)
    async fn query_entities(&self, context: &AgentContext) -> AgentResult<QueryResult> {
        let query_context = QueryContext {
            query: context.user_prompt.clone(),
            context: context.conversation_history.clone(),
            config: rag::RagConfig::default(),
        };
        // This will panic when called due to unimplemented!() in rag module
        rag::query_entities(&self.entities, &query_context).map_err(AgentError::Entity)
    }

    /// Perform modification (stub - calls unimplemented modification logic)
    async fn perform_modification(
        &mut self,
        context: &AgentContext,
    ) -> AgentResult<ExecutionResult> {
        let decision_context = DecisionContext {
            user_prompt: context.user_prompt.clone(),
            conversation_history: context.conversation_history.clone(),
            recent_modifications: vec![],
        };
        let decision = decision::decide_modification(&self.entities, &decision_context)
            .map_err(AgentError::Entity)?;
        let plan = modification::plan_modification(&self.entities, &decision)
            .map_err(AgentError::Entity)?;
        // This will panic when called due to unimplemented!() in modification module
        modification::execute_plan(&mut self.entities, &plan).map_err(AgentError::Entity)
    }

    /// Update entities after modification (stub - calls unimplemented update logic)
    async fn update_entities(&mut self) -> AgentResult<()> {
        // This will panic when called due to unimplemented!() in modification module
        modification::update_entities(&mut self.entities, &[]).map_err(AgentError::Entity)
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

    #[test]
    fn test_agent_state_transitions() {
        let state = AgentState::Enriching;
        assert_eq!(state, AgentState::Enriching);

        let state = AgentState::Planning;
        assert_eq!(state, AgentState::Planning);

        let state = AgentState::Completed;
        assert_eq!(state, AgentState::Completed);
    }

    #[test]
    fn test_agent_loop_creation() {
        let config = AgentConfig::default();
        let agent = AgentLoop::new(config);
        assert_eq!(agent.state(), &AgentState::Enriching);
        assert_eq!(agent.entities().len(), 0);
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_iterations, 100);
        assert!(!config.verbose);
    }

    #[tokio::test]
    async fn test_agent_run_structure() {
        // Test that the agent can be created and configured
        // without running the full loop (which requires unimplemented functions)
        let config = AgentConfig {
            max_iterations: 10,
            verbose: false,
            enrichment: EnrichmentConfig::default(),
        };
        let agent = AgentLoop::new(config);

        // Verify initial state
        assert_eq!(agent.state(), &AgentState::Enriching);
        assert_eq!(agent.entities().len(), 0);

        // Note: We don't call agent.run() here because enrichment is unimplemented.
        // In production, enrichment would be implemented and we could test the full loop.
    }

    #[tokio::test]
    async fn test_agent_configuration() {
        // Test different agent configurations
        let config = AgentConfig {
            max_iterations: 2,
            verbose: true,
            enrichment: EnrichmentConfig {
                infer_types: true,
                extract_docs: false,
                analyze_dependencies: true,
                semantic_analysis: false,
            },
        };
        let agent = AgentLoop::new(config);

        assert_eq!(agent.state(), &AgentState::Enriching);

        // Verify we can construct the context
        let _context = AgentContext {
            user_prompt: "test prompt".to_string(),
            conversation_history: vec![],
            app_state_id: "test_state".to_string(),
        };

        // Note: We don't run the agent loop because it depends on unimplemented functions
    }
}
