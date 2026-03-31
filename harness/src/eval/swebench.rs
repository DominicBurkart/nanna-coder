//! SWE-bench data model for benchmark run results.
//!
//! Provides structured types for capturing SWE-bench evaluation outcomes,
//! including per-instance resolution status, token usage, and timing data.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Configuration describing a SWE-bench benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweBenchRunConfig {
    /// Git commit SHA under test
    pub commit_sha: String,
    /// Benchmark suite name, e.g. "swebench_verified"
    pub bench_name: String,
    /// Scenario identifier, e.g. "claude_code__x86_laptop"
    pub scenario: String,
    /// Model used for the run, if applicable
    pub model_name: Option<String>,
    /// When the run started
    pub timestamp: DateTime<Utc>,
}

/// Token usage counters for a single instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

/// Result of a single SWE-bench instance evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweBenchInstanceResult {
    /// Instance identifier, e.g. "django__django-16379"
    pub instance_id: String,
    /// Whether the instance was resolved (tests pass after patch)
    pub resolved: bool,
    /// Token usage from the Claude model
    pub claude_token_usage: TokenUsage,
    /// Token usage from the nanna orchestrator model, if any
    pub nanna_token_usage: Option<TokenUsage>,
    /// Wall-clock time in seconds
    pub wall_time_secs: f64,
    /// Error message if the instance failed with an error
    pub error: Option<String>,
}

/// Complete result of a SWE-bench benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweBenchRunResult {
    /// Run configuration
    pub config: SweBenchRunConfig,
    /// Per-instance results
    pub instances: Vec<SweBenchInstanceResult>,
}

impl SweBenchRunResult {
    /// Number of resolved instances.
    pub fn resolved_count(&self) -> usize {
        self.instances.iter().filter(|i| i.resolved).count()
    }

    /// Total number of instances.
    pub fn total_count(&self) -> usize {
        self.instances.len()
    }

    /// Resolve rate as a fraction in [0.0, 1.0]. Returns 0.0 for empty runs.
    pub fn resolve_rate(&self) -> f64 {
        if self.instances.is_empty() {
            return 0.0;
        }
        self.resolved_count() as f64 / self.total_count() as f64
    }

    /// Sum of all Claude token usage across instances.
    pub fn total_claude_tokens(&self) -> u64 {
        self.instances
            .iter()
            .map(|i| i.claude_token_usage.total_tokens)
            .sum()
    }

    /// Average wall-clock time across instances. Returns 0.0 for empty runs.
    pub fn avg_wall_time(&self) -> f64 {
        if self.instances.is_empty() {
            return 0.0;
        }
        let total: f64 = self.instances.iter().map(|i| i.wall_time_secs).sum();
        total / self.instances.len() as f64
    }

    /// Average Claude tokens per resolved instance. Returns 0 if none resolved.
    pub fn tokens_per_resolved(&self) -> u64 {
        let resolved = self.resolved_count();
        if resolved == 0 {
            return 0;
        }
        self.total_claude_tokens() / resolved as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_instance(
        id: &str,
        resolved: bool,
        tokens: u64,
        wall_time: f64,
    ) -> SweBenchInstanceResult {
        SweBenchInstanceResult {
            instance_id: id.to_string(),
            resolved,
            claude_token_usage: TokenUsage {
                prompt_tokens: tokens / 2,
                completion_tokens: tokens / 2,
                total_tokens: tokens,
            },
            nanna_token_usage: None,
            wall_time_secs: wall_time,
            error: if resolved {
                None
            } else {
                Some("test error".to_string())
            },
        }
    }

    fn make_run(instances: Vec<SweBenchInstanceResult>) -> SweBenchRunResult {
        SweBenchRunResult {
            config: SweBenchRunConfig {
                commit_sha: "abc123".to_string(),
                bench_name: "swebench_verified".to_string(),
                scenario: "claude_code__x86_laptop".to_string(),
                model_name: Some("claude-sonnet-4-20250514".to_string()),
                timestamp: Utc::now(),
            },
            instances,
        }
    }

    #[test]
    fn test_resolve_rate_calculation() {
        let run = make_run(vec![
            make_instance("a", true, 1000, 10.0),
            make_instance("b", false, 2000, 20.0),
            make_instance("c", true, 1500, 15.0),
            make_instance("d", true, 1200, 12.0),
        ]);

        assert_eq!(run.resolved_count(), 3);
        assert_eq!(run.total_count(), 4);
        assert!((run.resolve_rate() - 0.75).abs() < f64::EPSILON);
        assert_eq!(run.total_claude_tokens(), 5700);
        assert!((run.avg_wall_time() - 14.25).abs() < f64::EPSILON);
        assert_eq!(run.tokens_per_resolved(), 1900);
    }

    #[test]
    fn test_tokens_per_resolved_zero_resolved() {
        let run = make_run(vec![
            make_instance("a", false, 1000, 10.0),
            make_instance("b", false, 2000, 20.0),
        ]);

        assert_eq!(run.resolved_count(), 0);
        assert_eq!(run.tokens_per_resolved(), 0);
        assert!((run.resolve_rate() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_empty_run() {
        let run = make_run(vec![]);

        assert_eq!(run.resolved_count(), 0);
        assert_eq!(run.total_count(), 0);
        assert!((run.resolve_rate() - 0.0).abs() < f64::EPSILON);
        assert!((run.avg_wall_time() - 0.0).abs() < f64::EPSILON);
        assert_eq!(run.tokens_per_resolved(), 0);
    }

    #[test]
    fn test_json_roundtrip() {
        let run = make_run(vec![
            make_instance("django__django-16379", true, 5000, 45.2),
            make_instance("sympy__sympy-23824", false, 8000, 120.5),
        ]);

        let json = serde_json::to_string_pretty(&run).unwrap();
        let deserialized: SweBenchRunResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.instances.len(), 2);
        assert_eq!(
            deserialized.instances[0].instance_id,
            "django__django-16379"
        );
        assert!(deserialized.instances[0].resolved);
        assert!(!deserialized.instances[1].resolved);
        assert_eq!(deserialized.config.commit_sha, "abc123");
        assert_eq!(deserialized.config.bench_name, "swebench_verified");
        assert_eq!(deserialized.resolved_count(), 1);
        assert_eq!(deserialized.total_claude_tokens(), 13000);
    }
}
