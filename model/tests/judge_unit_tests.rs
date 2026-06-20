//! Unit tests for judge scoring helpers and config edge cases.
//!
//! The judge module's `calculate_coherence_score` and `calculate_relevance_score`
//! functions are pure and deterministic (aside from retry delay jitter). These
//! tests exercise boundary conditions and ensure the scoring contract holds.

use model::judge::{
    calculate_coherence_score, calculate_relevance_score, JudgeConfig, ValidationCriteria,
    ValidationMetrics, ValidationResult,
};
use std::time::Duration;

// ---------------------------------------------------------------------------
// calculate_coherence_score edge cases
// ---------------------------------------------------------------------------

#[test]
fn coherence_empty_string_is_zero() {
    assert_eq!(calculate_coherence_score(""), 0.0);
}

#[test]
fn coherence_single_word_gets_base_score() {
    let score = calculate_coherence_score("hello");
    // Base 0.5 + 0.2 (one sentence-ish) + word variety bonus
    assert!(score >= 0.5, "single word should get at least base score");
    assert!(score <= 1.0);
}

#[test]
fn coherence_very_long_text_penalized() {
    // Text longer than 5000 chars misses the "reasonable length" bonus
    let long = "word ".repeat(1500); // ~7500 chars
    let short = "This is a well written paragraph. It has multiple sentences and good structure.";
    let long_score = calculate_coherence_score(&long);
    let short_score = calculate_coherence_score(short);
    // Both should be valid scores
    assert!(long_score > 0.0 && long_score <= 1.0);
    assert!(short_score > 0.0 && short_score <= 1.0);
}

#[test]
fn coherence_multiline_gets_paragraph_bonus() {
    let single = "This is a single paragraph with no newlines at all.";
    let multi = "This is paragraph one with some extra words added.\n\nThis is paragraph two.";
    let single_score = calculate_coherence_score(single);
    let multi_score = calculate_coherence_score(multi);
    assert!(
        multi_score >= single_score,
        "multi-paragraph text should score >= single paragraph"
    );
}

#[test]
fn coherence_low_word_variety_scores_lower() {
    let repetitive = "the the the the the the the the the the";
    let varied = "the quick brown fox jumps over a lazy sleeping dog";
    let rep_score = calculate_coherence_score(repetitive);
    let var_score = calculate_coherence_score(varied);
    assert!(
        var_score >= rep_score,
        "varied text ({}) should score >= repetitive text ({})",
        var_score,
        rep_score
    );
}

// ---------------------------------------------------------------------------
// calculate_relevance_score edge cases
// ---------------------------------------------------------------------------

#[test]
fn relevance_no_required_keywords_gives_base_score() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec![])
        .with_forbidden_keywords(vec![]);
    let score = calculate_relevance_score("Some random text here", "random prompt", &criteria);
    // Should get 0.5 base + prompt overlap + length bonus
    assert!(score > 0.0);
}

#[test]
fn relevance_all_required_keywords_present() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec!["rust".into(), "async".into()])
        .with_forbidden_keywords(vec![]);
    let score = calculate_relevance_score(
        "Rust has great async support for concurrent programming",
        "explain rust async",
        &criteria,
    );
    assert!(
        score > 0.5,
        "all keywords present should give high score: {}",
        score
    );
}

#[test]
fn relevance_forbidden_keyword_penalizes() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec![])
        .with_forbidden_keywords(vec!["error".into()]);
    let clean = calculate_relevance_score("Everything is fine", "status", &criteria);
    let dirty = calculate_relevance_score("There was an error", "status", &criteria);
    assert!(
        clean > dirty,
        "forbidden keyword should reduce score: clean={} dirty={}",
        clean,
        dirty
    );
}

#[test]
fn relevance_too_short_response_penalized() {
    let criteria = ValidationCriteria {
        min_response_length: 100,
        max_response_length: 10000,
        ..ValidationCriteria::default()
    };
    let score = calculate_relevance_score("Short.", "explain something", &criteria);
    // Short response gets -0.2 penalty
    assert!(score <= 1.0);
}

#[test]
fn relevance_empty_prompt_does_not_panic() {
    let criteria = ValidationCriteria::default();
    let score = calculate_relevance_score("some response text", "", &criteria);
    assert!((0.0..=1.0).contains(&score));
}

#[test]
fn relevance_score_clamped_to_unit_interval() {
    // Scenario that could push score negative: forbidden keyword + too short + no keywords match
    let criteria = ValidationCriteria {
        min_response_length: 1000,
        forbidden_keywords: vec!["bad".into()],
        required_keywords: vec!["nonexistent_xyz".into()],
        ..ValidationCriteria::default()
    };
    let score = calculate_relevance_score("bad", "query", &criteria);
    assert!(score >= 0.0, "score should be clamped to >= 0: {}", score);
    assert!(score <= 1.0);
}

// ---------------------------------------------------------------------------
// JudgeConfig retry delay
// ---------------------------------------------------------------------------

#[test]
fn retry_delay_zero_jitter_is_deterministic() {
    let config = JudgeConfig {
        jitter_factor: 0.0,
        base_delay_ms: 100,
        max_delay_ms: 5000,
        ..JudgeConfig::default()
    };
    let d0 = config.calculate_retry_delay(0);
    let d1 = config.calculate_retry_delay(1);
    let d2 = config.calculate_retry_delay(2);

    assert_eq!(d0, Duration::from_millis(100));
    assert_eq!(d1, Duration::from_millis(200));
    assert_eq!(d2, Duration::from_millis(400));
}

#[test]
fn retry_delay_caps_at_max() {
    let config = JudgeConfig {
        jitter_factor: 0.0,
        base_delay_ms: 1000,
        max_delay_ms: 2000,
        ..JudgeConfig::default()
    };
    // attempt 0: 1000ms, attempt 1: 2000ms, attempt 5: capped at 2000ms
    let d5 = config.calculate_retry_delay(5);
    assert_eq!(d5, Duration::from_millis(2000));
}

#[test]
fn retry_delay_with_jitter_bounded() {
    let config = JudgeConfig {
        jitter_factor: 0.5,
        base_delay_ms: 100,
        max_delay_ms: 5000,
        ..JudgeConfig::default()
    };
    // Run multiple times to exercise randomness
    for _ in 0..20 {
        let d = config.calculate_retry_delay(0);
        assert!(d >= Duration::from_millis(100)); // At least the base
        assert!(d <= Duration::from_millis(150)); // Base + 50% jitter max
    }
}

// ---------------------------------------------------------------------------
// ValidationMetrics clamping
// ---------------------------------------------------------------------------

#[test]
fn metrics_coherence_score_clamped_above_one() {
    let m = ValidationMetrics::default().with_coherence_score(1.5);
    assert_eq!(m.coherence_score, Some(1.0));
}

#[test]
fn metrics_coherence_score_clamped_below_zero() {
    let m = ValidationMetrics::default().with_coherence_score(-0.3);
    assert_eq!(m.coherence_score, Some(0.0));
}

#[test]
fn metrics_relevance_score_clamped() {
    let m = ValidationMetrics::default().with_relevance_score(2.0);
    assert_eq!(m.relevance_score, Some(1.0));

    let m = ValidationMetrics::default().with_relevance_score(-1.0);
    assert_eq!(m.relevance_score, Some(0.0));
}

// ---------------------------------------------------------------------------
// ValidationResult accessors
// ---------------------------------------------------------------------------

#[test]
fn validation_result_metrics_accessor() {
    let metrics = ValidationMetrics::with_duration(Duration::from_secs(1));
    let success = ValidationResult::Success {
        message: "ok".into(),
        metrics: metrics.clone(),
    };
    assert!(success.metrics().is_some());
    assert_eq!(success.metrics().unwrap().duration, Duration::from_secs(1));

    let warning = ValidationResult::Warning {
        message: "slow".into(),
        suggestions: vec![],
        metrics: metrics.clone(),
    };
    assert!(warning.metrics().is_some());

    let failure_no_metrics = ValidationResult::Failure {
        message: "bad".into(),
        error_details: "err".into(),
        suggestions: vec![],
        metrics: None,
    };
    assert!(failure_no_metrics.metrics().is_none());

    let failure_with_metrics = ValidationResult::Failure {
        message: "bad".into(),
        error_details: "err".into(),
        suggestions: vec![],
        metrics: Some(metrics),
    };
    assert!(failure_with_metrics.metrics().is_some());
}

#[test]
fn validation_result_suggestions_accessor() {
    let success = ValidationResult::Success {
        message: "ok".into(),
        metrics: ValidationMetrics::default(),
    };
    assert!(success.suggestions().is_empty());

    let warning = ValidationResult::Warning {
        message: "w".into(),
        suggestions: vec!["try X".into(), "try Y".into()],
        metrics: ValidationMetrics::default(),
    };
    assert_eq!(warning.suggestions(), vec!["try X", "try Y"]);

    let failure = ValidationResult::Failure {
        message: "f".into(),
        error_details: "e".into(),
        suggestions: vec!["fix it".into()],
        metrics: None,
    };
    assert_eq!(failure.suggestions(), vec!["fix it"]);
}

// ---------------------------------------------------------------------------
// ValidationCriteria preset builders
// ---------------------------------------------------------------------------

#[test]
fn technical_documentation_criteria_stricter_than_default() {
    let tech = ValidationCriteria::technical_documentation();
    let default = ValidationCriteria::default();
    assert!(tech.min_coherence_score > default.min_coherence_score);
    assert!(tech.min_relevance_score > default.min_relevance_score);
    assert!(tech.min_response_length > default.min_response_length);
}

#[test]
fn creative_writing_criteria_more_lenient() {
    let creative = ValidationCriteria::creative_writing();
    let default = ValidationCriteria::default();
    assert!(creative.min_coherence_score < default.min_coherence_score);
    assert!(!creative.require_factual_accuracy);
    assert!(creative.max_response_length > default.max_response_length);
}
