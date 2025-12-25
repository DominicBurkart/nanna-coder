//! LLM prompt templates for agent decision making
//!
//! This module provides structured prompts for the three key decision points
//! in the agent control loop:
//!
//! 1. **Planning**: Analyze user request and create execution plan
//! 2. **Decision**: Decide whether to QUERY (need RAG) or PROCEED (ready to act)
//! 3. **Completion**: Determine if task is COMPLETE or INCOMPLETE
//!
//! # Design Philosophy
//!
//! - Simple, clear prompts that request specific output formats
//! - Favor keywords over JSON for MVP simplicity
//! - Provide sufficient context without overwhelming the LLM
//! - Format outputs for easy parsing (uppercase keywords)

use crate::entities::QueryResult;

/// Planning prompt - Asks LLM to analyze user request and create execution plan
///
/// # Output Format
/// Expected LLM response should be 1-2 sentences describing the next action.
///
/// # Example
/// ```
/// use harness::agent::prompts::PlanningPrompt;
///
/// let prompt = PlanningPrompt::build(
///     "Create a new git repository",
///     5,
///     "Found: GitRepository entities"
/// );
/// assert!(prompt.contains("Create a new git repository"));
/// assert!(prompt.contains("5 entities"));
/// ```
pub struct PlanningPrompt;

impl PlanningPrompt {
    /// Build a planning prompt
    ///
    /// # Arguments
    /// * `user_prompt` - The user's request
    /// * `entity_count` - Number of entities in workspace
    /// * `rag_results` - Summary of RAG query results
    ///
    /// # Returns
    /// Formatted prompt string for LLM
    pub fn build(user_prompt: &str, entity_count: usize, rag_results: &str) -> String {
        format!(
            "You are a code assistant planning an action.\n\
             USER REQUEST: {}\n\
             WORKSPACE: {} entities\n\
             RELEVANT: {}\n\n\
             Plan the next action in 1-2 sentences.",
            user_prompt, entity_count, rag_results
        )
    }

    /// Build planning prompt from QueryResult vector
    ///
    /// # Arguments
    /// * `user_prompt` - The user's request
    /// * `entity_count` - Number of entities in workspace
    /// * `query_results` - Vector of RAG query results
    ///
    /// # Returns
    /// Formatted prompt string for LLM
    pub fn build_from_results(
        user_prompt: &str,
        entity_count: usize,
        query_results: &[QueryResult],
    ) -> String {
        let rag_summary = if query_results.is_empty() {
            "No relevant entities found".to_string()
        } else {
            format!(
                "Found {} relevant entities: {}",
                query_results.len(),
                query_results
                    .iter()
                    .take(3)
                    .map(|r| format!("{:?}", r.entity_type))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        Self::build(user_prompt, entity_count, &rag_summary)
    }
}

/// Decision prompt - Asks LLM to decide "QUERY" or "PROCEED"
///
/// # Output Format
/// Expected LLM response should start with either:
/// - "QUERY" - Need more context from RAG
/// - "PROCEED" - Ready to perform action
///
/// # Example
/// ```
/// use harness::agent::prompts::DecisionPrompt;
///
/// let prompt = DecisionPrompt::build(
///     "Create a new git repository",
///     "Plan: Create GitRepository entity",
///     5,
///     0
/// );
/// assert!(prompt.contains("QUERY or PROCEED"));
/// ```
pub struct DecisionPrompt;

impl DecisionPrompt {
    /// Build a decision prompt
    ///
    /// # Arguments
    /// * `user_prompt` - The user's request
    /// * `current_plan` - The current execution plan
    /// * `entity_count` - Number of entities in workspace
    /// * `performed_actions` - Number of actions performed so far
    ///
    /// # Returns
    /// Formatted prompt string for LLM
    pub fn build(user_prompt: &str, current_plan: &str, entity_count: usize, performed_actions: usize) -> String {
        format!(
            "You are a code assistant deciding the next step.\n\
             USER REQUEST: {}\n\
             CURRENT PLAN: {}\n\
             WORKSPACE: {} entities\n\
             ACTIONS PERFORMED: {}\n\n\
             Do you need more context (QUERY) or are you ready to act (PROCEED)?\n\
             Respond with QUERY or PROCEED followed by brief reasoning.",
            user_prompt, current_plan, entity_count, performed_actions
        )
    }

    /// Parse decision from LLM response
    ///
    /// # Arguments
    /// * `response` - LLM response text
    ///
    /// # Returns
    /// * `Some(true)` - Need to query (QUERY found)
    /// * `Some(false)` - Ready to proceed (PROCEED found)
    /// * `None` - Could not parse response
    pub fn parse_response(response: &str) -> Option<bool> {
        let upper = response.to_uppercase();
        let has_query = upper.contains("QUERY");
        let has_proceed = upper.contains("PROCEED");

        if has_query && has_proceed {
            None // Ambiguous - both keywords present
        } else if has_query {
            Some(true) // Need to query
        } else if has_proceed {
            Some(false) // Ready to proceed
        } else {
            None // No keywords found
        }
    }
}

/// Completion prompt - Asks LLM to determine "COMPLETE" or "INCOMPLETE"
///
/// # Output Format
/// Expected LLM response should start with either:
/// - "COMPLETE" - Task is finished
/// - "INCOMPLETE" - More work needed
///
/// # Example
/// ```
/// use harness::agent::prompts::CompletionPrompt;
///
/// let prompt = CompletionPrompt::build(
///     "Create a new git repository",
///     1,
///     &vec!["Git".to_string()]
/// );
/// assert!(prompt.contains("COMPLETE or INCOMPLETE"));
/// ```
pub struct CompletionPrompt;

impl CompletionPrompt {
    /// Build a completion check prompt
    ///
    /// # Arguments
    /// * `user_prompt` - The user's request
    /// * `actions_performed` - Number of actions taken
    /// * `entity_summary` - Summary of entities in workspace (types/descriptions)
    ///
    /// # Returns
    /// Formatted prompt string for LLM
    pub fn build(user_prompt: &str, actions_performed: usize, entity_summary: &[String]) -> String {
        let entities_text = if entity_summary.is_empty() {
            "No entities created yet".to_string()
        } else {
            entity_summary.join(", ")
        };
        
        format!(
            "You are a code assistant checking task completion.\n\
             USER REQUEST: {}\n\
             ACTIONS PERFORMED: {}\n\
             CURRENT ENTITIES: {}\n\n\
             Is the user's request complete (COMPLETE) or does more work need to be done (INCOMPLETE)?\n\
             Respond with COMPLETE or INCOMPLETE followed by brief reasoning.",
            user_prompt, actions_performed, entities_text
        )
    }

    /// Parse completion status from LLM response
    ///
    /// # Arguments
    /// * `response` - LLM response text
    ///
    /// # Returns
    /// * `Some(true)` - Task is complete (COMPLETE found)
    /// * `Some(false)` - Task is incomplete (INCOMPLETE found)
    /// * `None` - Could not parse response
    pub fn parse_response(response: &str) -> Option<bool> {
        let upper = response.to_uppercase();
        
        // Check for standalone "COMPLETE" (not part of "INCOMPLETE")
        let has_complete_only = upper.contains("COMPLETE") && !upper.contains("INCOMPLETE");
        let has_incomplete = upper.contains("INCOMPLETE");
        
        // If both appear (INCOMPLETE contains COMPLETE), it's ambiguous
        if has_incomplete && upper.matches("COMPLETE").count() > 1 {
            None // Ambiguous - both keywords present separately
        } else if has_complete_only {
            Some(true) // Task complete
        } else if has_incomplete {
            Some(false) // Task incomplete (INCOMPLETE contains COMPLETE)
        } else {
            None // Neither keyword present
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::EntityType;

    // ===== PlanningPrompt Tests =====

    #[test]
    fn test_planning_prompt_contains_user_request() {
        let prompt = PlanningPrompt::build("Create a new feature", 10, "Some results");
        assert!(
            prompt.contains("Create a new feature"),
            "Prompt should contain user request"
        );
    }

    #[test]
    fn test_planning_prompt_contains_entity_count() {
        let prompt = PlanningPrompt::build("Test request", 42, "Results");
        assert!(
            prompt.contains("42 entities"),
            "Prompt should contain entity count"
        );
    }

    #[test]
    fn test_planning_prompt_contains_rag_results() {
        let prompt = PlanningPrompt::build("Test", 5, "Found: GitRepository entities");
        assert!(
            prompt.contains("Found: GitRepository entities"),
            "Prompt should contain RAG results"
        );
    }

    #[test]
    fn test_planning_prompt_structure() {
        let prompt = PlanningPrompt::build("Test", 0, "None");
        assert!(prompt.contains("USER REQUEST:"), "Should have USER REQUEST");
        assert!(prompt.contains("WORKSPACE:"), "Should have WORKSPACE");
        assert!(prompt.contains("RELEVANT:"), "Should have RELEVANT");
        assert!(
            prompt.contains("Plan the next action"),
            "Should request action plan"
        );
    }

    #[test]
    fn test_planning_prompt_from_empty_results() {
        let results: Vec<QueryResult> = vec![];
        let prompt = PlanningPrompt::build_from_results("Create repo", 5, &results);
        assert!(
            prompt.contains("No relevant entities found"),
            "Should indicate no results"
        );
    }

    #[test]
    fn test_planning_prompt_from_query_results() {
        let results = vec![
            QueryResult {
                entity_id: "id1".to_string(),
                entity_type: EntityType::Git,
                relevance: 1.0,
                snippet: None,
            },
            QueryResult {
                entity_id: "id2".to_string(),
                entity_type: EntityType::Ast,
                relevance: 0.8,
                snippet: None,
            },
        ];
        let prompt = PlanningPrompt::build_from_results("Create repo", 5, &results);
        assert!(prompt.contains("2 relevant entities"), "Should show count");
        assert!(prompt.contains("Git"), "Should mention Git entity");
    }

    #[test]
    fn test_planning_prompt_limits_results_display() {
        let results = vec![
            QueryResult {
                entity_id: "id1".to_string(),
                entity_type: EntityType::Git,
                relevance: 1.0,
                snippet: None,
            },
            QueryResult {
                entity_id: "id2".to_string(),
                entity_type: EntityType::Ast,
                relevance: 0.9,
                snippet: None,
            },
            QueryResult {
                entity_id: "id3".to_string(),
                entity_type: EntityType::Test,
                relevance: 0.8,
                snippet: None,
            },
            QueryResult {
                entity_id: "id4".to_string(),
                entity_type: EntityType::Env,
                relevance: 0.7,
                snippet: None,
            },
        ];
        let prompt = PlanningPrompt::build_from_results("Create repo", 5, &results);
        // Should show 4 entities but only display first 3 types
        assert!(
            prompt.contains("4 relevant entities"),
            "Should show total count"
        );
    }

    // ===== DecisionPrompt Tests =====

    #[test]
    fn test_decision_prompt_contains_user_request() {
        let prompt = DecisionPrompt::build("Create feature", "Plan: Add code", 5, 0);
        assert!(
            prompt.contains("Create feature"),
            "Prompt should contain user request"
        );
    }

    #[test]
    fn test_decision_prompt_contains_plan() {
        let prompt = DecisionPrompt::build("Test", "Plan: Execute tests", 5, 0);
        assert!(
            prompt.contains("Plan: Execute tests"),
            "Prompt should contain current plan"
        );
    }

    #[test]
    fn test_decision_prompt_structure() {
        let prompt = DecisionPrompt::build("Test", "Plan", 5, 0);
        assert!(prompt.contains("USER REQUEST:"), "Should have USER REQUEST");
        assert!(prompt.contains("CURRENT PLAN:"), "Should have CURRENT PLAN");
        assert!(
            prompt.contains("QUERY or PROCEED"),
            "Should request QUERY or PROCEED"
        );
    }

    #[test]
    fn test_decision_parse_query() {
        assert_eq!(
            DecisionPrompt::parse_response("QUERY - need more context"),
            Some(true),
            "Should parse QUERY as true"
        );
        assert_eq!(
            DecisionPrompt::parse_response("query for additional entities"),
            Some(true),
            "Should handle lowercase QUERY"
        );
    }

    #[test]
    fn test_decision_parse_proceed() {
        assert_eq!(
            DecisionPrompt::parse_response("PROCEED with the action"),
            Some(false),
            "Should parse PROCEED as false"
        );
        assert_eq!(
            DecisionPrompt::parse_response("proceed to next step"),
            Some(false),
            "Should handle lowercase PROCEED"
        );
    }

    #[test]
    fn test_decision_parse_ambiguous() {
        assert_eq!(
            DecisionPrompt::parse_response("Not sure what to do"),
            None,
            "Should return None for ambiguous response"
        );
        assert_eq!(
            DecisionPrompt::parse_response("QUERY and PROCEED"),
            None,
            "Should return None when both keywords present"
        );
    }

    #[test]
    fn test_decision_parse_empty() {
        assert_eq!(
            DecisionPrompt::parse_response(""),
            None,
            "Should return None for empty response"
        );
    }

    // ===== CompletionPrompt Tests =====

    #[test]
    fn test_completion_prompt_contains_user_request() {
        let prompt = CompletionPrompt::build("Create feature", 1, &vec!["Git".to_string()]);
        assert!(
            prompt.contains("Create feature"),
            "Prompt should contain user request"
        );
    }

    #[test]
    fn test_completion_prompt_contains_action_count() {
        let prompt = CompletionPrompt::build("Test", 3, &vec![]);
        assert!(prompt.contains("3"), "Prompt should contain action count");
    }

    #[test]
    fn test_completion_prompt_contains_entities() {
        let prompt = CompletionPrompt::build("Test", 1, &vec!["Git".to_string(), "Ast".to_string()]);
        assert!(
            prompt.contains("Git"),
            "Prompt should contain entity types"
        );
        assert!(
            prompt.contains("Ast"),
            "Prompt should contain entity types"
        );
    }

    #[test]
    fn test_completion_prompt_structure() {
        let prompt = CompletionPrompt::build("Test", 0, &vec![]);
        assert!(prompt.contains("USER REQUEST:"), "Should have USER REQUEST");
        assert!(
            prompt.contains("ACTIONS PERFORMED:"),
            "Should have ACTIONS PERFORMED"
        );
        assert!(prompt.contains("CURRENT ENTITIES:"), "Should have CURRENT ENTITIES");
        assert!(
            prompt.contains("COMPLETE or INCOMPLETE"),
            "Should request COMPLETE or INCOMPLETE"
        );
    }

    #[test]
    fn test_completion_parse_complete() {
        assert_eq!(
            CompletionPrompt::parse_response("COMPLETE - task finished"),
            Some(true),
            "Should parse COMPLETE as true"
        );
        assert_eq!(
            CompletionPrompt::parse_response("complete, all done"),
            Some(true),
            "Should handle lowercase COMPLETE"
        );
    }

    #[test]
    fn test_completion_parse_incomplete() {
        assert_eq!(
            CompletionPrompt::parse_response("INCOMPLETE - more work needed"),
            Some(false),
            "Should parse INCOMPLETE as false"
        );
        assert_eq!(
            CompletionPrompt::parse_response("incomplete, still working"),
            Some(false),
            "Should handle lowercase INCOMPLETE"
        );
    }

    #[test]
    fn test_completion_parse_ambiguous() {
        assert_eq!(
            CompletionPrompt::parse_response("Not sure if done"),
            None,
            "Should return None for ambiguous response"
        );
    }

    #[test]
    fn test_completion_parse_empty() {
        assert_eq!(
            CompletionPrompt::parse_response(""),
            None,
            "Should return None for empty response"
        );
    }

    #[test]
    fn test_completion_parse_both_keywords() {
        assert_eq!(
            CompletionPrompt::parse_response("The task is INCOMPLETE but we're making progress toward COMPLETE"),
            None,
            "Should return None when both COMPLETE and INCOMPLETE are present"
        );
        assert_eq!(
            CompletionPrompt::parse_response("COMPLETE INCOMPLETE"),
            None,
            "Should return None when both keywords appear"
        );
    }

    // ===== Integration Tests =====

    #[test]
    fn test_all_prompts_are_non_empty() {
        let planning = PlanningPrompt::build("Test", 0, "None");
        let decision = DecisionPrompt::build("Test", "Plan", 0, 0);
        let completion = CompletionPrompt::build("Test", 0, &vec![]);

        assert!(!planning.is_empty(), "Planning prompt should not be empty");
        assert!(!decision.is_empty(), "Decision prompt should not be empty");
        assert!(
            !completion.is_empty(),
            "Completion prompt should not be empty"
        );
    }

    #[test]
    fn test_prompts_handle_empty_inputs() {
        let planning = PlanningPrompt::build("", 0, "");
        let decision = DecisionPrompt::build("", "", 0, 0);
        let completion = CompletionPrompt::build("", 0, &vec![]);

        // Should not panic with empty inputs
        assert!(planning.contains("USER REQUEST:"));
        assert!(decision.contains("USER REQUEST:"));
        assert!(completion.contains("USER REQUEST:"));
    }

    #[test]
    fn test_prompts_handle_special_characters() {
        let special = "Test with \"quotes\" and \n newlines";
        let planning = PlanningPrompt::build(special, 0, special);
        let decision = DecisionPrompt::build(special, special, 0, 0);
        let completion = CompletionPrompt::build(special, 0, &vec![special.to_string()]);

        assert!(planning.contains("quotes"));
        assert!(decision.contains("quotes"));
        assert!(completion.contains("quotes"));
    }
}
