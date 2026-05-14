//! Integration-level unit tests for `model::judge`.
//!
//! All tests here are pure (no network / no Ollama) and use an in-process
//! `MockModelJudge` that implements `ModelJudge` by returning canned results.
//! The goal is to exercise every observable behaviour of the types and the two
//! public pure functions (`calculate_coherence_score`,
//! `calculate_relevance_score`) without touching any LLM.
use async_trait::async_trait;
use model::judge::{
    calculate_coherence_score, calculate_relevance_score, JudgeConfig, ModelJudge,
    ValidationCriteria, ValidationMetrics, ValidationResult,
};
use model::provider::{ModelError, ModelProvider, ModelResult};
use model::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, ModelInfo, ToolDefinition,
};
use std::collections::HashMap;
use std::time::Duration;

// ─────────────────────────────────────────────────────────────────────────────────
// Minimal mock that satisfies both ModelProvider and ModelJudge
// ─────────────────────────────────────────────────────────────────────────────────

struct MockModelJudge {
    config: JudgeConfig,
    /// Controls what validate_api_responsiveness returns.
    responsiveness_result: Option<ValidationResult>,
    /// Controls what validate_response_quality returns.
    quality_result: Option<ValidationResult>,
    /// Controls what validate_tool_calling returns.
    tool_result: Option<ValidationResult>,
    /// Controls what validate_consistency returns.
    consistency_result: Option<ValidationResult>,
}

impl MockModelJudge {
    fn all_success() -> Self {
        let ok = |msg: &str| ValidationResult::Success {
            message: msg.to_string(),
            metrics: ValidationMetrics::with_duration(Duration::from_millis(10)),
        };
        Self {
            config: JudgeConfig::default(),
            responsiveness_result: Some(ok("api ok")),
            quality_result: Some(ok("quality ok")),
            tool_result: Some(ok("tools ok")),
            consistency_result: Some(ok("consistency ok")),
        }
    }

    fn with_responsiveness(mut self, r: ValidationResult) -> Self {
        self.responsiveness_result = Some(r);
        self
    }

    fn with_quality(mut self, r: ValidationResult) -> Self {
        self.quality_result = Some(r);
        self
    }

    fn with_tools(mut self, r: ValidationResult) -> Self {
        self.tool_result = Some(r);
        self
    }

    fn with_consistency(mut self, r: ValidationResult) -> Self {
        self.consistency_result = Some(r);
        self
    }
}

#[async_trait]
impl ModelProvider for MockModelJudge {
    async fn chat(&self, _req: ChatRequest) -> ModelResult<ChatResponse> {
        Ok(ChatResponse {
            choices: vec![Choice {
                message: ChatMessage::assistant("mock"),
                finish_reason: Some(FinishReason::Stop),
            }],
            usage: None,
        })
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> ModelResult<()> {
        Ok(())
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }
}

#[async_trait]
impl ModelJudge for MockModelJudge {
    fn judge_config(&self) -> &JudgeConfig {
        &self.config
    }

    async fn validate_api_responsiveness(
        &self,
        _threshold: Duration,
    ) -> ModelResult<ValidationResult> {
        self.responsiveness_result
            .clone()
            .ok_or_else(|| ModelError::Unknown {
                message: "no responsiveness result configured".into(),
            })
    }

    async fn validate_response_quality(
        &self,
        _prompt: &str,
        _criteria: &ValidationCriteria,
    ) -> ModelResult<ValidationResult> {
        self.quality_result
            .clone()
            .ok_or_else(|| ModelError::Unknown {
                message: "no quality result configured".into(),
            })
    }

    async fn validate_tool_calling(
        &self,
        _tools: &[ToolDefinition],
    ) -> ModelResult<ValidationResult> {
        self.tool_result.clone().ok_or_else(|| ModelError::Unknown {
            message: "no tool result configured".into(),
        })
    }

    async fn validate_consistency(
        &self,
        _prompts: &[&str],
        _iterations: usize,
    ) -> ModelResult<ValidationResult> {
        self.consistency_result
            .clone()
            .ok_or_else(|| ModelError::Unknown {
                message: "no consistency result configured".into(),
            })
    }
}

// ─────────────────────────────────────────────────────────────────────────────────
// calculate_coherence_score
// ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn coherence_empty_string_is_zero() {
    assert_eq!(calculate_coherence_score(""), 0.0);
}

#[test]
fn coherence_single_word_is_nonzero() {
    // Even one word should produce something > 0 because there is content.
    let score = calculate_coherence_score("hello");
    assert!(score > 0.0, "single word should score > 0, got {}", score);
    assert!(score <= 1.0, "score must be <= 1.0, got {}", score);
}

#[test]
fn coherence_multi_sentence_higher_than_single_word() {
    let single = calculate_coherence_score("hello");
    let multi = calculate_coherence_score(
        "This is the first sentence. This is the second sentence. This is the third.",
    );
    assert!(
        multi > single,
        "multi-sentence text ({}) should score higher than single word ({})",
        multi,
        single
    );
}

#[test]
fn coherence_multi_paragraph_higher_than_single_paragraph() {
    let single_para = "The quick brown fox jumps over the lazy dog. It is a well-known sentence.";
    let multi_para = "The quick brown fox jumps over the lazy dog. It is a well-known sentence.\n\nAnother paragraph with different words adds structure and variety to the text.";
    let s1 = calculate_coherence_score(single_para);
    let s2 = calculate_coherence_score(multi_para);
    assert!(
        s2 >= s1,
        "multi-paragraph ({}) should score >= single paragraph ({})",
        s2,
        s1
    );
}

#[test]
fn coherence_score_is_clamped_between_zero_and_one() {
    // Craft a text that might push the arithmetic beyond [0,1] without clamping.
    let long_repeated = "word ".repeat(2000);
    let score = calculate_coherence_score(&long_repeated);
    assert!(score >= 0.0, "score must be >= 0.0, got {}", score);
    assert!(score <= 1.0, "score must be <= 1.0, got {}", score);
}

#[test]
fn coherence_high_word_variety_boosts_score() {
    // All same word – low variety.
    let low_variety = "dog dog dog dog dog dog dog dog dog dog dog dog dog dog dog";
    // All different words – high variety.
    let high_variety = "apple orange banana grape mango cherry blueberry raspberry strawberry kiwi";
    let low_score = calculate_coherence_score(low_variety);
    let high_score = calculate_coherence_score(high_variety);
    assert!(
        high_score > low_score,
        "high-variety text ({}) should score higher than low-variety ({})",
        high_score,
        low_score
    );
}

#[test]
fn coherence_very_long_text_does_not_get_length_bonus() {
    // The length bonus (+0.10) only applies to text in the range (50, 5000) chars.
    // Build two texts with identical word-variety (all the same word) so that
    // only the length signal differs.  The long text (> 5000 chars) must not
    // score more than 0.05 above the short text (< 5000 chars) — a tolerance
    // narrower than the 0.10 bonus itself, which would fire if the cap is removed.
    let short = "word ".repeat(900); // ~4500 chars — inside the bonus range
    let long = "word ".repeat(1100); // ~5500 chars — outside the bonus range
    let short_score = calculate_coherence_score(&short);
    let long_score = calculate_coherence_score(&long);
    assert!(
        long_score <= short_score + 0.05,
        "long text score ({}) must not exceed short text score ({}) by more than 0.05 \
         (tolerance is intentionally narrower than the 0.10 length bonus)",
        long_score,
        short_score
    );
}

// ─────────────────────────────────────────────────────────────────────────────────
// calculate_relevance_score
// ─────────────────────────────────────────────────────────────────────────────────

fn default_criteria_no_keywords() -> ValidationCriteria {
    ValidationCriteria {
        required_keywords: vec![],
        forbidden_keywords: vec![],
        min_response_length: 10,
        max_response_length: 10000,
        ..ValidationCriteria::default()
    }
}

#[test]
fn relevance_score_is_clamped_between_zero_and_one() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec!["rust".to_string()])
        .with_forbidden_keywords(vec!["error".to_string(), "fail".to_string()]);

    // Deliberately include every forbidden word and omit all required ones.
    let worst = "error fail error fail error fail";
    let score = calculate_relevance_score(worst, "rust programming language", &criteria);
    assert!(score >= 0.0, "score must be >= 0.0, got {}", score);
    assert!(score <= 1.0, "score must be <= 1.0, got {}", score);
}

#[test]
fn relevance_all_required_keywords_present_scores_higher_than_none_present() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec![
            "machine".to_string(),
            "learning".to_string(),
            "neural".to_string(),
        ])
        .with_forbidden_keywords(vec![]);

    let prompt = "Describe machine learning";

    let good = "Machine learning and neural networks are at the core of modern AI.";
    let bad = "Artificial intelligence is a broad field of computer science.";

    let good_score = calculate_relevance_score(good, prompt, &criteria);
    let bad_score = calculate_relevance_score(bad, prompt, &criteria);

    assert!(
        good_score > bad_score,
        "response with all required keywords ({}) should score higher than one without ({})",
        good_score,
        bad_score
    );
}

#[test]
fn relevance_partial_keyword_match_between_all_and_none() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec![
            "apple".to_string(),
            "banana".to_string(),
            "cherry".to_string(),
            "date".to_string(),
        ])
        .with_forbidden_keywords(vec![]);

    let prompt = "list some fruits";

    let all_present = "I like apple, banana, cherry, and date fruits.";
    let two_present = "I like apple and banana.";
    let none_present = "I enjoy many different foods.";

    let all_score = calculate_relevance_score(all_present, prompt, &criteria);
    let two_score = calculate_relevance_score(two_present, prompt, &criteria);
    let none_score = calculate_relevance_score(none_present, prompt, &criteria);

    assert!(
        all_score > two_score,
        "all keywords ({}) should score higher than two keywords ({})",
        all_score,
        two_score
    );
    assert!(
        two_score > none_score,
        "two keywords ({}) should score higher than no keywords ({})",
        two_score,
        none_score
    );
}

#[test]
fn relevance_forbidden_keyword_applies_penalty() {
    let criteria = ValidationCriteria::default()
        .with_required_keywords(vec![])
        .with_forbidden_keywords(vec!["error".to_string()]);

    let prompt = "say something";

    let clean = "Everything is working perfectly fine.";
    let dirty = "Everything is working perfectly fine but an error occurred.";

    let clean_score = calculate_relevance_score(clean, prompt, &criteria);
    let dirty_score = calculate_relevance_score(dirty, prompt, &criteria);

    assert!(
        clean_score > dirty_score,
        "response without forbidden word ({}) should score higher than one with it ({})",
        clean_score,
        dirty_score
    );
}

#[test]
fn relevance_response_too_short_is_penalised() {
    let criteria = ValidationCriteria {
        min_response_length: 100,
        max_response_length: 10000,
        required_keywords: vec![],
        forbidden_keywords: vec![],
        ..ValidationCriteria::default()
    };

    let prompt = "explain something";
    let short = "Yes."; // well under 100 chars
    let adequate = "a".repeat(110); // meets minimum

    let short_score = calculate_relevance_score(short, prompt, &criteria);
    let adequate_score = calculate_relevance_score(&adequate, prompt, &criteria);

    assert!(
        adequate_score > short_score,
        "adequate-length response ({}) should score higher than too-short response ({})",
        adequate_score,
        short_score
    );
}

#[test]
fn relevance_no_required_keywords_gives_base_score() {
    // When required_keywords is empty, the function grants a 0.5 base score
    // for that component – a non-empty response at the right length and without
    // forbidden words should receive a non-trivial positive score.
    let criteria = default_criteria_no_keywords();
    let prompt = "hello";
    let response = "hello world, nice to meet you today";

    let score = calculate_relevance_score(response, prompt, &criteria);
    assert!(
        score > 0.3,
        "no-keyword criteria should still yield a decent score, got {}",
        score
    );
}

#[test]
fn relevance_prompt_term_overlap_boosts_score() {
    let criteria = default_criteria_no_keywords();

    // Response that echoes every word in the prompt vs one that doesn't.
    let prompt = "explain gravity clearly simply";
    let echoes_prompt = "gravity is explained clearly and simply here";
    let ignores_prompt = "the sky is blue and the grass is green";

    let echo_score = calculate_relevance_score(echoes_prompt, prompt, &criteria);
    let ignore_score = calculate_relevance_score(ignores_prompt, prompt, &criteria);

    assert!(
        echo_score > ignore_score,
        "response echoing prompt ({}) should score higher than one that doesn't ({})",
        echo_score,
        ignore_score
    );
}

// ─────────────────────────────────────────────────────────────────────────────────
// ValidationCriteria
// ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn criteria_default_has_sensible_values() {
    let c = ValidationCriteria::default();
    assert!(c.min_response_length > 0);
    assert!(c.max_response_length > c.min_response_length);
    assert!(
        !c.forbidden_keywords.is_empty(),
        "default should forbid some phrases"
    );
    assert!(c.min_coherence_score > 0.0 && c.min_coherence_score < 1.0);
    assert!(c.min_relevance_score > 0.0 && c.min_relevance_score < 1.0);
}

#[test]
fn criteria_technical_documentation_is_stricter_than_default() {
    let default = ValidationCriteria::default();
    let tech = ValidationCriteria::technical_documentation();
    assert!(tech.min_response_length > default.min_response_length);
    assert!(tech.min_coherence_score > default.min_coherence_score);
    assert!(tech.min_relevance_score > default.min_relevance_score);
}

#[test]
fn criteria_creative_writing_is_more_permissive_than_default() {
    let default = ValidationCriteria::default();
    let creative = ValidationCriteria::creative_writing();
    assert!(!creative.require_factual_accuracy);
    assert!(creative.min_coherence_score < default.min_coherence_score);
    assert!(creative.min_relevance_score < default.min_relevance_score);
}

#[test]
fn criteria_with_required_keywords_replaces_list() {
    let kw = vec!["alpha".to_string(), "beta".to_string()];
    let c = ValidationCriteria::default().with_required_keywords(kw.clone());
    assert_eq!(c.required_keywords, kw);
}

#[test]
fn criteria_with_forbidden_keywords_replaces_list() {
    let kw = vec!["gamma".to_string()];
    let c = ValidationCriteria::default().with_forbidden_keywords(kw.clone());
    assert_eq!(c.forbidden_keywords, kw);
}

// ─────────────────────────────────────────────────────────────────────────────────
// ValidationResult helpers
// ─────────────────────────────────────────────────────────────────────────────────

fn make_success() -> ValidationResult {
    ValidationResult::Success {
        message: "all good".to_string(),
        metrics: ValidationMetrics::default(),
    }
}

fn make_warning() -> ValidationResult {
    ValidationResult::Warning {
        message: "slow".to_string(),
        suggestions: vec!["try again".to_string()],
        metrics: ValidationMetrics::default(),
    }
}

fn make_failure() -> ValidationResult {
    ValidationResult::Failure {
        message: "boom".to_string(),
        error_details: "oops".to_string(),
        suggestions: vec!["fix it".to_string()],
        metrics: None,
    }
}

#[test]
fn result_is_success_only_for_success_variant() {
    assert!(make_success().is_success());
    assert!(!make_warning().is_success());
    assert!(!make_failure().is_success());
}

#[test]
fn result_is_warning_only_for_warning_variant() {
    assert!(!make_success().is_warning());
    assert!(make_warning().is_warning());
    assert!(!make_failure().is_warning());
}

#[test]
fn result_is_failure_only_for_failure_variant() {
    assert!(!make_success().is_failure());
    assert!(!make_warning().is_failure());
    assert!(make_failure().is_failure());
}

#[test]
fn result_suggestions_empty_for_success() {
    assert!(make_success().suggestions().is_empty());
}

#[test]
fn result_suggestions_nonempty_for_warning_and_failure() {
    assert!(!make_warning().suggestions().is_empty());
    assert!(!make_failure().suggestions().is_empty());
}

#[test]
fn result_metrics_present_for_success_and_warning() {
    assert!(make_success().metrics().is_some());
    assert!(make_warning().metrics().is_some());
}

#[test]
fn result_metrics_none_for_failure_without_metrics() {
    assert!(make_failure().metrics().is_none());
}

#[test]
fn result_metrics_some_for_failure_with_metrics() {
    let r = ValidationResult::Failure {
        message: "bad".to_string(),
        error_details: "details".to_string(),
        suggestions: vec![],
        metrics: Some(ValidationMetrics::with_duration(Duration::from_millis(5))),
    };
    assert!(r.metrics().is_some());
}

#[test]
fn result_display_contains_variant_marker() {
    // Assert on the full emoji+word token so a regression that drops the emoji
    // is caught (the existing in-module test already checks "\u{2705} SUCCESS").
    assert!(format!("{}", make_success()).contains("\u{2705} SUCCESS"));
    assert!(format!("{}", make_failure()).contains("\u{274c} FAILURE"));
    // WARNING uses a multi-codepoint emoji sequence; assert on the exact compound
    // token that the Display impl emits so the test catches regressions where
    // the emoji is moved or dropped, and is consistent with SUCCESS/FAILURE above.
    let warning_str = format!("{}", make_warning());
    assert!(
        warning_str.contains("\u{26a0}\u{fe0f}  WARNING"),
        "WARNING display should contain '\u{26a0}\u{fe0f}  WARNING', got: {}",
        warning_str
    );
}

// ─────────────────────────────────────────────────────────────────────────────────
// ValidationMetrics
// ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn metrics_default_has_zero_duration_and_no_optionals() {
    let m = ValidationMetrics::default();
    assert_eq!(m.duration, Duration::ZERO);
    assert_eq!(m.retry_count, 0);
    assert!(m.response_length.is_none());
    assert!(m.coherence_score.is_none());
    assert!(m.relevance_score.is_none());
    assert!(m.success_rate.is_none());
    assert!(m.custom_metrics.is_empty());
}

#[test]
fn metrics_builder_chain_sets_all_fields() {
    let mut m = ValidationMetrics::with_duration(Duration::from_secs(2))
        .with_response_length(300)
        .with_coherence_score(0.9)
        .with_relevance_score(0.8);
    m.add_custom_metric("my_score".to_string(), 42.0);

    assert_eq!(m.duration, Duration::from_secs(2));
    assert_eq!(m.response_length, Some(300));
    assert_eq!(m.coherence_score, Some(0.9));
    assert_eq!(m.relevance_score, Some(0.8));
    assert_eq!(m.custom_metrics["my_score"], 42.0);
}

#[test]
fn metrics_coherence_score_is_clamped() {
    let above = ValidationMetrics::default().with_coherence_score(1.5);
    assert_eq!(above.coherence_score, Some(1.0));

    let below = ValidationMetrics::default().with_coherence_score(-0.5);
    assert_eq!(below.coherence_score, Some(0.0));
}

#[test]
fn metrics_relevance_score_is_clamped() {
    let above = ValidationMetrics::default().with_relevance_score(2.0);
    assert_eq!(above.relevance_score, Some(1.0));

    let below = ValidationMetrics::default().with_relevance_score(-1.0);
    assert_eq!(below.relevance_score, Some(0.0));
}

#[test]
fn metrics_display_includes_all_populated_fields() {
    let m = ValidationMetrics {
        duration: Duration::from_millis(500),
        retry_count: 2,
        response_length: Some(100),
        coherence_score: Some(0.85),
        relevance_score: Some(0.92),
        success_rate: Some(0.75),
        custom_metrics: HashMap::new(),
    };
    let s = format!("{}", m);
    // The Display impl uses `format!("duration: {:?}", self.duration)` which
    // emits the label "duration: " followed by the Debug representation.
    // Assert on both the label and the numeric millis value so that the test
    // fails if the duration field is absent (guards against false-pass when
    // "500" happens to appear in another field such as response_length).
    assert!(
        s.contains("duration:") && s.contains(&m.duration.as_millis().to_string()),
        "expected 'duration:' label with {} millis in display, got: {}",
        m.duration.as_millis(),
        s
    );
    assert!(s.contains("retries: 2"), "expected retries, got: {}", s);
    assert!(s.contains("length: 100"), "expected length, got: {}", s);
    assert!(
        s.contains("coherence: 0.85"),
        "expected coherence, got: {}",
        s
    );
    assert!(
        s.contains("relevance: 0.92"),
        "expected relevance, got: {}",
        s
    );
    assert!(
        s.contains("success_rate: 75.00%"),
        "expected success_rate, got: {}",
        s
    );
}

#[test]
fn metrics_display_omits_zero_retry_count() {
    let m = ValidationMetrics::with_duration(Duration::from_millis(10));
    let s = format!("{}", m);
    assert!(
        !s.contains("retries"),
        "zero retries should be omitted, got: {}",
        s
    );
}

// ─────────────────────────────────────────────────────────────────────────────────
// JudgeConfig
// ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn config_defaults_are_correct() {
    let c = JudgeConfig::default();
    assert_eq!(c.max_retries, 3);
    assert_eq!(c.base_delay_ms, 100);
    assert_eq!(c.max_delay_ms, 5000);
    assert!(!c.verbose_logging);
    assert_eq!(c.default_timeout, Duration::from_secs(30));
}

#[test]
fn config_with_retries_sets_fields_and_keeps_defaults_for_rest() {
    let c = JudgeConfig::with_retries(7, 250);
    assert_eq!(c.max_retries, 7);
    assert_eq!(c.base_delay_ms, 250);
    assert_eq!(c.max_delay_ms, JudgeConfig::default().max_delay_ms);
    assert!(!c.verbose_logging);
}

#[test]
fn config_with_verbose_logging_enables_flag() {
    let c = JudgeConfig::default().with_verbose_logging();
    assert!(c.verbose_logging);
}

#[test]
fn config_with_timeout_sets_duration() {
    let c = JudgeConfig::default().with_timeout(Duration::from_secs(120));
    assert_eq!(c.default_timeout, Duration::from_secs(120));
}

#[test]
fn config_retry_delay_grows_with_attempt_number() {
    // Use zero jitter so we can reason about exact growth.
    let c = JudgeConfig {
        jitter_factor: 0.0,
        base_delay_ms: 100,
        max_delay_ms: 10_000,
        ..JudgeConfig::default()
    };

    let d0 = c.calculate_retry_delay(0);
    let d1 = c.calculate_retry_delay(1);
    let d2 = c.calculate_retry_delay(2);

    // Attempt 0: 100ms * 2^0 = 100ms
    // Attempt 1: 100ms * 2^1 = 200ms
    // Attempt 2: 100ms * 2^2 = 400ms
    assert_eq!(d0, Duration::from_millis(100));
    assert_eq!(d1, Duration::from_millis(200));
    assert_eq!(d2, Duration::from_millis(400));
}

#[test]
fn config_retry_delay_is_capped_at_max_delay() {
    let c = JudgeConfig {
        jitter_factor: 0.0,
        base_delay_ms: 1000,
        max_delay_ms: 2000,
        ..JudgeConfig::default()
    };

    // Attempt 5 would be 1000 * 32 = 32_000ms, well over the 2_000ms cap.
    let delay = c.calculate_retry_delay(5);
    assert_eq!(
        delay,
        Duration::from_millis(2000),
        "delay should be capped at max_delay_ms, got {:?}",
        delay
    );
}

/// Regression test: attempt >= 32 previously caused u32::pow to panic in debug
/// or silently overflow in release.  With the fix (saturating u64 arithmetic)
/// the result must simply be capped at max_delay_ms without any panic.
#[test]
fn config_retry_delay_large_attempt_does_not_overflow() {
    let c = JudgeConfig {
        jitter_factor: 0.0,
        base_delay_ms: 100,
        max_delay_ms: 5000,
        ..JudgeConfig::default()
    };

    // All values >= 32 are saturated to max_delay_ms.
    for attempt in [32_u32, 33, 63, 64, 100, u32::MAX] {
        let delay = c.calculate_retry_delay(attempt);
        assert_eq!(
            delay,
            Duration::from_millis(5000),
            "attempt={}: expected cap at max_delay_ms, got {:?}",
            attempt,
            delay
        );
    }
}

#[test]
fn config_retry_delay_with_jitter_is_at_least_base() {
    let c = JudgeConfig {
        jitter_factor: 0.5,
        base_delay_ms: 100,
        max_delay_ms: 5000,
        ..JudgeConfig::default()
    };

    // Jitter range for attempt 0 is 0.0..=0.5 of the base delay (100ms),
    // so the result must be in [100ms, 150ms].  The lower bound confirms jitter
    // is additive-only; the upper bound confirms the jitter factor is respected.
    let no_jitter = Duration::from_millis(100);
    let max_jitter = Duration::from_millis(150); // 100ms + 50% of 100ms
    for _ in 0..20 {
        let d = c.calculate_retry_delay(0);
        assert!(
            d >= no_jitter,
            "jitter must not reduce the delay below base, got {:?}",
            d
        );
        assert!(
            d <= max_jitter,
            "jitter must not exceed base + jitter_factor * base = 150ms, got {:?}",
            d
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────
// validate_comprehensive – via MockModelJudge
// ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn comprehensive_with_no_tools_and_no_prompts_returns_two_results() {
    let judge = MockModelJudge::all_success();
    let criteria = ValidationCriteria::default();

    let results = judge
        .validate_comprehensive(Duration::from_secs(1), &criteria, &[], &[], 1)
        .await
        .expect("validate_comprehensive should not error");

    // Only responsiveness + quality when tools and prompts are both empty.
    assert_eq!(
        results.len(),
        2,
        "expected 2 results, got {}",
        results.len()
    );
    assert!(results.iter().all(|r| r.is_success()));
}

#[tokio::test]
async fn comprehensive_with_tools_adds_tool_result() {
    use model::types::{FunctionDefinition, JsonSchema, SchemaType};

    let judge = MockModelJudge::all_success();
    let criteria = ValidationCriteria::default();

    let tool = ToolDefinition {
        function: FunctionDefinition {
            name: "noop".to_string(),
            description: "does nothing".to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: None,
                required: None,
            },
        },
    };

    let results = judge
        .validate_comprehensive(Duration::from_secs(1), &criteria, &[tool], &[], 1)
        .await
        .expect("validate_comprehensive should not error");

    assert_eq!(
        results.len(),
        3,
        "expected 3 results with tools, got {}",
        results.len()
    );
}

#[tokio::test]
async fn comprehensive_with_consistency_prompts_adds_consistency_result() {
    let judge = MockModelJudge::all_success();
    let criteria = ValidationCriteria::default();

    let results = judge
        .validate_comprehensive(Duration::from_secs(1), &criteria, &[], &["What is 2+2?"], 2)
        .await
        .expect("validate_comprehensive should not error");

    assert_eq!(
        results.len(),
        3,
        "expected 3 results with consistency prompts, got {}",
        results.len()
    );
}

#[tokio::test]
async fn comprehensive_all_branches_returns_four_results() {
    use model::types::{FunctionDefinition, JsonSchema, SchemaType};

    let judge = MockModelJudge::all_success();
    let criteria = ValidationCriteria::default();

    let tool = ToolDefinition {
        function: FunctionDefinition {
            name: "noop".to_string(),
            description: "does nothing".to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: None,
                required: None,
            },
        },
    };

    let results = judge
        .validate_comprehensive(
            Duration::from_secs(1),
            &criteria,
            &[tool],
            &["prompt1", "prompt2"],
            3,
        )
        .await
        .expect("validate_comprehensive should not error");

    assert_eq!(
        results.len(),
        4,
        "expected 4 results when all branches are exercised, got {}",
        results.len()
    );
    assert!(results.iter().all(|r| r.is_success()));
}

#[tokio::test]
async fn comprehensive_failure_injection_preserved_in_results() {
    let failure = ValidationResult::Failure {
        message: "injected failure".to_string(),
        error_details: "test".to_string(),
        suggestions: vec![],
        metrics: None,
    };

    let judge = MockModelJudge::all_success().with_responsiveness(failure);
    let criteria = ValidationCriteria::default();

    let results = judge
        .validate_comprehensive(Duration::from_secs(1), &criteria, &[], &[], 1)
        .await
        .expect("validate_comprehensive itself should not Err");

    // Index 0 = responsiveness (always the first call in validate_comprehensive).
    // Index 1 = quality       (always the second call).
    // This positional coupling is intentional: if the call order in
    // validate_comprehensive changes, these assertions must be updated too.
    assert!(
        results[0].is_failure(),
        "index 0 (responsiveness) should be the injected failure"
    );
    assert!(
        results[1].is_success(),
        "index 1 (quality) should still be success"
    );
}

#[tokio::test]
async fn comprehensive_all_fail_all_results_are_failures() {
    let make_fail = || ValidationResult::Failure {
        message: "fail".to_string(),
        error_details: "err".to_string(),
        suggestions: vec![],
        metrics: None,
    };

    let judge = MockModelJudge::all_success()
        .with_responsiveness(make_fail())
        .with_quality(make_fail())
        .with_tools(make_fail())
        .with_consistency(make_fail());

    use model::types::{FunctionDefinition, JsonSchema, SchemaType};
    let tool = ToolDefinition {
        function: FunctionDefinition {
            name: "noop".to_string(),
            description: "does nothing".to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: None,
                required: None,
            },
        },
    };

    let criteria = ValidationCriteria::default();
    let results = judge
        .validate_comprehensive(Duration::from_secs(1), &criteria, &[tool], &["prompt"], 1)
        .await
        .expect("validate_comprehensive should not error");

    assert_eq!(results.len(), 4);
    assert!(
        results.iter().all(|r| r.is_failure()),
        "all results should be failures"
    );
}

#[tokio::test]
async fn comprehensive_mixed_pass_fail_results_are_ordered_correctly() {
    // responsiveness=success, quality=failure, no tools, no prompts
    let judge = MockModelJudge::all_success().with_quality(ValidationResult::Failure {
        message: "quality failed".to_string(),
        error_details: "low score".to_string(),
        suggestions: vec!["Improve model".to_string()],
        metrics: None,
    });
    let criteria = ValidationCriteria::default();

    let results = judge
        .validate_comprehensive(Duration::from_secs(1), &criteria, &[], &[], 1)
        .await
        .expect("validate_comprehensive should not error");

    assert_eq!(results.len(), 2);
    assert!(
        results[0].is_success(),
        "index 0 should be responsiveness success"
    );
    assert!(results[1].is_failure(), "index 1 should be quality failure");
}

/// Exercises the Err-to-Failure conversion path inside validate_comprehensive.
/// When a sub-validator returns Err (because the corresponding field is None),
/// validate_comprehensive must convert it into a ValidationResult::Failure and
/// continue rather than propagating the error.
#[tokio::test]
async fn comprehensive_converts_model_err_into_failure_result() {
    // responsiveness_result = None => validate_api_responsiveness returns Err
    // quality_result        = Some => validate_response_quality returns Ok
    let judge = MockModelJudge {
        config: JudgeConfig::default(),
        responsiveness_result: None, // triggers ModelError::Unknown
        quality_result: Some(ValidationResult::Success {
            message: "ok".into(),
            metrics: ValidationMetrics::default(),
        }),
        tool_result: None,
        consistency_result: None,
    };

    let results = judge
        .validate_comprehensive(
            Duration::from_secs(1),
            &ValidationCriteria::default(),
            &[],
            &[],
            1,
        )
        .await
        .expect("validate_comprehensive should not propagate Err");

    // Only two sub-calls are made (responsiveness + quality; no tools, no prompts).
    assert_eq!(results.len(), 2);
    assert!(
        results[0].is_failure(),
        "ModelError from responsiveness should become Failure, got {:?}",
        results[0]
    );
    assert!(
        results[1].is_success(),
        "quality Ok should remain Success, got {:?}",
        results[1]
    );
}

// ─────────────────────────────────────────────────────────────────────────────────
// MockModelJudge – cover previously-uncovered code paths
// ─────────────────────────────────────────────────────────────────────────────────

/// Calling list_models and health_check on the mock exercises those
/// ModelProvider method bodies (previously uncovered).
#[tokio::test]
async fn mock_provider_list_models_and_health_check() {
    let judge = MockModelJudge::all_success();
    let models = judge.list_models().await.expect("list_models should succeed");
    assert!(models.is_empty());
    judge
        .health_check()
        .await
        .expect("health_check should succeed");
    assert_eq!(judge.provider_name(), "mock");
}

/// When a field is None the ok_or_else closure executes and returns Err.
/// This covers the error arms that were previously dead code.
#[tokio::test]
async fn mock_judge_returns_err_when_result_is_none() {
    // Build a judge with all fields explicitly None.
    let judge = MockModelJudge {
        config: JudgeConfig::default(),
        responsiveness_result: None,
        quality_result: None,
        tool_result: None,
        consistency_result: None,
    };

    // validate_api_responsiveness -> Err
    assert!(
        judge
            .validate_api_responsiveness(Duration::from_secs(1))
            .await
            .is_err(),
        "expected Err when responsiveness_result is None"
    );

    // validate_response_quality -> Err
    assert!(
        judge
            .validate_response_quality("prompt", &ValidationCriteria::default())
            .await
            .is_err(),
        "expected Err when quality_result is None"
    );

    // validate_tool_calling -> Err
    assert!(
        judge.validate_tool_calling(&[]).await.is_err(),
        "expected Err when tool_result is None"
    );

    // validate_consistency -> Err
    assert!(
        judge.validate_consistency(&[], 1).await.is_err(),
        "expected Err when consistency_result is None"
    );
}
