//! Example demonstrating the LLM prompt templates
//!
//! Run with: cargo run --package harness --example prompts_usage

use harness::agent::prompts::{CompletionPrompt, DecisionPrompt, PlanningPrompt};

fn main() {
    println!("=== LLM Prompt Templates Demo ===\n");

    // 1. Planning Prompt
    println!("1. PLANNING PROMPT:");
    println!("{}", "=".repeat(60));
    let planning = PlanningPrompt::build(
        "Create a new git repository with README",
        5,
        "Found: 2 GitRepository entities, 1 Context entity",
    );
    println!("{}\n", planning);

    // 2. Decision Prompt
    println!("2. DECISION PROMPT:");
    println!("{}", "=".repeat(60));
    let decision = DecisionPrompt::build(
        "Create a new git repository with README",
        "Create GitRepository entity and Context entity for README",
        5,
        0,
    );
    println!("{}\n", decision);

    // Test parsing decision responses
    println!("Decision parsing examples:");
    let responses = vec![
        "QUERY - need to check existing repositories",
        "PROCEED - ready to create entities",
        "I think we should QUERY and PROCEED",
    ];
    for resp in responses {
        let parsed = DecisionPrompt::parse_response(resp);
        println!("  '{}' -> {:?}", resp, parsed);
    }
    println!();

    // 3. Completion Prompt
    println!("3. COMPLETION PROMPT:");
    println!("{}", "=".repeat(60));
    let completion = CompletionPrompt::build(
        "Create a new git repository with README",
        2,
        &["GitRepository".to_string(), "Context".to_string()],
    );
    println!("{}\n", completion);

    // Test parsing completion responses
    println!("Completion parsing examples:");
    let responses = vec![
        "COMPLETE - created repository and README",
        "INCOMPLETE - still need to add README content",
        "Maybe done?",
    ];
    for resp in responses {
        let parsed = CompletionPrompt::parse_response(resp);
        println!("  '{}' -> {:?}", resp, parsed);
    }
}
