//! LLM prompt templates for agent decision making
//!
//! Provides structured prompts for the key decision points
//! in the ARCHITECTURE.md Harness Control Flow:
//!
//! 1. **Plan Entity Modification**: Analyze user request and create execution plan
//! 2. **Entity Modification Decision**: Decide whether to QUERY entities (RAG) or PROCEED to plan
//! 3. **Task Complete?**: Determine if task is COMPLETE or INCOMPLETE
//!
//! # Design Philosophy
//!
//! - Simple, clear prompts that request specific output formats
//! - Favor keywords over JSON for MVP simplicity
//! - Provide sufficient context without overwhelming the LLM
//! - Format outputs for easy parsing (uppercase keywords)
//!
//! # Security Considerations
//!
//! **Prompt Injection Risk**: User input is interpolated directly into prompts.
//! This is a known limitation of the MVP implementation. For production use:
//! - Consider input sanitization or validation
//! - Monitor agent outputs for unexpected behavior
//! - Implement output validation before acting on LLM decisions
//!
//! See: <https://owasp.org/www-project-top-10-for-large-language-model-applications/>

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
    /// Build a planning prompt.
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

    /// Build planning prompt from QueryResult vector.
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
    /// Build a decision prompt.
    pub fn build(
        user_prompt: &str,
        current_plan: &str,
        entity_count: usize,
        performed_actions: usize,
    ) -> String {
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

    /// Parse decision from LLM response.
    ///
    /// Returns `Some(true)` for QUERY, `Some(false)` for PROCEED, `None` if ambiguous.
    pub fn parse_response(response: &str) -> Option<bool> {
        let upper = response.to_uppercase();
        let has_query = upper.contains("QUERY");
        let has_proceed = upper.contains("PROCEED");

        if has_query && has_proceed {
            None // Ambiguous - both keywords present
        } else if has_query {
            Some(true)
        } else if has_proceed {
            Some(false)
        } else {
            None
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
    /// Build a completion check prompt.
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

    /// Parse completion status from LLM response.
    ///
    /// Returns `Some(true)` for COMPLETE, `Some(false)` for INCOMPLETE, `None` if ambiguous.
    pub fn parse_response(response: &str) -> Option<bool> {
        let upper = response.to_uppercase();

        // Check for standalone "COMPLETE" (not part of "INCOMPLETE")
        let has_complete_only = upper.contains("COMPLETE") && !upper.contains("INCOMPLETE");
        let has_incomplete = upper.contains("INCOMPLETE");

        // If both appear (INCOMPLETE contains COMPLETE), it's ambiguous
        if has_incomplete && upper.matches("COMPLETE").count() > 1 {
            None // Ambiguous - both keywords present separately
        } else if has_complete_only {
            Some(true)
        } else if has_incomplete {
            Some(false)
        } else {
            None
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
        assert!(prompt.contains("Create a new feature"));
    }

    #[test]
    fn test_planning_prompt_contains_entity_count() {
        let prompt = PlanningPrompt::build("Test request", 42, "Results");
        assert!(prompt.contains("42 entities"));
    }

    #[test]
    fn test_planning_prompt_contains_rag_results() {
        let prompt = PlanningPrompt::build("Test", 5, "Found: GitRepository entities");
        assert!(prompt.contains("Found: GitRepository entities"));
    }

    #[test]
    fn test_planning_prompt_structure() {
        let prompt = PlanningPrompt::build("Test", 0, "None");
        assert!(prompt.contains("USER REQUEST:"));
        assert!(prompt.contains("WORKSPACE:"));
        assert!(prompt.contains("RELEVANT:"));
        assert!(prompt.contains("Plan the next action"));
    }

    #[test]
    fn test_planning_prompt_from_empty_results() {
        let results: Vec<QueryResult> = vec![];
        let prompt = PlanningPrompt::build_from_results("Create repo", 5, &results);
        assert!(prompt.contains("No relevant entities found"));
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
        assert!(prompt.contains("2 relevant entities"));
        assert!(prompt.contains("Git"));
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
        // Shows total count but only first 3 types in the summary
        assert!(prompt.contains("4 relevant entities"));
    }

    // ===== DecisionPrompt Tests =====

    #[test]
    fn test_decision_prompt_contains_user_request() {
        let prompt = DecisionPrompt::build("Create feature", "Plan: Add code", 5, 0);
        assert!(prompt.contains("Create feature"));
    }

    #[test]
    fn test_decision_prompt_contains_plan() {
        let prompt = DecisionPrompt::build("Test", "Plan: Execute tests", 5, 0);
        assert!(prompt.contains("Plan: Execute tests"));
    }

    #[test]
    fn test_decision_prompt_structure() {
        let prompt = DecisionPrompt::build("Test", "Plan", 5, 0);
        assert!(prompt.contains("USER REQUEST:"));
        assert!(prompt.contains("CURRENT PLAN:"));
        assert!(prompt.contains("QUERY or PROCEED"));
    }

    #[test]
    fn test_decision_parse_query() {
        assert_eq!(
            DecisionPrompt::parse_response("QUERY - need more context"),
            Some(true)
        );
        assert_eq!(
            DecisionPrompt::parse_response("query for additional entities"),
            Some(true)
        );
    }

    #[test]
    fn test_decision_parse_proceed() {
        assert_eq!(
            DecisionPrompt::parse_response("PROCEED with the action"),
            Some(false)
        );
        assert_eq!(
            DecisionPrompt::parse_response("proceed to next step"),
            Some(false)
        );
    }

    #[test]
    fn test_decision_parse_ambiguous() {
        assert_eq!(DecisionPrompt::parse_response("Not sure what to do"), None);
        assert_eq!(DecisionPrompt::parse_response("QUERY and PROCEED"), None);
    }

    #[test]
    fn test_decision_parse_empty() {
        assert_eq!(DecisionPrompt::parse_response(""), None);
    }

    // ===== CompletionPrompt Tests =====

    #[test]
    fn test_completion_prompt_contains_user_request() {
        let prompt = CompletionPrompt::build("Create feature", 1, &["Git".to_string()]);
        assert!(prompt.contains("Create feature"));
    }

    #[test]
    fn test_completion_prompt_contains_action_count() {
        let prompt = CompletionPrompt::build("Test", 3, &[]);
        assert!(prompt.contains("3"));
    }

    #[test]
    fn test_completion_prompt_contains_entities() {
        let prompt = CompletionPrompt::build("Test", 1, &["Git".to_string(), "Ast".to_string()]);
        assert!(prompt.contains("Git"));
        assert!(prompt.contains("Ast"));
    }

    #[test]
    fn test_completion_prompt_structure() {
        let prompt = CompletionPrompt::build("Test", 0, &[]);
        assert!(prompt.contains("USER REQUEST:"));
        assert!(prompt.contains("ACTIONS PERFORMED:"));
        assert!(prompt.contains("CURRENT ENTITIES:"));
        assert!(prompt.contains("COMPLETE or INCOMPLETE"));
    }

    #[test]
    fn test_completion_parse_complete() {
        assert_eq!(
            CompletionPrompt::parse_response("COMPLETE - task finished"),
            Some(true)
        );
        assert_eq!(
            CompletionPrompt::parse_response("complete, all done"),
            Some(true)
        );
    }

    #[test]
    fn test_completion_parse_incomplete() {
        assert_eq!(
            CompletionPrompt::parse_response("INCOMPLETE - more work needed"),
            Some(false)
        );
        assert_eq!(
            CompletionPrompt::parse_response("incomplete, still working"),
            Some(false)
        );
    }

    #[test]
    fn test_completion_parse_ambiguous() {
        assert_eq!(CompletionPrompt::parse_response("Not sure if done"), None);
    }

    #[test]
    fn test_completion_parse_empty() {
        assert_eq!(CompletionPrompt::parse_response(""), None);
    }

    #[test]
    fn test_completion_parse_both_keywords() {
        assert_eq!(
            CompletionPrompt::parse_response(
                "The task is INCOMPLETE but we're making progress toward COMPLETE"
            ),
            None
        );
        assert_eq!(
            CompletionPrompt::parse_response("COMPLETE INCOMPLETE"),
            None
        );
    }

    // ===== Integration Tests =====

    #[test]
    fn test_all_prompts_are_non_empty() {
        assert!(!PlanningPrompt::build("Test", 0, "None").is_empty());
        assert!(!DecisionPrompt::build("Test", "Plan", 0, 0).is_empty());
        assert!(!CompletionPrompt::build("Test", 0, &[]).is_empty());
    }

    #[test]
    fn test_prompts_handle_empty_inputs() {
        let planning = PlanningPrompt::build("", 0, "");
        let decision = DecisionPrompt::build("", "", 0, 0);
        let completion = CompletionPrompt::build("", 0, &[]);

        assert!(planning.contains("USER REQUEST:"));
        assert!(decision.contains("USER REQUEST:"));
        assert!(completion.contains("USER REQUEST:"));
    }

    #[test]
    fn test_prompts_handle_special_characters() {
        let special = "Test with \"quotes\" and \n newlines";
        let planning = PlanningPrompt::build(special, 0, special);
        let decision = DecisionPrompt::build(special, special, 0, 0);
        let completion = CompletionPrompt::build(special, 0, &[special.to_string()]);

        assert!(planning.contains("quotes"));
        assert!(decision.contains("quotes"));
        assert!(completion.contains("quotes"));
    }
}
