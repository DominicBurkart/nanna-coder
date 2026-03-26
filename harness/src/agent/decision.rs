//! Entity Modification Decision logic for the agent (ARCHITECTURE.md)
//!
//! This module implements the "Entity Modification Decision" node from the
//! Harness Control Flow diagram: given the current entity state, decide
//! whether to **Query Entities (RAG)** for more context or proceed to
//! **Plan Entity Modification**.
//!
//! The decision is driven by the LLM using [`DecisionPrompt`] to build
//! a structured prompt and parse the response for QUERY/PROCEED keywords.

use crate::agent::prompts::DecisionPrompt;
use crate::entities::{EntityQuery, EntityStore};
use thiserror::Error;

/// Errors related to entity modification decisions
#[derive(Error, Debug)]
pub enum DecisionError {
    #[error("Entity modification decision error: {0}")]
    DecisionFailed(String),
}

pub type DecisionResult<T> = Result<T, DecisionError>;

/// Input assembled for the Entity Modification Decision LLM call.
#[derive(Debug)]
pub struct DecisionInput {
    /// The formatted prompt to send to the LLM.
    pub prompt: String,
}

/// Build the decision prompt from the current agent state.
///
/// Queries the entity store for the current entity count and assembles a
/// [`DecisionPrompt`] that the LLM will answer with QUERY or PROCEED.
///
/// # Arguments
/// * `entity_store` - The entity store to query for context
/// * `user_prompt` - The user's original request
/// * `current_plan` - The current execution plan (if any)
/// * `performed_actions` - Number of actions performed so far
pub async fn build_decision_prompt<S: EntityStore>(
    entity_store: &S,
    user_prompt: &str,
    current_plan: Option<&str>,
    performed_actions: usize,
) -> DecisionResult<DecisionInput> {
    let entity_count = entity_store
        .query(&EntityQuery::default())
        .await
        .map_err(|e| DecisionError::DecisionFailed(format!("Failed to query entities: {}", e)))?
        .len();

    let prompt = DecisionPrompt::build(
        user_prompt,
        current_plan.unwrap_or("No plan yet"),
        entity_count,
        performed_actions,
    );

    Ok(DecisionInput { prompt })
}

/// Parse the LLM's decision response.
///
/// Returns `Some(true)` if the LLM says QUERY (need more context),
/// `Some(false)` if the LLM says PROCEED (ready to plan), or
/// `None` if the response is ambiguous.
pub fn parse_decision_response(response: &str) -> Option<bool> {
    DecisionPrompt::parse_response(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::InMemoryEntityStore;

    #[tokio::test]
    async fn test_build_decision_prompt_empty_store() {
        let store = InMemoryEntityStore::new();
        let input = build_decision_prompt(&store, "Create feature", None, 0)
            .await
            .unwrap();
        assert!(input.prompt.contains("Create feature"));
        assert!(input.prompt.contains("QUERY or PROCEED"));
    }

    #[tokio::test]
    async fn test_build_decision_prompt_with_plan() {
        let store = InMemoryEntityStore::new();
        let input =
            build_decision_prompt(&store, "Create feature", Some("Plan: Add module"), 2)
                .await
                .unwrap();
        assert!(input.prompt.contains("Plan: Add module"));
        assert!(input.prompt.contains("2"));
    }

    #[test]
    fn test_parse_decision_query() {
        assert_eq!(parse_decision_response("QUERY - need more info"), Some(true));
    }

    #[test]
    fn test_parse_decision_proceed() {
        assert_eq!(
            parse_decision_response("PROCEED with the plan"),
            Some(false)
        );
    }

    #[test]
    fn test_parse_decision_ambiguous() {
        assert_eq!(parse_decision_response("not sure"), None);
    }
}
