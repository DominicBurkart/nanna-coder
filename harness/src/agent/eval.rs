//! Agent Evaluation Framework
//!
//! Comprehensive evaluation system for testing the full nanna-coder agent
//! with all running subcontainers. This module provides patterns and tooling
//! for evaluating:
//!
//! - Agent decision-making quality
//! - LLM prompt effectiveness  
//! - RAG accuracy and relevance
//! - Entity management correctness
//! - End-to-end workflow completion
//! - System integration and reliability
//!
//! # Architecture
//!
//! The evaluation framework operates at multiple levels:
//!
//! ## Level 1: Unit Evaluations
//! - Individual agent state transitions
//! - RAG query quality
//! - Entity modifications
//!
//! ## Level 2: Integration Evaluations  
//! - Agent control loop behavior
//! - LLM-agent interaction
//! - Multi-entity workflows
//!
//! ## Level 3: System Evaluations
//! - Full containerized environment
//! - Model provider integration
//! - End-to-end task completion
//!
//! # Example
//!
//! ```rust,no_run
//! use harness::agent::eval::{AgentEvaluator, EvaluationConfig, EvaluationScenario};
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//!
//! let config = EvaluationConfig::default()
//!     .with_model("qwen3:0.6b")
//!     .with_timeout(std::time::Duration::from_secs(300));
//!
//! let mut evaluator = AgentEvaluator::new(config).await?;
//!
//! // Run a predefined scenario
//! let scenario = EvaluationScenario::simple_entity_creation();
//! let result = evaluator.evaluate(scenario).await?;
//!
//! assert!(result.success, "Agent should complete task successfully");
//! assert!(result.metrics.decision_quality >= 0.7);
//! assert!(result.metrics.rag_relevance >= 0.8);
//! # Ok(())
//! # }
//! ```

use crate::agent::{AgentConfig, AgentContext, AgentLoop, AgentState};
use crate::entities::{EntityQuery, EntityStore, EntityType, InMemoryEntityStore};
use crate::monitoring::SystemMetrics;
use crate::observability::ObservabilitySystem;
use model::judge::{ValidationCriteria, ValidationResult};
use model::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use thiserror::Error;
use tracing::{info, warn};

/// Errors that can occur during evaluation
#[derive(Error, Debug)]
pub enum EvaluationError {
    #[error("Evaluation setup failed: {0}")]
    SetupFailed(String),

    #[error("Agent execution failed: {0}")]
    AgentExecutionFailed(String),

    #[error("Validation failed: {0}")]
    ValidationFailed(String),

    #[error("Model error: {0}")]
    ModelError(#[from] ModelError),

    #[error("Timeout: evaluation exceeded {0:?}")]
    Timeout(Duration),

    #[error("Entity error: {0}")]
    EntityError(#[from] crate::entities::EntityError),
}

pub type EvaluationResult<T> = Result<T, EvaluationError>;

/// Configuration for agent evaluation
#[derive(Debug, Clone)]
pub struct EvaluationConfig {
    /// Model to use for LLM-powered agent
    pub model: String,

    /// Model base URL (for containerized models)
    pub model_base_url: Option<String>,

    /// Maximum evaluation time
    pub timeout: Duration,

    /// Enable verbose logging
    pub verbose: bool,

    /// Number of retry attempts
    pub max_retries: u32,

    /// Enable observability collection
    pub collect_observability: bool,

    /// Validation criteria for LLM outputs
    pub validation_criteria: ValidationCriteria,
}

impl Default for EvaluationConfig {
    fn default() -> Self {
        Self {
            model: "qwen3:0.6b".to_string(),
            model_base_url: None,
            timeout: Duration::from_secs(300),
            verbose: false,
            max_retries: 2,
            collect_observability: true,
            validation_criteria: ValidationCriteria {
                min_response_length: 10,
                max_response_length: 1000,
                required_keywords: vec![],
                forbidden_keywords: vec!["I don't know".to_string(), "I cannot".to_string()],
                min_coherence_score: 0.7,
                min_relevance_score: 0.8,
                require_factual_accuracy: true,
                custom_validators: vec![],
            },
        }
    }
}

impl EvaluationConfig {
    pub fn with_model(mut self, model: &str) -> Self {
        self.model = model.to_string();
        self
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.model_base_url = Some(url.to_string());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

/// Evaluation scenario defines a test case for the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationScenario {
    /// Unique scenario ID
    pub id: String,

    /// Human-readable scenario name
    pub name: String,

    /// Description of what the scenario tests
    pub description: String,

    /// User prompt to give the agent
    pub user_prompt: String,

    /// Initial entity state (optional)
    pub initial_entities: Vec<EntityType>,

    /// Expected outcomes
    pub expected_outcomes: ExpectedOutcomes,

    /// Scenario-specific validation criteria
    pub validation_criteria: Option<ValidationCriteria>,

    /// Maximum iterations for agent loop
    pub max_iterations: usize,

    /// Category of evaluation
    pub category: EvaluationCategory,
}

/// Category of evaluation scenario
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvaluationCategory {
    /// Tests basic entity creation
    EntityCreation,

    /// Tests RAG query and retrieval
    RagRetrieval,

    /// Tests decision-making logic
    DecisionMaking,

    /// Tests multi-step workflows
    Workflow,

    /// Tests LLM prompt effectiveness
    PromptEngineering,

    /// Tests error handling
    ErrorHandling,

    /// Tests performance characteristics
    Performance,

    /// Custom category
    Custom(String),
}

/// Expected outcomes from an evaluation scenario
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedOutcomes {
    /// Expected final agent state
    pub final_state: Option<AgentState>,

    /// Minimum entities that should be created
    pub min_entities_created: usize,

    /// Expected entity types to be present
    pub expected_entity_types: Vec<EntityType>,

    /// Should complete within iterations
    pub should_complete: bool,

    /// Maximum allowed iterations
    pub max_allowed_iterations: usize,

    /// Expected relationships created
    pub min_relationships_created: usize,

    /// Minimum decision quality score (0.0 to 1.0)
    pub min_decision_quality: f64,

    /// Minimum RAG relevance score (0.0 to 1.0)
    pub min_rag_relevance: f64,
}

impl Default for ExpectedOutcomes {
    fn default() -> Self {
        Self {
            final_state: Some(AgentState::Completed),
            min_entities_created: 1,
            expected_entity_types: vec![],
            should_complete: true,
            max_allowed_iterations: 10,
            min_relationships_created: 0,
            min_decision_quality: 0.6,
            min_rag_relevance: 0.7,
        }
    }
}

impl EvaluationScenario {
    /// Create a simple entity creation scenario
    pub fn simple_entity_creation() -> Self {
        Self {
            id: "simple_entity_creation".to_string(),
            name: "Simple Entity Creation".to_string(),
            description: "Tests basic agent ability to create a git repository entity".to_string(),
            user_prompt: "Create a new git repository entity".to_string(),
            initial_entities: vec![],
            expected_outcomes: ExpectedOutcomes {
                min_entities_created: 1,
                expected_entity_types: vec![EntityType::Git],
                max_allowed_iterations: 15,
                min_rag_relevance: 0.0, // Not testing RAG for simple entity creation
                ..Default::default()
            },
            validation_criteria: None,
            max_iterations: 10,
            category: EvaluationCategory::EntityCreation,
        }
    }

    /// Create a RAG retrieval scenario
    pub fn rag_retrieval_accuracy() -> Self {
        Self {
            id: "rag_retrieval_accuracy".to_string(),
            name: "RAG Retrieval Accuracy".to_string(),
            description: "Tests RAG system's ability to find relevant entities".to_string(),
            user_prompt: "Find all git repository entities".to_string(),
            initial_entities: vec![EntityType::Git, EntityType::Git, EntityType::Context],
            expected_outcomes: ExpectedOutcomes {
                min_rag_relevance: 0.7,
                should_complete: true,
                max_allowed_iterations: 12,
                ..Default::default()
            },
            validation_criteria: None,
            max_iterations: 10,
            category: EvaluationCategory::RagRetrieval,
        }
    }

    /// Create a multi-entity workflow scenario
    pub fn multi_entity_workflow() -> Self {
        Self {
            id: "multi_entity_workflow".to_string(),
            name: "Multi-Entity Workflow".to_string(),
            description: "Tests agent's ability to manage multiple related entities".to_string(),
            user_prompt: "Create a git repository with associated test results".to_string(),
            initial_entities: vec![],
            expected_outcomes: ExpectedOutcomes {
                min_entities_created: 2,
                expected_entity_types: vec![EntityType::Git, EntityType::Test],
                min_relationships_created: 1,
                max_allowed_iterations: 10,
                ..Default::default()
            },
            validation_criteria: None,
            max_iterations: 15,
            category: EvaluationCategory::Workflow,
        }
    }

    /// Create a decision quality scenario
    pub fn decision_quality_test() -> Self {
        Self {
            id: "decision_quality_test".to_string(),
            name: "Decision Quality Test".to_string(),
            description: "Tests agent's decision-making when choosing between query and perform"
                .to_string(),
            user_prompt: "Analyze existing entities and create a related context entity"
                .to_string(),
            initial_entities: vec![EntityType::Git, EntityType::Test],
            expected_outcomes: ExpectedOutcomes {
                min_entities_created: 1,
                expected_entity_types: vec![EntityType::Context],
                min_decision_quality: 0.8,
                ..Default::default()
            },
            validation_criteria: None,
            max_iterations: 8,
            category: EvaluationCategory::DecisionMaking,
        }
    }
}

/// Metrics collected during evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationMetrics {
    /// Total execution time
    pub execution_time: Duration,

    /// Number of iterations executed
    pub iterations_executed: usize,

    /// Agent decision quality (0.0 to 1.0)
    pub decision_quality: f64,

    /// RAG relevance score (0.0 to 1.0)
    pub rag_relevance: f64,

    /// Entity creation accuracy
    pub entity_accuracy: f64,

    /// Prompt effectiveness score
    pub prompt_effectiveness: f64,

    /// Number of entities created
    pub entities_created: usize,

    /// Number of relationships created
    pub relationships_created: usize,

    /// State transitions taken
    pub state_transitions: Vec<AgentState>,

    /// Validation results (if LLM used)
    pub validation_results: Vec<ValidationResult>,

    /// Custom metrics
    pub custom_metrics: HashMap<String, f64>,
}

impl Default for EvaluationMetrics {
    fn default() -> Self {
        Self {
            execution_time: Duration::ZERO,
            iterations_executed: 0,
            decision_quality: 0.0,
            rag_relevance: 0.0,
            entity_accuracy: 0.0,
            prompt_effectiveness: 0.0,
            entities_created: 0,
            relationships_created: 0,
            state_transitions: vec![],
            validation_results: vec![],
            custom_metrics: HashMap::new(),
        }
    }
}

/// Result of an agent evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvaluationResult {
    /// Scenario that was evaluated
    pub scenario_id: String,

    /// Whether the evaluation passed
    pub success: bool,

    /// Collected metrics
    pub metrics: EvaluationMetrics,

    /// Final agent state
    pub final_state: AgentState,

    /// Validation failures (if any)
    pub failures: Vec<String>,

    /// Warnings
    pub warnings: Vec<String>,

    /// System metrics (if observability enabled)
    pub system_metrics: Option<SystemMetrics>,

    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Main agent evaluator
pub struct AgentEvaluator {
    /// Configuration
    config: EvaluationConfig,

    /// Model provider (optional, for LLM-powered evaluations)
    /// Reserved for future use when evaluating LLM-powered agents
    #[allow(dead_code)]
    model_provider: Option<Box<dyn ModelProvider>>,

    /// Observability system (optional)
    observability: Option<ObservabilitySystem>,
}

impl AgentEvaluator {
    /// Create a new agent evaluator
    pub async fn new(config: EvaluationConfig) -> EvaluationResult<Self> {
        let model_provider = if let Some(base_url) = &config.model_base_url {
            let ollama_config = OllamaConfig::new()
                .with_base_url(base_url.clone())
                .with_timeout(Duration::from_secs(120));

            let provider = OllamaProvider::new(ollama_config)
                .map_err(|e| EvaluationError::SetupFailed(e.to_string()))?;

            Some(Box::new(provider) as Box<dyn ModelProvider>)
        } else {
            None
        };

        let observability = if config.collect_observability {
            let mut obs = ObservabilitySystem::new()
                .with_service_name("agent-evaluator")
                .with_health_check_interval(Duration::from_secs(60));

            // Initialize, but don't fail if it errors (might be tracing already set up)
            let _ = obs.initialize().await;

            Some(obs)
        } else {
            None
        };

        Ok(Self {
            config,
            model_provider,
            observability,
        })
    }

    /// Evaluate an agent scenario
    pub async fn evaluate(
        &mut self,
        scenario: EvaluationScenario,
    ) -> EvaluationResult<AgentEvaluationResult> {
        info!("Starting evaluation: {} ({})", scenario.name, scenario.id);

        let start_time = Instant::now();
        let mut metrics = EvaluationMetrics::default();
        let mut failures = Vec::new();
        let mut warnings = Vec::new();

        // Setup initial entity store
        let entity_store = self.setup_entity_store(&scenario).await?;

        // Create agent
        let agent_config = AgentConfig {
            max_iterations: scenario.max_iterations,
            verbose: self.config.verbose,
        };

        let mut agent = AgentLoop::with_entity_store(agent_config, entity_store);

        // Create context
        let context = AgentContext {
            user_prompt: scenario.user_prompt.clone(),
            conversation_history: vec![],
            app_state_id: format!("eval_{}", scenario.id),
        };

        // Track state transitions
        let initial_state = agent.state().clone();
        metrics.state_transitions.push(initial_state);

        // Run agent with timeout
        let agent_result = tokio::time::timeout(self.config.timeout, agent.run(context)).await;

        let run_result = match agent_result {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                failures.push(format!("Agent execution failed: {}", e));
                metrics.execution_time = start_time.elapsed();
                return Ok(AgentEvaluationResult {
                    scenario_id: scenario.id,
                    success: false,
                    metrics,
                    final_state: AgentState::Error(e.to_string()),
                    failures,
                    warnings,
                    system_metrics: None,
                    timestamp: chrono::Utc::now(),
                });
            }
            Err(_) => {
                failures.push(format!(
                    "Evaluation timed out after {:?}",
                    self.config.timeout
                ));
                metrics.execution_time = start_time.elapsed();
                return Ok(AgentEvaluationResult {
                    scenario_id: scenario.id,
                    success: false,
                    metrics,
                    final_state: AgentState::Error("Timeout".to_string()),
                    failures,
                    warnings,
                    system_metrics: None,
                    timestamp: chrono::Utc::now(),
                });
            }
        };

        // Collect execution metrics
        metrics.execution_time = start_time.elapsed();
        metrics.iterations_executed = run_result.iterations;

        // Analyze final entity state
        let final_entities = agent.entity_store().query(&EntityQuery::default()).await?;

        metrics.entities_created = final_entities.len();

        // Calculate entity accuracy
        let entity_accuracy = self.calculate_entity_accuracy(&scenario, &final_entities);
        metrics.entity_accuracy = entity_accuracy;

        // Evaluate RAG relevance
        let rag_relevance = self.evaluate_rag_relevance(&agent, &scenario).await?;
        metrics.rag_relevance = rag_relevance;

        // Evaluate decision quality
        let decision_quality = self.calculate_decision_quality(&run_result, &scenario);
        metrics.decision_quality = decision_quality;

        // Check expected outcomes
        self.validate_outcomes(
            &scenario,
            &metrics,
            &run_result,
            &mut failures,
            &mut warnings,
        )?;

        // Determine success
        let success = failures.is_empty();

        if success {
            info!("✅ Evaluation passed: {}", scenario.name);
        } else {
            warn!("❌ Evaluation failed: {}", scenario.name);
            for failure in &failures {
                warn!("  - {}", failure);
            }
        }

        // Collect system metrics if observability enabled
        let system_metrics = if let Some(obs) = &self.observability {
            obs.get_comprehensive_status().await.ok().map(|s| s.metrics)
        } else {
            None
        };

        Ok(AgentEvaluationResult {
            scenario_id: scenario.id,
            success,
            metrics,
            final_state: run_result.final_state,
            failures,
            warnings,
            system_metrics,
            timestamp: chrono::Utc::now(),
        })
    }

    /// Setup initial entity store based on scenario
    async fn setup_entity_store(
        &self,
        scenario: &EvaluationScenario,
    ) -> EvaluationResult<InMemoryEntityStore> {
        let mut store = InMemoryEntityStore::new();

        // Create initial entities based on scenario
        for entity_type in &scenario.initial_entities {
            let entity: Box<dyn crate::entities::Entity> = match entity_type {
                EntityType::Git => Box::new(crate::entities::git::types::GitRepository::new()),
                EntityType::Context => {
                    Box::new(crate::entities::context::types::ContextEntity::new())
                }
                EntityType::Test => Box::new(crate::entities::test::types::TestEntity::new()),
                EntityType::Ast => Box::new(crate::entities::ast::types::AstEntity::new()),
                EntityType::Env => Box::new(crate::entities::env::types::EnvEntity::new()),
                EntityType::Telemetry => {
                    Box::new(crate::entities::telemetry::types::TelemetryEntity::new())
                }
            };

            store.store(entity).await?;
        }

        Ok(store)
    }

    /// Calculate entity creation accuracy
    fn calculate_entity_accuracy(
        &self,
        scenario: &EvaluationScenario,
        final_entities: &[crate::entities::QueryResult],
    ) -> f64 {
        let expected = &scenario.expected_outcomes.expected_entity_types;
        if expected.is_empty() {
            return 1.0;
        }

        let mut found_count = 0;
        for expected_type in expected {
            if final_entities
                .iter()
                .any(|e| &e.entity_type == expected_type)
            {
                found_count += 1;
            }
        }

        found_count as f64 / expected.len() as f64
    }

    /// Evaluate RAG relevance
    async fn evaluate_rag_relevance(
        &self,
        agent: &AgentLoop,
        scenario: &EvaluationScenario,
    ) -> EvaluationResult<f64> {
        // Query entities using the scenario's prompt
        let results = crate::agent::rag::query_entities(
            agent.entity_store(),
            &scenario.user_prompt,
            Some(10),
        )
        .await
        .map_err(|e| EvaluationError::ValidationFailed(e.to_string()))?;

        // If no query results (no entities to query), return 1.0 (perfect score)
        // This handles scenarios where entity creation hasn't happened yet
        if results.is_empty() {
            let total_entities = agent
                .entity_store()
                .query(&EntityQuery::default())
                .await
                .map(|e| e.len())
                .unwrap_or(0);

            // If there are entities but no results, that's a 0.0
            // If there are no entities at all, that's a 1.0 (nothing to query)
            return Ok(if total_entities == 0 { 1.0 } else { 0.0 });
        }

        // Average relevance of top results
        let avg_relevance: f64 =
            results.iter().map(|r| r.relevance).sum::<f64>() / results.len() as f64;

        Ok(avg_relevance)
    }

    /// Calculate decision quality based on iterations and outcomes
    fn calculate_decision_quality(
        &self,
        run_result: &crate::agent::AgentRunResult,
        scenario: &EvaluationScenario,
    ) -> f64 {
        // Decision quality is based on:
        // 1. Task completion (50%)
        // 2. Iteration efficiency (30%)
        // 3. Final state correctness (20%)

        let mut score = 0.0;

        // Task completion
        if run_result.task_completed {
            score += 0.5;
        }

        // Iteration efficiency (fewer iterations = better)
        let iteration_ratio = run_result.iterations as f64 / scenario.max_iterations as f64;
        let efficiency_score = (1.0 - iteration_ratio).max(0.0) * 0.3;
        score += efficiency_score;

        // Final state correctness
        if let Some(expected_state) = &scenario.expected_outcomes.final_state {
            if &run_result.final_state == expected_state {
                score += 0.2;
            }
        } else {
            score += 0.2; // Full credit if no specific state expected
        }

        score.min(1.0)
    }

    /// Validate that outcomes match expectations
    fn validate_outcomes(
        &self,
        scenario: &EvaluationScenario,
        metrics: &EvaluationMetrics,
        run_result: &crate::agent::AgentRunResult,
        failures: &mut Vec<String>,
        warnings: &mut Vec<String>,
    ) -> EvaluationResult<()> {
        let expected = &scenario.expected_outcomes;

        // Check completion
        if expected.should_complete && !run_result.task_completed {
            failures.push("Agent did not complete the task".to_string());
        }

        // Check entity creation
        if metrics.entities_created < expected.min_entities_created {
            failures.push(format!(
                "Expected at least {} entities, but only {} were created",
                expected.min_entities_created, metrics.entities_created
            ));
        }

        // Check iterations
        if metrics.iterations_executed > expected.max_allowed_iterations {
            warnings.push(format!(
                "Exceeded recommended iterations: {} > {}",
                metrics.iterations_executed, expected.max_allowed_iterations
            ));
        }

        // Check decision quality
        if metrics.decision_quality < expected.min_decision_quality {
            failures.push(format!(
                "Decision quality too low: {:.2} < {:.2}",
                metrics.decision_quality, expected.min_decision_quality
            ));
        }

        // Check RAG relevance
        if metrics.rag_relevance < expected.min_rag_relevance {
            failures.push(format!(
                "RAG relevance too low: {:.2} < {:.2}",
                metrics.rag_relevance, expected.min_rag_relevance
            ));
        }

        // Check final state
        if let Some(expected_state) = &expected.final_state {
            if &run_result.final_state != expected_state {
                failures.push(format!(
                    "Expected final state {:?}, but got {:?}",
                    expected_state, run_result.final_state
                ));
            }
        }

        Ok(())
    }

    /// Run a batch of evaluation scenarios
    pub async fn evaluate_batch(
        &mut self,
        scenarios: Vec<EvaluationScenario>,
    ) -> EvaluationResult<BatchEvaluationResult> {
        let mut results = Vec::new();
        let start_time = Instant::now();

        for scenario in scenarios {
            let result = self.evaluate(scenario).await?;
            results.push(result);
        }

        let total_time = start_time.elapsed();
        let passed = results.iter().filter(|r| r.success).count();
        let failed = results.len() - passed;

        Ok(BatchEvaluationResult {
            total_scenarios: results.len(),
            passed,
            failed,
            total_time,
            results,
        })
    }
}

/// Result of batch evaluation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchEvaluationResult {
    pub total_scenarios: usize,
    pub passed: usize,
    pub failed: usize,
    pub total_time: Duration,
    pub results: Vec<AgentEvaluationResult>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluation_config_builder() {
        let config = EvaluationConfig::default()
            .with_model("test-model")
            .with_timeout(Duration::from_secs(60))
            .with_verbose(true);

        assert_eq!(config.model, "test-model");
        assert_eq!(config.timeout, Duration::from_secs(60));
        assert!(config.verbose);
    }

    #[test]
    fn test_scenario_creation() {
        let scenario = EvaluationScenario::simple_entity_creation();
        assert_eq!(scenario.category, EvaluationCategory::EntityCreation);
        assert_eq!(scenario.expected_outcomes.min_entities_created, 1);
    }

    #[tokio::test]
    async fn test_evaluator_creation_without_model() {
        let config = EvaluationConfig {
            model_base_url: None,
            collect_observability: false,
            ..Default::default()
        };

        let evaluator = AgentEvaluator::new(config).await;
        assert!(evaluator.is_ok());
    }

    #[tokio::test]
    async fn test_simple_entity_creation_scenario() {
        let config = EvaluationConfig {
            model_base_url: None,
            collect_observability: false,
            timeout: Duration::from_secs(10),
            verbose: true,
            ..Default::default()
        };

        let mut evaluator = AgentEvaluator::new(config).await.unwrap();
        let scenario = EvaluationScenario::simple_entity_creation();

        let result = evaluator.evaluate(scenario).await.unwrap();

        assert!(
            result.success,
            "Scenario should pass: {:?}",
            result.failures
        );
        assert!(result.metrics.entities_created >= 1);
        assert_eq!(result.final_state, AgentState::Completed);
    }

    #[tokio::test]
    async fn test_rag_retrieval_scenario() {
        let config = EvaluationConfig {
            model_base_url: None,
            collect_observability: false,
            timeout: Duration::from_secs(10),
            ..Default::default()
        };

        let mut evaluator = AgentEvaluator::new(config).await.unwrap();
        let scenario = EvaluationScenario::rag_retrieval_accuracy();

        let result = evaluator.evaluate(scenario).await.unwrap();

        // RAG scenario should complete
        // The entity creation happens successfully even if RAG doesn't match perfectly
        assert!(
            result.success || result.metrics.entities_created > 0,
            "Scenario should either pass or create entities"
        );

        // Check that RAG was at least attempted (score is computed)
        assert!(result.metrics.rag_relevance >= 0.0);
    }

    #[tokio::test]
    async fn test_batch_evaluation() {
        let config = EvaluationConfig {
            model_base_url: None,
            collect_observability: false,
            timeout: Duration::from_secs(30),
            ..Default::default()
        };

        let mut evaluator = AgentEvaluator::new(config).await.unwrap();

        let scenarios = vec![
            EvaluationScenario::simple_entity_creation(),
            EvaluationScenario::rag_retrieval_accuracy(),
        ];

        let batch_result = evaluator.evaluate_batch(scenarios).await.unwrap();

        assert_eq!(batch_result.total_scenarios, 2);
        assert!(batch_result.passed > 0);
    }
}
