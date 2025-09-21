//! Model validation and judging framework
//!
//! This module provides a comprehensive framework for validating AI model performance,
//! responsiveness, and reliability. It includes tools for benchmarking, consistency
//! testing, and quality assessment.
//!
//! # Examples
//!
//! ```rust
//! use model::judge::{ModelJudge, ValidationCriteria, JudgeConfig};
//! use model::OllamaProvider;
//! use std::time::Duration;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let provider = OllamaProvider::with_default_config()?;
//! let config = JudgeConfig::default();
//! let criteria = ValidationCriteria::default();
//!
//! // Test API responsiveness
//! let result = provider.validate_api_responsiveness(Duration::from_secs(5)).await?;
//! println!("Responsiveness: {}", result);
//!
//! // Test response quality
//! let result = provider.validate_response_quality(
//!     "Explain quantum computing in simple terms",
//!     &criteria
//! ).await?;
//! println!("Quality: {}", result);
//! # Ok(())
//! # }
//! ```

use crate::provider::{ModelProvider, ModelResult};
use crate::types::ToolDefinition;
use async_trait::async_trait;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Configuration for model validation parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeConfig {
    /// Maximum number of retry attempts for failed operations
    pub max_retries: u32,
    /// Base delay for exponential backoff (in milliseconds)
    pub base_delay_ms: u64,
    /// Maximum delay for exponential backoff (in milliseconds)
    pub max_delay_ms: u64,
    /// Jitter factor for randomizing retry delays (0.0 to 1.0)
    pub jitter_factor: f64,
    /// Default timeout for individual requests
    pub default_timeout: Duration,
    /// Enable detailed logging for validation operations
    pub verbose_logging: bool,
}

impl Default for JudgeConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 100,
            max_delay_ms: 5000,
            jitter_factor: 0.1,
            default_timeout: Duration::from_secs(30),
            verbose_logging: false,
        }
    }
}

impl JudgeConfig {
    /// Create a new configuration with custom retry settings
    pub fn with_retries(max_retries: u32, base_delay_ms: u64) -> Self {
        Self {
            max_retries,
            base_delay_ms,
            ..Default::default()
        }
    }

    /// Enable verbose logging for debugging
    pub fn with_verbose_logging(mut self) -> Self {
        self.verbose_logging = true;
        self
    }

    /// Set custom timeout for requests
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Calculate delay for retry attempt with exponential backoff and jitter
    pub fn calculate_retry_delay(&self, attempt: u32) -> Duration {
        let base_delay = Duration::from_millis(self.base_delay_ms);
        let exponential_delay = base_delay * 2_u32.pow(attempt);
        let max_delay = Duration::from_millis(self.max_delay_ms);

        let delay = exponential_delay.min(max_delay);

        // Add jitter to prevent thundering herd
        if self.jitter_factor > 0.0 {
            let mut rng = rand::thread_rng();
            let jitter = rng.gen_range(0.0..=self.jitter_factor);
            let jitter_ms = (delay.as_millis() as f64 * jitter) as u64;
            delay + Duration::from_millis(jitter_ms)
        } else {
            delay
        }
    }
}

/// Criteria for validating model responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCriteria {
    /// Minimum expected response length in characters
    pub min_response_length: usize,
    /// Maximum acceptable response length in characters
    pub max_response_length: usize,
    /// Keywords or phrases that should appear in the response
    pub required_keywords: Vec<String>,
    /// Keywords or phrases that should NOT appear in the response
    pub forbidden_keywords: Vec<String>,
    /// Minimum coherence score (0.0 to 1.0)
    pub min_coherence_score: f64,
    /// Minimum relevance score (0.0 to 1.0)
    pub min_relevance_score: f64,
    /// Whether the response should be factually accurate
    pub require_factual_accuracy: bool,
    /// Custom validation functions
    pub custom_validators: Vec<String>, // Function names for extensibility
}

impl Default for ValidationCriteria {
    fn default() -> Self {
        Self {
            min_response_length: 10,
            max_response_length: 10000,
            required_keywords: Vec::new(),
            forbidden_keywords: vec![
                "I cannot".to_string(),
                "I don't know".to_string(),
                "unable to".to_string(),
            ],
            min_coherence_score: 0.7,
            min_relevance_score: 0.8,
            require_factual_accuracy: true,
            custom_validators: Vec::new(),
        }
    }
}

impl ValidationCriteria {
    /// Create criteria for technical documentation validation
    pub fn technical_documentation() -> Self {
        Self {
            min_response_length: 100,
            max_response_length: 5000,
            required_keywords: vec!["implementation".to_string(), "example".to_string()],
            forbidden_keywords: vec!["I think".to_string(), "maybe".to_string()],
            min_coherence_score: 0.85,
            min_relevance_score: 0.9,
            require_factual_accuracy: true,
            custom_validators: Vec::new(),
        }
    }

    /// Create criteria for creative writing validation
    pub fn creative_writing() -> Self {
        Self {
            min_response_length: 50,
            max_response_length: 20000,
            required_keywords: Vec::new(),
            forbidden_keywords: vec!["error".to_string(), "failed".to_string()],
            min_coherence_score: 0.6,
            min_relevance_score: 0.7,
            require_factual_accuracy: false,
            custom_validators: Vec::new(),
        }
    }

    /// Add required keywords to the criteria
    pub fn with_required_keywords(mut self, keywords: Vec<String>) -> Self {
        self.required_keywords = keywords;
        self
    }

    /// Add forbidden keywords to the criteria
    pub fn with_forbidden_keywords(mut self, keywords: Vec<String>) -> Self {
        self.forbidden_keywords = keywords;
        self
    }
}

/// Detailed result of a validation operation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ValidationResult {
    /// Validation passed successfully
    Success {
        /// Human-readable description of what passed
        message: String,
        /// Performance metrics collected during validation
        metrics: ValidationMetrics,
    },
    /// Validation failed with recoverable issues
    Warning {
        /// Description of the warning
        message: String,
        /// Suggestions for improvement
        suggestions: Vec<String>,
        /// Metrics collected before failure
        metrics: ValidationMetrics,
    },
    /// Validation failed critically
    Failure {
        /// Description of the failure
        message: String,
        /// Detailed error information
        error_details: String,
        /// Suggestions for fixing the issue
        suggestions: Vec<String>,
        /// Partial metrics if available
        metrics: Option<ValidationMetrics>,
    },
}

impl fmt::Display for ValidationResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationResult::Success { message, metrics } => {
                write!(f, "✅ SUCCESS: {} ({})", message, metrics)
            }
            ValidationResult::Warning {
                message,
                suggestions,
                metrics,
            } => {
                write!(
                    f,
                    "⚠️  WARNING: {} ({}) - Suggestions: {}",
                    message,
                    metrics,
                    suggestions.join(", ")
                )
            }
            ValidationResult::Failure {
                message,
                error_details,
                suggestions,
                ..
            } => {
                write!(
                    f,
                    "❌ FAILURE: {} - Error: {} - Suggestions: {}",
                    message,
                    error_details,
                    suggestions.join(", ")
                )
            }
        }
    }
}

impl ValidationResult {
    /// Check if the validation was successful
    pub fn is_success(&self) -> bool {
        matches!(self, ValidationResult::Success { .. })
    }

    /// Check if the validation had warnings
    pub fn is_warning(&self) -> bool {
        matches!(self, ValidationResult::Warning { .. })
    }

    /// Check if the validation failed
    pub fn is_failure(&self) -> bool {
        matches!(self, ValidationResult::Failure { .. })
    }

    /// Get the metrics from the validation result
    pub fn metrics(&self) -> Option<&ValidationMetrics> {
        match self {
            ValidationResult::Success { metrics, .. } => Some(metrics),
            ValidationResult::Warning { metrics, .. } => Some(metrics),
            ValidationResult::Failure { metrics, .. } => metrics.as_ref(),
        }
    }

    /// Get suggestions for improvement
    pub fn suggestions(&self) -> Vec<&str> {
        match self {
            ValidationResult::Success { .. } => Vec::new(),
            ValidationResult::Warning { suggestions, .. } => {
                suggestions.iter().map(|s| s.as_str()).collect()
            }
            ValidationResult::Failure { suggestions, .. } => {
                suggestions.iter().map(|s| s.as_str()).collect()
            }
        }
    }
}

/// Performance and quality metrics collected during validation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidationMetrics {
    /// Duration of the operation
    pub duration: Duration,
    /// Number of retry attempts made
    pub retry_count: u32,
    /// Response length in characters
    pub response_length: Option<usize>,
    /// Calculated coherence score (0.0 to 1.0)
    pub coherence_score: Option<f64>,
    /// Calculated relevance score (0.0 to 1.0)
    pub relevance_score: Option<f64>,
    /// Success rate for multiple attempts
    pub success_rate: Option<f64>,
    /// Additional custom metrics
    pub custom_metrics: HashMap<String, f64>,
}

impl fmt::Display for ValidationMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = vec![format!("duration: {:?}", self.duration)];

        if self.retry_count > 0 {
            parts.push(format!("retries: {}", self.retry_count));
        }

        if let Some(len) = self.response_length {
            parts.push(format!("length: {}", len));
        }

        if let Some(score) = self.coherence_score {
            parts.push(format!("coherence: {:.2}", score));
        }

        if let Some(score) = self.relevance_score {
            parts.push(format!("relevance: {:.2}", score));
        }

        if let Some(rate) = self.success_rate {
            parts.push(format!("success_rate: {:.2}%", rate * 100.0));
        }

        write!(f, "{}", parts.join(", "))
    }
}

impl Default for ValidationMetrics {
    fn default() -> Self {
        Self {
            duration: Duration::ZERO,
            retry_count: 0,
            response_length: None,
            coherence_score: None,
            relevance_score: None,
            success_rate: None,
            custom_metrics: HashMap::new(),
        }
    }
}

impl ValidationMetrics {
    /// Create new metrics with a duration
    pub fn with_duration(duration: Duration) -> Self {
        Self {
            duration,
            ..Default::default()
        }
    }

    /// Add a custom metric
    pub fn add_custom_metric(&mut self, name: String, value: f64) {
        self.custom_metrics.insert(name, value);
    }

    /// Set the response length
    pub fn with_response_length(mut self, length: usize) -> Self {
        self.response_length = Some(length);
        self
    }

    /// Set the coherence score
    pub fn with_coherence_score(mut self, score: f64) -> Self {
        self.coherence_score = Some(score.clamp(0.0, 1.0));
        self
    }

    /// Set the relevance score
    pub fn with_relevance_score(mut self, score: f64) -> Self {
        self.relevance_score = Some(score.clamp(0.0, 1.0));
        self
    }
}

/// Main trait for model validation and judging
#[async_trait]
pub trait ModelJudge: ModelProvider {
    /// Get the judge configuration
    fn judge_config(&self) -> &JudgeConfig;

    /// Validate API responsiveness within a given latency threshold
    ///
    /// This method tests whether the model can respond within acceptable time limits.
    /// It performs multiple health checks and measures response times.
    ///
    /// # Arguments
    /// * `latency_threshold` - Maximum acceptable response time
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::time::Duration;
    /// use model::judge::ModelJudge;
    /// # use model::OllamaProvider;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let provider = OllamaProvider::with_default_config()?;
    /// let result = provider.validate_api_responsiveness(Duration::from_secs(5)).await?;
    /// assert!(result.is_success());
    /// # Ok(())
    /// # }
    /// ```
    async fn validate_api_responsiveness(
        &self,
        latency_threshold: Duration,
    ) -> ModelResult<ValidationResult>;

    /// Validate response quality against specified criteria
    ///
    /// This method sends a prompt to the model and validates the response quality
    /// based on length, coherence, relevance, and content criteria.
    ///
    /// # Arguments
    /// * `prompt` - The prompt to send to the model
    /// * `expected_criteria` - Criteria for validating the response
    ///
    /// # Examples
    /// ```rust,no_run
    /// use model::judge::{ModelJudge, ValidationCriteria};
    /// # use model::OllamaProvider;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let provider = OllamaProvider::with_default_config()?;
    /// let criteria = ValidationCriteria::default();
    /// let result = provider.validate_response_quality(
    ///     "Explain machine learning",
    ///     &criteria
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn validate_response_quality(
        &self,
        prompt: &str,
        expected_criteria: &ValidationCriteria,
    ) -> ModelResult<ValidationResult>;

    /// Validate tool calling capabilities
    ///
    /// This method tests whether the model can properly use provided tools
    /// and generate appropriate tool calls.
    ///
    /// # Arguments
    /// * `tools` - List of tool definitions to test
    ///
    /// # Examples
    /// ```rust,no_run
    /// use model::judge::ModelJudge;
    /// use model::types::{ToolDefinition, FunctionDefinition, JsonSchema, SchemaType};
    /// # use model::OllamaProvider;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let provider = OllamaProvider::with_default_config()?;
    /// let tools = vec![]; // Tool definitions would go here
    /// let result = provider.validate_tool_calling(&tools).await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn validate_tool_calling(
        &self,
        tools: &[ToolDefinition],
    ) -> ModelResult<ValidationResult>;

    /// Validate response consistency across multiple iterations
    ///
    /// This method sends the same prompts multiple times and checks for
    /// consistent behavior and outputs.
    ///
    /// # Arguments
    /// * `prompts` - List of prompts to test
    /// * `iterations` - Number of times to repeat each prompt
    ///
    /// # Examples
    /// ```rust,no_run
    /// use model::judge::ModelJudge;
    /// # use model::OllamaProvider;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let provider = OllamaProvider::with_default_config()?;
    /// let prompts = vec!["What is 2+2?", "Explain gravity"];
    /// let result = provider.validate_consistency(&prompts, 3).await?;
    /// # Ok(())
    /// # }
    /// ```
    async fn validate_consistency(
        &self,
        prompts: &[&str],
        iterations: usize,
    ) -> ModelResult<ValidationResult>;

    /// Run a comprehensive validation suite
    ///
    /// This method runs all validation tests and provides a summary report.
    async fn validate_comprehensive(
        &self,
        latency_threshold: Duration,
        quality_criteria: &ValidationCriteria,
        tools: &[ToolDefinition],
        consistency_prompts: &[&str],
        consistency_iterations: usize,
    ) -> ModelResult<Vec<ValidationResult>> {
        let mut results = Vec::new();

        info!("Starting comprehensive model validation");

        // Test API responsiveness
        debug!("Testing API responsiveness");
        match self.validate_api_responsiveness(latency_threshold).await {
            Ok(result) => results.push(result),
            Err(e) => {
                warn!("API responsiveness test failed: {}", e);
                results.push(ValidationResult::Failure {
                    message: "API responsiveness test failed".to_string(),
                    error_details: e.to_string(),
                    suggestions: vec![
                        "Check network connectivity".to_string(),
                        "Verify service is running".to_string(),
                    ],
                    metrics: None,
                });
            }
        }

        // Test response quality
        debug!("Testing response quality");
        let quality_prompt =
            "Explain the concept of artificial intelligence in a clear and comprehensive manner.";
        match self
            .validate_response_quality(quality_prompt, quality_criteria)
            .await
        {
            Ok(result) => results.push(result),
            Err(e) => {
                warn!("Response quality test failed: {}", e);
                results.push(ValidationResult::Failure {
                    message: "Response quality test failed".to_string(),
                    error_details: e.to_string(),
                    suggestions: vec![
                        "Adjust validation criteria".to_string(),
                        "Check model parameters".to_string(),
                    ],
                    metrics: None,
                });
            }
        }

        // Test tool calling if tools are provided
        if !tools.is_empty() {
            debug!("Testing tool calling capabilities");
            match self.validate_tool_calling(tools).await {
                Ok(result) => results.push(result),
                Err(e) => {
                    warn!("Tool calling test failed: {}", e);
                    results.push(ValidationResult::Failure {
                        message: "Tool calling test failed".to_string(),
                        error_details: e.to_string(),
                        suggestions: vec![
                            "Verify tool definitions".to_string(),
                            "Check model tool support".to_string(),
                        ],
                        metrics: None,
                    });
                }
            }
        }

        // Test consistency if prompts are provided
        if !consistency_prompts.is_empty() {
            debug!("Testing response consistency");
            match self
                .validate_consistency(consistency_prompts, consistency_iterations)
                .await
            {
                Ok(result) => results.push(result),
                Err(e) => {
                    warn!("Consistency test failed: {}", e);
                    results.push(ValidationResult::Failure {
                        message: "Consistency test failed".to_string(),
                        error_details: e.to_string(),
                        suggestions: vec![
                            "Reduce temperature parameter".to_string(),
                            "Check for deterministic settings".to_string(),
                        ],
                        metrics: None,
                    });
                }
            }
        }

        info!(
            "Comprehensive validation completed with {} results",
            results.len()
        );
        Ok(results)
    }
}

/// Helper function to calculate simple coherence score based on text structure
pub fn calculate_coherence_score(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }

    let mut score = 0.5; // Base score

    // Check for sentence structure
    let sentence_count = text.split('.').filter(|s| !s.trim().is_empty()).count();
    if sentence_count > 0 {
        score += 0.2;
    }

    // Check for paragraph structure
    let paragraph_count = text.split('\n').filter(|s| !s.trim().is_empty()).count();
    if paragraph_count > 1 {
        score += 0.1;
    }

    // Check for reasonable length
    if text.len() > 50 && text.len() < 5000 {
        score += 0.1;
    }

    // Check for word variety (simple metric)
    let words: Vec<&str> = text.split_whitespace().collect();
    let unique_words: std::collections::HashSet<&str> = words.iter().cloned().collect();
    let word_variety = unique_words.len() as f64 / words.len() as f64;
    score += word_variety * 0.1;

    score.clamp(0.0, 1.0)
}

/// Helper function to calculate relevance score based on keyword matching
pub fn calculate_relevance_score(text: &str, prompt: &str, criteria: &ValidationCriteria) -> f64 {
    let text_lower = text.to_lowercase();
    let prompt_lower = prompt.to_lowercase();

    let mut score = 0.0;

    // Check for required keywords
    if !criteria.required_keywords.is_empty() {
        let found_keywords = criteria
            .required_keywords
            .iter()
            .filter(|keyword| text_lower.contains(&keyword.to_lowercase()))
            .count();
        score += (found_keywords as f64 / criteria.required_keywords.len() as f64) * 0.5;
    } else {
        score += 0.5; // Give base score if no required keywords
    }

    // Penalty for forbidden keywords
    let forbidden_found = criteria
        .forbidden_keywords
        .iter()
        .any(|keyword| text_lower.contains(&keyword.to_lowercase()));
    if forbidden_found {
        score -= 0.3;
    }

    // Check for prompt terms in response
    let prompt_words: Vec<&str> = prompt_lower.split_whitespace().collect();
    let response_words: Vec<&str> = text_lower.split_whitespace().collect();
    let common_words = prompt_words
        .iter()
        .filter(|word| response_words.contains(word))
        .count();

    if !prompt_words.is_empty() {
        score += (common_words as f64 / prompt_words.len() as f64) * 0.3;
    }

    // Length appropriateness
    if text.len() >= criteria.min_response_length && text.len() <= criteria.max_response_length {
        score += 0.2;
    } else if text.len() < criteria.min_response_length {
        score -= 0.2;
    } else {
        score -= 0.1; // Less penalty for too long
    }

    score.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, ChatRequest, ChatResponse};

    #[test]
    fn test_judge_config_defaults() {
        let config = JudgeConfig::default();
        assert_eq!(config.max_retries, 3);
        assert_eq!(config.base_delay_ms, 100);
        assert_eq!(config.max_delay_ms, 5000);
        assert!(!config.verbose_logging);
    }

    #[test]
    fn test_judge_config_retry_delay() {
        let config = JudgeConfig::default();

        let delay_1 = config.calculate_retry_delay(0);
        let delay_2 = config.calculate_retry_delay(1);
        let delay_3 = config.calculate_retry_delay(2);

        // With jitter, delays might vary, but should follow exponential pattern approximately
        assert!(delay_1 <= delay_2);
        assert!(delay_2 <= delay_3);
        assert!(delay_3 <= Duration::from_millis(config.max_delay_ms + 1000)); // Allow for jitter
    }

    #[test]
    fn test_validation_criteria_defaults() {
        let criteria = ValidationCriteria::default();
        assert_eq!(criteria.min_response_length, 10);
        assert_eq!(criteria.max_response_length, 10000);
        assert!(criteria.require_factual_accuracy);
        assert!(!criteria.forbidden_keywords.is_empty());
    }

    #[test]
    fn test_validation_criteria_builders() {
        let criteria = ValidationCriteria::technical_documentation();
        assert!(criteria.min_response_length > ValidationCriteria::default().min_response_length);
        assert!(criteria.min_coherence_score > 0.8);

        let creative = ValidationCriteria::creative_writing();
        assert!(!creative.require_factual_accuracy);
        assert!(creative.min_coherence_score < criteria.min_coherence_score);
    }

    #[test]
    fn test_validation_result_display() {
        let metrics = ValidationMetrics::with_duration(Duration::from_millis(100));

        let success = ValidationResult::Success {
            message: "Test passed".to_string(),
            metrics,
        };

        let display = format!("{}", success);
        assert!(display.contains("✅ SUCCESS"));
        assert!(display.contains("Test passed"));
    }

    #[test]
    fn test_validation_result_checks() {
        let metrics = ValidationMetrics::default();

        let success = ValidationResult::Success {
            message: "OK".to_string(),
            metrics: metrics.clone(),
        };
        assert!(success.is_success());
        assert!(!success.is_warning());
        assert!(!success.is_failure());

        let warning = ValidationResult::Warning {
            message: "Slow".to_string(),
            suggestions: vec!["Optimize".to_string()],
            metrics,
        };
        assert!(warning.is_warning());
        assert!(!warning.is_success());

        let failure = ValidationResult::Failure {
            message: "Failed".to_string(),
            error_details: "Error".to_string(),
            suggestions: vec!["Fix".to_string()],
            metrics: None,
        };
        assert!(failure.is_failure());
        assert!(!failure.is_success());
    }

    #[test]
    fn test_validation_metrics() {
        let mut metrics = ValidationMetrics::with_duration(Duration::from_secs(1))
            .with_response_length(500)
            .with_coherence_score(0.85)
            .with_relevance_score(0.92);

        metrics.add_custom_metric("test_score".to_string(), 0.75);

        assert_eq!(metrics.duration, Duration::from_secs(1));
        assert_eq!(metrics.response_length, Some(500));
        assert_eq!(metrics.coherence_score, Some(0.85));
        assert_eq!(metrics.relevance_score, Some(0.92));
        assert_eq!(metrics.custom_metrics.get("test_score"), Some(&0.75));
    }

    #[test]
    fn test_coherence_score_calculation() {
        // Empty text should get 0
        assert_eq!(calculate_coherence_score(""), 0.0);

        // Simple text should get a reasonable score
        let simple_text = "This is a test. It has multiple sentences.";
        let score = calculate_coherence_score(simple_text);
        assert!(score > 0.5);
        assert!(score <= 1.0);

        // Well-structured text should get a higher score
        let structured_text = "This is a well-structured paragraph. It contains multiple sentences with good variety.\n\nThis is another paragraph. It demonstrates proper formatting and structure.";
        let structured_score = calculate_coherence_score(structured_text);
        assert!(structured_score >= score);
    }

    #[test]
    fn test_relevance_score_calculation() {
        let criteria = ValidationCriteria::default()
            .with_required_keywords(vec!["machine".to_string(), "learning".to_string()])
            .with_forbidden_keywords(vec!["error".to_string()]);

        let prompt = "Explain machine learning concepts";

        // Response with required keywords should score well
        let good_response = "Machine learning is a subset of artificial intelligence that enables computers to learn and improve from experience without being explicitly programmed.";
        let good_score = calculate_relevance_score(good_response, prompt, &criteria);
        assert!(good_score > 0.5);

        // Response with forbidden keywords should score poorly
        let bad_response = "I encountered an error and cannot explain machine learning.";
        let bad_score = calculate_relevance_score(bad_response, prompt, &criteria);
        assert!(bad_score < good_score);

        // Response missing required keywords should score lower
        let incomplete_response = "This is a response about artificial intelligence.";
        let incomplete_score = calculate_relevance_score(incomplete_response, prompt, &criteria);
        assert!(incomplete_score < good_score);
    }

    #[test]
    fn test_validation_criteria_with_methods() {
        let criteria = ValidationCriteria::default()
            .with_required_keywords(vec!["test".to_string()])
            .with_forbidden_keywords(vec!["fail".to_string()]);

        assert_eq!(criteria.required_keywords, vec!["test"]);
        assert!(criteria.forbidden_keywords.contains(&"fail".to_string()));
    }

    #[test]
    fn test_judge_config_builder_methods() {
        let config = JudgeConfig::with_retries(5, 200)
            .with_verbose_logging()
            .with_timeout(Duration::from_secs(60));

        assert_eq!(config.max_retries, 5);
        assert_eq!(config.base_delay_ms, 200);
        assert!(config.verbose_logging);
        assert_eq!(config.default_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_metrics_display() {
        let metrics = ValidationMetrics {
            duration: Duration::from_millis(500),
            retry_count: 2,
            response_length: Some(100),
            coherence_score: Some(0.85),
            relevance_score: Some(0.92),
            success_rate: Some(0.75),
            custom_metrics: HashMap::new(),
        };

        let display = format!("{}", metrics);
        assert!(display.contains("duration: 500ms"));
        assert!(display.contains("retries: 2"));
        assert!(display.contains("length: 100"));
        assert!(display.contains("coherence: 0.85"));
        assert!(display.contains("relevance: 0.92"));
        assert!(display.contains("success_rate: 75.00%"));
    }

    #[tokio::test]
    async fn test_comprehensive_validation_empty_inputs() {
        use crate::provider::ModelProvider;

        // Create a mock provider that implements both traits
        struct MockJudgeProvider {
            config: JudgeConfig,
        }

        #[async_trait]
        impl ModelProvider for MockJudgeProvider {
            async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
                Ok(ChatResponse {
                    choices: vec![crate::types::Choice {
                        message: ChatMessage::assistant("Mock response"),
                        finish_reason: Some(crate::types::FinishReason::Stop),
                    }],
                    usage: None,
                })
            }

            async fn list_models(&self) -> ModelResult<Vec<crate::types::ModelInfo>> {
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
        impl ModelJudge for MockJudgeProvider {
            fn judge_config(&self) -> &JudgeConfig {
                &self.config
            }

            async fn validate_api_responsiveness(
                &self,
                _latency_threshold: Duration,
            ) -> ModelResult<ValidationResult> {
                Ok(ValidationResult::Success {
                    message: "API responsive".to_string(),
                    metrics: ValidationMetrics::with_duration(Duration::from_millis(100)),
                })
            }

            async fn validate_response_quality(
                &self,
                _prompt: &str,
                _criteria: &ValidationCriteria,
            ) -> ModelResult<ValidationResult> {
                Ok(ValidationResult::Success {
                    message: "Quality acceptable".to_string(),
                    metrics: ValidationMetrics::with_duration(Duration::from_millis(200)),
                })
            }

            async fn validate_tool_calling(
                &self,
                _tools: &[ToolDefinition],
            ) -> ModelResult<ValidationResult> {
                Ok(ValidationResult::Success {
                    message: "Tool calling works".to_string(),
                    metrics: ValidationMetrics::with_duration(Duration::from_millis(150)),
                })
            }

            async fn validate_consistency(
                &self,
                _prompts: &[&str],
                _iterations: usize,
            ) -> ModelResult<ValidationResult> {
                Ok(ValidationResult::Success {
                    message: "Responses consistent".to_string(),
                    metrics: ValidationMetrics::with_duration(Duration::from_millis(300)),
                })
            }
        }

        let provider = MockJudgeProvider {
            config: JudgeConfig::default(),
        };

        let criteria = ValidationCriteria::default();
        let results = provider
            .validate_comprehensive(
                Duration::from_secs(5),
                &criteria,
                &[], // No tools
                &[], // No consistency prompts
                3,
            )
            .await
            .unwrap();

        // Should have 2 results: API responsiveness and response quality
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.is_success()));
    }
}
