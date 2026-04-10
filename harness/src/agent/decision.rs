//! Entity Modification Decision (placeholder)
//!
//! This module is a **stub** for the "Entity Modification Decision" node in the
//! ARCHITECTURE.md Harness Control Flow diagram. The node decides whether the
//! agent should query entities for more context (RAG) or proceed to plan the
//! next modification.
//!
//! The actual decision logic lives in [`crate::agent::prompts::DecisionPrompt`],
//! which builds and parses QUERY/PROCEED LLM responses. This module will hold
//! higher-level orchestration once the interface is stabilised.

use thiserror::Error;

/// Errors produced by entity modification decision logic.
#[derive(Error, Debug)]
pub enum DecisionError {
    #[error("Entity modification decision error: {0}")]
    DecisionFailed(String),
}

pub type DecisionResult<T> = Result<T, DecisionError>;
