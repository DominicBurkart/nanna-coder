//! Common test helpers shared across integration and unit test files.
//!
//! Provides [`SequenceMockProvider`] — a scripted [`ModelProvider`] that returns
//! pre-canned [`ChatResponse`] values in order — plus helper functions for
//! constructing responses and wrapping them with the agent state-machine scaffolding
//! required by the Harness Control Flow (ARCHITECTURE.md).
//!
//! # Usage
//!
//! Add the following at the top of any test file that needs these helpers:
//!
//! ```rust,ignore
//! mod common;
//! use common::{SequenceMockProvider, make_tool_call, make_stop_response,
//!              wrap_with_state_machine_responses};
//! ```

#![allow(dead_code)]

use async_trait::async_trait;
use model::{
    ModelError, ModelProvider, ModelResult,
    types::{ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, ModelInfo, ToolCall},
};
use model::types::ChatMessage;
use std::sync::Mutex;

/// A scripted [`ModelProvider`] that pops and returns responses in FIFO order.
///
/// Each call to [`ModelProvider::chat`] removes and returns the next response
/// from the front of the queue.  When the queue is exhausted, further calls
/// return [`ModelError::ServiceUnavailable`].
///
/// Construct with [`SequenceMockProvider::new`], wrapping with
/// [`wrap_with_state_machine_responses`] when you need to account for the full
/// agent state machine (EnrichingEntities → PlanningEntityModification →
/// PerformingEntityModification → UpdatingEntities → CheckingTaskCompletion).
pub struct SequenceMockProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl SequenceMockProvider {
    /// Create a new provider from an ordered list of scripted responses.
    pub fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl ModelProvider for SequenceMockProvider {
    async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err(ModelError::ServiceUnavailable {
                message: "No more scripted responses".to_string(),
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
        "sequence_mock"
    }
}

/// Build a [`ToolCall`] value for use in assistant messages.
pub fn make_tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: args,
        },
    }
}

/// Build a [`ChatResponse`] whose single choice has `finish_reason = Stop`
/// and the given text content.
pub fn make_stop_response(content: &str) -> ChatResponse {
    ChatResponse {
        choices: vec![Choice {
            message: ChatMessage::assistant(content),
            finish_reason: Some(FinishReason::Stop),
        }],
        usage: None,
    }
}

/// Wrap tool-loop responses with the state-machine scaffolding required for a
/// full agent run:
///
/// ```text
/// EnrichingEntities          — no LLM call
/// PlanningEntityModification — "Plan: execute the task"
/// PerformingEntityModification — <tool_responses>
/// UpdatingEntities           — no LLM call
/// CheckingTaskCompletion     — "COMPLETE - task done"
/// ```
///
/// Pass only the responses needed for the *PerformingEntityModification* phase;
/// this function adds the surrounding planning and completion responses.
pub fn wrap_with_state_machine_responses(tool_responses: Vec<ChatResponse>) -> Vec<ChatResponse> {
    let mut responses = vec![
        make_stop_response("Plan: execute the task"), // PlanningEntityModification
    ];
    responses.extend(tool_responses); // PerformingEntityModification
    // UpdatingEntities: no LLM call
    responses.push(make_stop_response("COMPLETE - task done")); // CheckingTaskCompletion
    responses
}
