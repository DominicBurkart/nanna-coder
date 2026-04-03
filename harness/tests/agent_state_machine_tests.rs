//! Unit tests for the agent state-machine transitions (ARCHITECTURE.md).
//!
//! These tests use [`SequenceMockProvider`] from the shared `common` module so
//! that the agent loop can be driven end-to-end without a real LLM or container.
//! They complement the broader integration tests in `integration_tests.rs` by
//! focusing exclusively on state-transition correctness and completion-signal
//! recognition.

// Pull in the shared mock helpers without duplicating code.
#[path = "common/mod.rs"]
mod common;

use std::sync::Arc;

use harness::{
    agent::{AgentConfig, AgentContext, AgentLoop, AgentState},
    entities::InMemoryEntityStore,
    tools::{EchoTool, ToolRegistry},
};
use model::types::{ChatMessage, ChatResponse, Choice, FinishReason};
use serde_json::json;

use common::{make_stop_response, make_tool_call, wrap_with_state_machine_responses,
             SequenceMockProvider};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Minimal [`AgentConfig`] for state-machine tests.
fn default_config() -> AgentConfig {
    AgentConfig {
        max_iterations: 30,
        verbose: false,
        system_prompt: "You are a test assistant.".to_string(),
        model_name: "test-model".to_string(),
    }
}

/// Build an [`AgentContext`] with the given user prompt.
fn make_context(prompt: &str) -> AgentContext {
    AgentContext {
        user_prompt: prompt.to_string(),
        conversation_history: vec![ChatMessage::user(prompt)],
        app_state_id: "state_machine_test".to_string(),
    }
}

/// Build a tool-call response for the echo tool.
fn echo_tool_call_response() -> ChatResponse {
    ChatResponse {
        choices: vec![Choice {
            message: model::types::ChatMessage::assistant_with_tools(
                None,
                vec![make_tool_call(
                    "call_1",
                    "echo",
                    json!({"message": "state machine test"}),
                )],
            ),
            finish_reason: Some(FinishReason::ToolCalls),
        }],
        usage: None,
    }
}

/// Registry with just the echo tool — enough for most state-machine tests.
fn echo_registry() -> ToolRegistry {
    let mut r = ToolRegistry::new();
    r.register(Box::new(EchoTool::new()));
    r
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The agent must transition through the full architectural state sequence on a
/// simple task that requires no re-planning:
///
/// EnrichingEntities → PlanningEntityModification → PerformingEntityModification
///   → UpdatingEntities → CheckingTaskCompletion → Completed
#[tokio::test]
async fn agent_loop_transitions_plan_to_perform_to_complete() {
    let provider = Arc::new(SequenceMockProvider::new(
        wrap_with_state_machine_responses(vec![make_stop_response("Action taken.")]),
    ));

    let mut agent = AgentLoop::with_tools(
        default_config(),
        InMemoryEntityStore::new(),
        provider,
        echo_registry(),
    );

    let result = agent.run(make_context("Do a simple task")).await.unwrap();
    assert!(result.task_completed, "task should be marked complete");

    let history = agent.state_history();
    assert!(
        history.len() >= 4,
        "expected at least 4 state transitions, got {:?}",
        history
    );

    // Verify the canonical ordering of the first four recorded states.
    assert_eq!(
        history[0],
        AgentState::PlanningEntityModification,
        "first recorded state should be PlanningEntityModification (EnrichingEntities produces no LLM call)"
    );
    assert_eq!(
        history[1],
        AgentState::PerformingEntityModification,
        "should move from Planning to Performing"
    );
    assert_eq!(
        history[2],
        AgentState::UpdatingEntities,
        "should move from Performing to UpdatingEntities"
    );
    assert_eq!(
        history[3],
        AgentState::CheckingTaskCompletion,
        "should move from Updating to CheckingTaskCompletion"
    );
    assert!(
        history.contains(&AgentState::Completed),
        "Completed state must appear in history"
    );
}

/// When the LLM response for CheckingTaskCompletion starts with or contains
/// "COMPLETE", the agent should stop and report `task_completed = true`.
#[tokio::test]
async fn agent_loop_recognizes_completion_signal() {
    // The completion response is produced by wrap_with_state_machine_responses.
    let provider = Arc::new(SequenceMockProvider::new(
        wrap_with_state_machine_responses(vec![make_stop_response("All done.")]),
    ));

    let mut agent = AgentLoop::with_tools(
        default_config(),
        InMemoryEntityStore::new(),
        provider,
        echo_registry(),
    );

    let result = agent.run(make_context("Complete this task")).await.unwrap();
    assert!(
        result.task_completed,
        "task_completed must be true when COMPLETE signal is present"
    );
    assert_eq!(
        result.final_state,
        AgentState::Completed,
        "final_state must be Completed"
    );
}

/// When the completion check returns INCOMPLETE the agent must re-plan, and
/// `PlanningEntityModification` must appear at least twice in the state history.
#[tokio::test]
async fn agent_loop_replans_when_incomplete() {
    let provider = Arc::new(SequenceMockProvider::new(vec![
        // Enrich: no LLM
        make_stop_response("Plan: first attempt"),  // PlanningEntityModification (1st)
        make_stop_response("First action done."),   // PerformingEntityModification
        // Update: no LLM
        make_stop_response("INCOMPLETE"),            // CheckingTaskCompletion → re-loop
        make_stop_response("PROCEED"),               // EntityModificationDecision
        make_stop_response("Plan: second attempt"), // PlanningEntityModification (2nd)
        make_stop_response("Second action done."),  // PerformingEntityModification
        // Update: no LLM
        make_stop_response("COMPLETE"),              // CheckingTaskCompletion
    ]));

    let mut agent = AgentLoop::with_tools(
        default_config(),
        InMemoryEntityStore::new(),
        provider,
        echo_registry(),
    );

    let result = agent.run(make_context("Two-step task")).await.unwrap();
    assert!(result.task_completed);

    let planning_count = agent
        .state_history()
        .iter()
        .filter(|s| **s == AgentState::PlanningEntityModification)
        .count();
    assert!(
        planning_count >= 2,
        "agent should re-plan after INCOMPLETE; got {} planning transitions",
        planning_count
    );
}

/// The agent must visit `QueryingEntities` when the decision response signals
/// QUERY before eventually completing.
#[tokio::test]
async fn agent_loop_enters_querying_entities_on_query_decision() {
    let provider = Arc::new(SequenceMockProvider::new(vec![
        // Enrich: no LLM
        make_stop_response("Plan: initial"),          // PlanningEntityModification
        make_stop_response("Action done."),           // PerformingEntityModification
        // Update: no LLM
        make_stop_response("INCOMPLETE"),              // CheckingTaskCompletion
        make_stop_response("QUERY - need context"),   // EntityModificationDecision → query branch
        // QueryingEntities: no LLM, loops back to Decision
        make_stop_response("PROCEED"),                // EntityModificationDecision → plan branch
        make_stop_response("Revised plan"),           // PlanningEntityModification (2nd)
        make_stop_response("Revised action done."),   // PerformingEntityModification (2nd)
        // Update: no LLM
        make_stop_response("COMPLETE"),               // CheckingTaskCompletion
    ]));

    let mut agent = AgentLoop::with_tools(
        AgentConfig {
            max_iterations: 30,
            ..default_config()
        },
        InMemoryEntityStore::new(),
        provider,
        echo_registry(),
    );

    let result = agent.run(make_context("Query loop task")).await.unwrap();
    assert!(result.task_completed);

    let history = agent.state_history();
    assert!(
        history.contains(&AgentState::QueryingEntities),
        "QueryingEntities must appear when decision response contains QUERY; history: {:?}",
        history
    );
}

/// Tool calls made during PerformingEntityModification must be recorded in the
/// conversation history and in the run result's `tool_calls_made`.
#[tokio::test]
async fn agent_loop_records_tool_calls_in_result() {
    let provider = Arc::new(SequenceMockProvider::new(
        wrap_with_state_machine_responses(vec![
            echo_tool_call_response(),
            make_stop_response("Echo tool called."),
        ]),
    ));

    let mut agent = AgentLoop::with_tools(
        default_config(),
        InMemoryEntityStore::new(),
        provider,
        echo_registry(),
    );

    let result = agent.run(make_context("Echo something")).await.unwrap();
    assert!(result.task_completed);

    assert!(
        !result.tool_calls_made.is_empty(),
        "at least one tool call should be recorded in the run result"
    );
    assert_eq!(
        result.tool_calls_made[0].tool_name, "echo",
        "the recorded tool call should be for the echo tool"
    );
}
