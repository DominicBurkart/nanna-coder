//! Entity Modification Decision logic for the agent (ARCHITECTURE.md)
//!
//! This module implements the "Entity Modification Decision" node from the
//! Harness Control Flow diagram: given the current entity state, decide
//! whether to **Query Entities (RAG)** for more context or proceed to
//! **Plan Entity Modification**.
//!
//! The implementation is currently a stub and needs further problem definition.

use thiserror::Error;

/// Errors related to entity modification decisions
#[derive(Error, Debug)]
pub enum DecisionError {
    #[error("Entity modification decision error: {0}")]
    DecisionFailed(String),
}

pub type DecisionResult<T> = Result<T, DecisionError>;

/// The two branches the decision node can take (see ARCHITECTURE.md control
/// flow diagram).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionAction {
    /// Insufficient context — route to Query Entities (RAG).
    Query,
    /// Enough context — route to Plan Entity Modification.
    Perform,
}

/// Entity Modification Decision (ARCHITECTURE.md)
///
/// Given whether the agent currently has sufficient context, decide whether
/// to query for more entity information or proceed to plan a modification.
///
/// # Note
/// This is a stub implementation that requires further problem definition.
/// The `context_sufficient` parameter will be replaced by a richer entity
/// state type once that is defined.
pub fn entity_modification_decision(
    context_sufficient: bool,
) -> DecisionResult<DecisionAction> {
    let _ = context_sufficient;
    unimplemented!(
        "Entity modification decision logic requires further problem definition. \
         This should analyze context and determine next actions."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(
        expected = "Entity modification decision logic requires further problem definition"
    )]
    fn test_entity_modification_decision_unimplemented() {
        let _ = entity_modification_decision(false);
    }
}
