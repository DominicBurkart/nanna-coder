//! Unit tests for `model::types` constructors and `model::judge` pure helpers.
//!
//! These tests exercise code paths that are reachable without a live model
//! server and were previously absent from the test suite.

use model::judge::{
    calculate_coherence_score, calculate_relevance_score, JudgeConfig, ValidationCriteria,
    ValidationMetrics, ValidationResult,
};
use model::types::{
    ChatMessage, ChatRequest, FunctionCall, FunctionDefinition, JsonSchema, MessageRole,
    SchemaType, ToolCall, ToolChoice, ToolDefinition,
};
use std::time::Duration;

// ---------------------------------------------------------------------------
// ChatMessage constructors
// ---------------------------------------------------------------------------

#[test]
fn assistant_with_tools_sets_role_and_tool_calls() {
    let tc = ToolCall {
        id: "call_abc".to_string(),
        function: FunctionCall {
            name: "read_file".to_string(),
            arguments: serde_json::json!({"path": "src/main.rs"}),
        },
    };
    let msg = ChatMessage::assistant_with_tools(Some("thinking…".to_string()), vec![tc.clone()]);

    assert_eq!(msg.role, MessageRole::Assistant);
    assert_eq!(msg.content, Some("thinking…".to_string()));
    let calls = msg.tool_calls.expect("tool_calls must be Some");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call_abc");
    assert_eq!(calls[0].function.name, "read_file");
    assert!(msg.tool_call_id.is_none());
}

#[test]
fn assistant_with_tools_accepts_none_content() {
    let tc = ToolCall {
        id: "c".to_string(),
        function: FunctionCall {
            name: "echo".to_string(),
            arguments: serde_json::json!({}),
        },
    };
    let msg = ChatMessage::assistant_with_tools(None, vec![tc]);
    assert_eq!(msg.role, MessageRole::Assistant);
    assert!(msg.content.is_none());
    assert!(msg.tool_calls.is_some());
}

#[test]
fn tool_response_sets_role_and_call_id() {
    let msg = ChatMessage::tool_response("call_999", "result text");
    assert_eq!(msg.role, MessageRole::Tool);
    assert_eq!(msg.tool_call_id, Some("call_999".to_string()));
    assert_eq!(msg.content, Some("result text".to_string()));
    assert!(msg.tool_calls.is_none());
}

// ---------------------------------------------------------------------------
// ToolChoice — default and serialization
// ---------------------------------------------------------------------------

#[test]
fn tool_choice_default_is_auto() {
    let choice: ToolChoice = Default::default();
    assert_eq!(choice, ToolChoice::Auto);
}

#[test]
fn tool_choice_all_variants_round_trip_through_json() {
    let variants = vec![
        ToolChoice::Auto,
        ToolChoice::None,
        ToolChoice::Required,
        ToolChoice::Specific("my_tool".to_string()),
    ];
    for variant in &variants {
        let json = serde_json::to_string(variant).expect("serialize");
        let back: ToolChoice = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, variant, "round-trip failed for {:?}", variant);
    }
}

// ---------------------------------------------------------------------------
// ChatRequest builder — with_tools sets tool_choice
// ---------------------------------------------------------------------------

#[test]
fn chat_request_with_tools_sets_tool_choice_to_auto() {
    let tool = ToolDefinition {
        function: FunctionDefinition {
            name: "calculate".to_string(),
            description: "A calculator".to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: None,
                required: None,
            },
        },
    };
    let req = ChatRequest::new("model", vec![ChatMessage::user("hi")]).with_tools(vec![tool]);

    assert_eq!(req.tool_choice, Some(ToolChoice::Auto));
    assert!(req.tools.is_some());
    assert_eq!(req.tools.unwrap().len(), 1);
}

#[test]
fn chat_request_without_tools_has_no_tool_choice() {
    let req = ChatRequest::new("model", vec![ChatMessage::user("hi")]);
    assert!(req.tool_choice.is_none());
    assert!(req.tools.is_none());
}

// ---------------------------------------------------------------------------
// JudgeConfig — retry delay invariants
// ---------------------------------------------------------------------------

#[test]
fn retry_delay_with_zero_jitter_is_deterministic_and_respects_cap() {
    let config = JudgeConfig {
        jitter_factor: 0.0,
        base_delay_ms: 100,
        max_delay_ms: 500,
        ..Default::default()
    };

    // attempt 0 → 100 ms, attempt 1 → 200 ms, attempt 2 → 400 ms, attempt 3 → capped at 500 ms
    assert_eq!(config.calculate_retry_delay(0), Duration::from_millis(100));
    assert_eq!(config.calculate_retry_delay(1), Duration::from_millis(200));
    assert_eq!(config.calculate_retry_delay(2), Duration::from_millis(400));
    // Cap kicks in — 100 * 2^3 = 800, capped to 500.
    assert_eq!(config.calculate_retry_delay(3), Duration::from_millis(500));
}

#[test]
fn retry_delay_is_non_decreasing_across_attempts() {
    // With jitter enabled delays are not strictly deterministic, but they must
    // be non-decreasing on average.  We call the function twice per attempt
    // and require that the upper bound for attempt N ≤ lower bound of attempt N+1
    // is satisfied when jitter is zero.
    let config = JudgeConfig {
        jitter_factor: 0.0,
        base_delay_ms: 50,
        max_delay_ms: 2000,
        ..Default::default()
    };

    let delays: Vec<Duration> = (0..6).map(|i| config.calculate_retry_delay(i)).collect();
    for window in delays.windows(2) {
        assert!(
            window[0] <= window[1],
            "delays must be non-decreasing: {:?}",
            delays
        );
    }
}

// ---------------------------------------------------------------------------
// ValidationResult — accessor methods
// ---------------------------------------------------------------------------

#[test]
fn validation_result_metrics_accessor_returns_correct_ref() {
    let m = ValidationMetrics::with_duration(Duration::from_millis(42));

    let success = ValidationResult::Success {
        message: "ok".to_string(),
        metrics: m.clone(),
    };
    assert_eq!(success.metrics().unwrap().duration, Duration::from_millis(42));

    let warning = ValidationResult::Warning {
        message: "slow".to_string(),
        suggestions: vec![],
        metrics: m.clone(),
    };
    assert_eq!(warning.metrics().unwrap().duration, Duration::from_millis(42));

    let failure_with = ValidationResult::Failure {
        message: "bad".to_string(),
        error_details: "oops".to_string(),
        suggestions: vec![],
        metrics: Some(m.clone()),
    };
    assert_eq!(
        failure_with.metrics().unwrap().duration,
        Duration::from_millis(42)
    );

    let failure_none: ValidationResult = ValidationResult::Failure {
        message: "bad".to_string(),
        error_details: "oops".to_string(),
        suggestions: vec![],
        metrics: None,
    };
    assert!(failure_none.metrics().is_none());
}

#[test]
fn validation_result_suggestions_accessor() {
    let m = ValidationMetrics::default();

    let success = ValidationResult::Success {
        message: "ok".to_string(),
        metrics: m.clone(),
    };
    assert!(success.suggestions().is_empty());

    let warning = ValidationResult::Warning {
        message: "slow".to_string(),
        suggestions: vec!["try X".to_string(), "try Y".to_string()],
        metrics: m.clone(),
    };
    assert_eq!(warning.suggestions(), vec!["try X", "try Y"]);

    let failure = ValidationResult::Failure {
        message: "bad".to_string(),
        error_details: "details".to_string(),
        suggestions: vec!["fix Z".to_string()],
        metrics: None,
    };
    assert_eq!(failure.suggestions(), vec!["fix Z"]);
}

// ---------------------------------------------------------------------------
// calculate_coherence_score — invariants
// ---------------------------------------------------------------------------

#[test]
fn coherence_score_empty_string_is_zero() {
    assert_eq!(calculate_coherence_score(""), 0.0);
}

#[test]
fn coherence_score_is_always_in_unit_interval() {
    let samples = [
        "",
        "a",
        "Hello world.",
        "Short text with some sentences. Another one here.",
        &"word ".repeat(1000),
    ];
    for s in &samples {
        let score = calculate_coherence_score(s);
        assert!(
            (0.0..=1.0).contains(&score),
            "coherence score {} out of [0,1] for input (len={})",
            score,
            s.len()
        );
    }
}

#[test]
fn coherence_score_structured_text_exceeds_single_word() {
    let single = calculate_coherence_score("hello");
    let structured = calculate_coherence_score(
        "This is a well-formed sentence. Here is another one.\n\nAnd a second paragraph.",
    );
    assert!(
        structured > single,
        "structured text ({}) should score higher than a single word ({})",
        structured,
        single
    );
}

// ---------------------------------------------------------------------------
// calculate_relevance_score — invariants
// ---------------------------------------------------------------------------

#[test]
fn relevance_score_is_always_in_unit_interval() {
    let criteria = ValidationCriteria::default();
    let prompt = "explain machine learning";
    let samples = [
        "",
        "Machine learning is great.",
        "I cannot help with that.",
        &"x ".repeat(500),
    ];
    for s in &samples {
        let score = calculate_relevance_score(s, prompt, &criteria);
        assert!(
            (0.0..=1.0).contains(&score),
            "relevance score {} out of [0,1]",
            score
        );
    }
}

#[test]
fn relevance_score_penalises_forbidden_keywords() {
    let criteria = ValidationCriteria::default()
        // default forbidden: "I cannot", "I don't know", "unable to"
    ;
    let prompt = "explain something";
    let good = calculate_relevance_score("Here is a detailed explanation.", prompt, &criteria);
    let bad = calculate_relevance_score("I cannot provide that information.", prompt, &criteria);
    assert!(
        good > bad,
        "forbidden-keyword response ({}) should score below clean response ({})",
        bad,
        good
    );
}

#[test]
fn relevance_score_rewards_required_keywords() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec!["rust".to_string(), "memory".to_string()]);
    let prompt = "discuss rust memory safety";
    let with_kw = calculate_relevance_score("Rust ensures memory safety without GC.", prompt, &criteria);
    let without_kw = calculate_relevance_score("Python is also a programming language.", prompt, &criteria);
    assert!(
        with_kw > without_kw,
        "response with required keywords ({}) should outscore one without ({})",
        with_kw,
        without_kw
    );
}

#[test]
fn relevance_score_penalises_response_shorter_than_minimum() {
    let criteria = ValidationCriteria {
        min_response_length: 200,
        ..Default::default()
    };
    let prompt = "write something long";
    let short = calculate_relevance_score("ok", prompt, &criteria);
    let long = calculate_relevance_score(&"word ".repeat(50), prompt, &criteria);
    assert!(
        long > short,
        "response meeting min length ({}) should score higher than short response ({})",
        long,
        short
    );
}
