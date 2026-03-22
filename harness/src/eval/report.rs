//! Markdown report generation for evaluation results.
//!
//! Produces GitHub-compatible markdown with Mermaid visualizations
//! from structured [`BatchEvaluationResult`] data.
//!
//! # Example
//!
//! ```rust,no_run
//! use harness::eval::report::EvalReport;
//! use harness::eval::BatchEvaluationResult;
//!
//! # fn example(batch: BatchEvaluationResult) {
//! let report = EvalReport::new("Nightly Eval Run", batch);
//! let markdown = report.render_markdown();
//! println!("{}", markdown);
//! # }
//! ```

use crate::agent::eval::{AgentEvaluationResult, BatchEvaluationResult, EvaluationMetrics};
use crate::agent::AgentState;
use std::fmt::Write;

/// A report built from batch evaluation results, renderable as markdown.
#[derive(Debug, Clone)]
pub struct EvalReport {
    /// Report title
    pub title: String,
    /// When the report was generated
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// The evaluation results to report on
    pub batch_result: BatchEvaluationResult,
}

impl EvalReport {
    /// Create a new report from batch evaluation results.
    pub fn new(title: impl Into<String>, batch_result: BatchEvaluationResult) -> Self {
        Self {
            title: title.into(),
            generated_at: chrono::Utc::now(),
            batch_result,
        }
    }

    /// Render the full report as GitHub-compatible markdown.
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        self.render_header(&mut out);
        self.render_summary_table(&mut out);
        self.render_metrics_bar_chart(&mut out);
        self.render_time_histogram(&mut out);
        self.render_quantile_table(&mut out);
        self.render_state_diagram(&mut out);
        self.render_failures(&mut out);
        out
    }

    fn render_header(&self, out: &mut String) {
        let b = &self.batch_result;
        let _ = writeln!(out, "# {}\n", self.title);
        let _ = writeln!(
            out,
            "**Generated:** {}\n",
            self.generated_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        let _ = writeln!(
            out,
            "**Result:** {}/{} passed ({} failed) in {:.2}s\n",
            b.passed,
            b.total_scenarios,
            b.failed,
            b.total_time.as_secs_f64()
        );
    }

    fn render_summary_table(&self, out: &mut String) {
        if self.batch_result.results.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Summary\n");
        let _ = writeln!(
            out,
            "| Scenario | Category | Result | Duration | Decision Quality | RAG Relevance | Entity Accuracy |"
        );
        let _ = writeln!(
            out,
            "|----------|----------|--------|----------|-----------------|---------------|-----------------|"
        );

        for r in &self.batch_result.results {
            let status = if r.success { "PASS" } else { "FAIL" };
            let _ = writeln!(
                out,
                "| {} | {} | {} | {:.2}s | {:.2} | {:.2} | {:.2} |",
                r.scenario_id,
                category_name(&r.metrics),
                status,
                r.metrics.execution_time.as_secs_f64(),
                r.metrics.decision_quality,
                r.metrics.rag_relevance,
                r.metrics.entity_accuracy,
            );
        }
        let _ = writeln!(out);
    }

    fn render_metrics_bar_chart(&self, out: &mut String) {
        let results = &self.batch_result.results;
        if results.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Metric Scores\n");
        let _ = writeln!(out, "```mermaid");
        let _ = writeln!(out, "xychart-beta");
        let _ = writeln!(out, "    title \"Metric Scores by Scenario\"");

        // X-axis: scenario IDs (truncated for readability)
        let labels: Vec<String> = results
            .iter()
            .map(|r| truncate_id(&r.scenario_id))
            .collect();
        let _ = writeln!(out, "    x-axis [{}]", labels.join(", "));
        let _ = writeln!(out, "    y-axis \"Score (0-1)\" 0 --> 1");

        // Bars for each metric
        let dq: Vec<String> = results
            .iter()
            .map(|r| format!("{:.2}", r.metrics.decision_quality))
            .collect();
        let _ = writeln!(out, "    bar [{}]", dq.join(", "));

        let rag: Vec<String> = results
            .iter()
            .map(|r| format!("{:.2}", r.metrics.rag_relevance))
            .collect();
        let _ = writeln!(out, "    bar [{}]", rag.join(", "));

        let ea: Vec<String> = results
            .iter()
            .map(|r| format!("{:.2}", r.metrics.entity_accuracy))
            .collect();
        let _ = writeln!(out, "    bar [{}]", ea.join(", "));

        let _ = writeln!(out, "```\n");
    }

    fn render_time_histogram(&self, out: &mut String) {
        let results = &self.batch_result.results;
        if results.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Execution Times\n");
        let _ = writeln!(out, "```mermaid");
        let _ = writeln!(out, "xychart-beta");
        let _ = writeln!(out, "    title \"Execution Time by Scenario\"");

        let labels: Vec<String> = results
            .iter()
            .map(|r| truncate_id(&r.scenario_id))
            .collect();
        let _ = writeln!(out, "    x-axis [{}]", labels.join(", "));

        let max_time = results
            .iter()
            .map(|r| r.metrics.execution_time.as_secs_f64())
            .fold(0.0_f64, f64::max);
        let y_max = if max_time < 0.01 { 1.0 } else { max_time * 1.2 };
        let _ = writeln!(out, "    y-axis \"Time (seconds)\" 0 --> {:.1}", y_max);

        let times: Vec<String> = results
            .iter()
            .map(|r| format!("{:.2}", r.metrics.execution_time.as_secs_f64()))
            .collect();
        let _ = writeln!(out, "    bar [{}]", times.join(", "));

        let _ = writeln!(out, "```\n");
    }

    fn render_quantile_table(&self, out: &mut String) {
        let results = &self.batch_result.results;
        if results.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Quantiles\n");
        let _ = writeln!(out, "| Metric | P50 | P90 | P95 | P99 |");
        let _ = writeln!(out, "|--------|-----|-----|-----|-----|");

        let mut times: Vec<f64> = results
            .iter()
            .map(|r| r.metrics.execution_time.as_secs_f64())
            .collect();
        times.sort_by(|a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let _ = writeln!(
            out,
            "| Execution Time (s) | {:.2} | {:.2} | {:.2} | {:.2} |",
            calculate_quantile(&times, 0.50),
            calculate_quantile(&times, 0.90),
            calculate_quantile(&times, 0.95),
            calculate_quantile(&times, 0.99),
        );

        let mut dq: Vec<f64> = results.iter().map(|r| r.metrics.decision_quality).collect();
        dq.sort_by(|a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let _ = writeln!(
            out,
            "| Decision Quality | {:.2} | {:.2} | {:.2} | {:.2} |",
            calculate_quantile(&dq, 0.50),
            calculate_quantile(&dq, 0.90),
            calculate_quantile(&dq, 0.95),
            calculate_quantile(&dq, 0.99),
        );

        let mut rag: Vec<f64> = results.iter().map(|r| r.metrics.rag_relevance).collect();
        rag.sort_by(|a: &f64, b: &f64| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let _ = writeln!(
            out,
            "| RAG Relevance | {:.2} | {:.2} | {:.2} | {:.2} |",
            calculate_quantile(&rag, 0.50),
            calculate_quantile(&rag, 0.90),
            calculate_quantile(&rag, 0.95),
            calculate_quantile(&rag, 0.99),
        );
        let _ = writeln!(out);
    }

    fn render_state_diagram(&self, out: &mut String) {
        let results = &self.batch_result.results;

        // Collect all observed transitions across scenarios
        let mut transitions: Vec<(String, String)> = Vec::new();
        for r in results {
            let states = &r.metrics.state_transitions;
            for window in states.windows(2) {
                let from = state_name(&window[0]).to_string();
                let to = state_name(&window[1]).to_string();
                if !transitions.contains(&(from.clone(), to.clone())) {
                    transitions.push((from, to));
                }
            }
        }

        if transitions.is_empty() {
            return;
        }

        let _ = writeln!(out, "## State Transitions\n");
        let _ = writeln!(out, "```mermaid");
        let _ = writeln!(out, "stateDiagram-v2");
        for (from, to) in &transitions {
            let _ = writeln!(out, "    {} --> {}", from, to);
        }
        let _ = writeln!(out, "```\n");
    }

    fn render_failures(&self, out: &mut String) {
        let failed: Vec<&AgentEvaluationResult> = self
            .batch_result
            .results
            .iter()
            .filter(|r| !r.success)
            .collect();

        if failed.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Failures\n");
        for r in failed {
            let _ = writeln!(out, "### `{}`\n", r.scenario_id);
            for f in &r.failures {
                let _ = writeln!(out, "- {}", f);
            }
            if !r.warnings.is_empty() {
                let _ = writeln!(out, "\n**Warnings:**");
                for w in &r.warnings {
                    let _ = writeln!(out, "- {}", w);
                }
            }
            let _ = writeln!(out);
        }
    }
}

/// Map an `AgentState` to a clean label for Mermaid diagrams.
fn state_name(state: &AgentState) -> &str {
    match state {
        AgentState::Planning => "Planning",
        AgentState::Querying => "Querying",
        AgentState::Deciding => "Deciding",
        AgentState::Performing => "Performing",
        AgentState::CheckingCompletion => "CheckingCompletion",
        AgentState::Completed => "Completed",
        AgentState::Error(_) => "Error",
    }
}

/// Extract a category label from metrics. Falls back to examining custom_metrics keys.
fn category_name(_metrics: &EvaluationMetrics) -> String {
    // We don't have direct access to the scenario category from the result,
    // so we infer from what's available. The scenario_id often encodes the category.
    // For a clean table, we just show "-" and let the scenario ID speak for itself.
    "-".to_string()
}

/// Truncate a scenario ID for chart x-axis labels, wrapping in quotes for Mermaid.
fn truncate_id(id: &str) -> String {
    let label = if id.len() > 16 {
        format!("{}...", &id[..13])
    } else {
        id.to_string()
    };
    format!("\"{}\"", label)
}

/// Calculate a quantile from a sorted slice of values.
/// Uses linear interpolation between adjacent values.
fn calculate_quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lower = pos.floor() as usize;
    let upper = pos.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let frac = pos - lower as f64;
        sorted[lower] * (1.0 - frac) + sorted[upper] * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::eval::EvaluationMetrics;
    use crate::agent::AgentState;
    use std::collections::HashMap;
    use std::time::Duration;

    #[allow(clippy::too_many_arguments)]
    fn make_result(
        id: &str,
        success: bool,
        exec_time: Duration,
        decision_quality: f64,
        rag_relevance: f64,
        entity_accuracy: f64,
        states: Vec<AgentState>,
        failures: Vec<String>,
    ) -> AgentEvaluationResult {
        AgentEvaluationResult {
            scenario_id: id.to_string(),
            success,
            metrics: EvaluationMetrics {
                execution_time: exec_time,
                iterations_executed: 5,
                decision_quality,
                rag_relevance,
                entity_accuracy,
                prompt_effectiveness: 0.8,
                entities_created: 2,
                relationships_created: 1,
                state_transitions: states,
                validation_results: vec![],
                custom_metrics: HashMap::new(),
            },
            final_state: if success {
                AgentState::Completed
            } else {
                AgentState::Error("test failure".to_string())
            },
            failures,
            warnings: vec![],
            system_metrics: None,
            timestamp: chrono::Utc::now(),
        }
    }

    fn make_batch(results: Vec<AgentEvaluationResult>) -> BatchEvaluationResult {
        let passed = results.iter().filter(|r| r.success).count();
        let failed = results.len() - passed;
        BatchEvaluationResult {
            total_scenarios: results.len(),
            passed,
            failed,
            total_time: Duration::from_secs(10),
            results,
        }
    }

    #[test]
    fn test_happy_path_mixed_results() {
        let batch = make_batch(vec![
            make_result(
                "entity_creation",
                true,
                Duration::from_millis(1500),
                0.9,
                0.85,
                1.0,
                vec![
                    AgentState::Planning,
                    AgentState::Performing,
                    AgentState::Completed,
                ],
                vec![],
            ),
            make_result(
                "rag_retrieval",
                true,
                Duration::from_millis(2200),
                0.75,
                0.92,
                0.8,
                vec![
                    AgentState::Planning,
                    AgentState::Querying,
                    AgentState::Completed,
                ],
                vec![],
            ),
            make_result(
                "decision_quality",
                false,
                Duration::from_millis(5000),
                0.4,
                0.6,
                0.5,
                vec![
                    AgentState::Planning,
                    AgentState::Deciding,
                    AgentState::Error("timeout".to_string()),
                ],
                vec!["Decision quality too low".to_string()],
            ),
        ]);

        let report = EvalReport::new("Test Run", batch);
        let md = report.render_markdown();

        assert!(md.contains("# Test Run"));
        assert!(md.contains("2/3 passed"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("entity_creation"));
        assert!(md.contains("PASS"));
        assert!(md.contains("FAIL"));
        assert!(md.contains("```mermaid"));
        assert!(md.contains("xychart-beta"));
        assert!(md.contains("stateDiagram-v2"));
        assert!(md.contains("## Quantiles"));
        assert!(md.contains("## Failures"));
        assert!(md.contains("Decision quality too low"));
    }

    #[test]
    fn test_single_scenario() {
        let batch = make_batch(vec![make_result(
            "solo",
            true,
            Duration::from_millis(800),
            0.95,
            0.9,
            1.0,
            vec![AgentState::Planning, AgentState::Completed],
            vec![],
        )]);

        let report = EvalReport::new("Single", batch);
        let md = report.render_markdown();

        assert!(md.contains("1/1 passed"));
        assert!(md.contains("solo"));
        assert!(!md.contains("## Failures"));
    }

    #[test]
    fn test_all_failures() {
        let batch = make_batch(vec![
            make_result(
                "fail_a",
                false,
                Duration::from_secs(3),
                0.2,
                0.3,
                0.1,
                vec![AgentState::Planning],
                vec!["Error A".to_string()],
            ),
            make_result(
                "fail_b",
                false,
                Duration::from_secs(4),
                0.1,
                0.2,
                0.0,
                vec![AgentState::Planning],
                vec!["Error B".to_string()],
            ),
        ]);

        let report = EvalReport::new("All Fail", batch);
        let md = report.render_markdown();

        assert!(md.contains("0/2 passed"));
        assert!(md.contains("## Failures"));
        assert!(md.contains("Error A"));
        assert!(md.contains("Error B"));
    }

    #[test]
    fn test_all_passes() {
        let batch = make_batch(vec![
            make_result(
                "pass_a",
                true,
                Duration::from_secs(1),
                0.9,
                0.9,
                1.0,
                vec![],
                vec![],
            ),
            make_result(
                "pass_b",
                true,
                Duration::from_secs(2),
                0.8,
                0.8,
                0.9,
                vec![],
                vec![],
            ),
        ]);

        let report = EvalReport::new("All Pass", batch);
        let md = report.render_markdown();

        assert!(md.contains("2/2 passed"));
        assert!(!md.contains("## Failures"));
    }

    #[test]
    fn test_empty_batch() {
        let batch = BatchEvaluationResult {
            total_scenarios: 0,
            passed: 0,
            failed: 0,
            total_time: Duration::ZERO,
            results: vec![],
        };

        let report = EvalReport::new("Empty", batch);
        let md = report.render_markdown();

        assert!(md.contains("# Empty"));
        assert!(md.contains("0/0 passed"));
        // No tables or charts for empty batch
        assert!(!md.contains("## Summary"));
        assert!(!md.contains("```mermaid"));
    }

    #[test]
    fn test_zero_duration() {
        let batch = make_batch(vec![make_result(
            "zero_time",
            true,
            Duration::ZERO,
            1.0,
            1.0,
            1.0,
            vec![AgentState::Planning, AgentState::Completed],
            vec![],
        )]);

        let report = EvalReport::new("Zero Duration", batch);
        let md = report.render_markdown();

        // Should render without panic
        assert!(md.contains("0.00s"));
        assert!(md.contains("```mermaid"));
    }

    #[test]
    fn test_mermaid_fencing() {
        let batch = make_batch(vec![make_result(
            "test",
            true,
            Duration::from_secs(1),
            0.8,
            0.9,
            1.0,
            vec![AgentState::Planning, AgentState::Completed],
            vec![],
        )]);

        let report = EvalReport::new("Fencing Test", batch);
        let md = report.render_markdown();

        // Count mermaid open/close pairs
        let mermaid_opens = md.matches("```mermaid").count();
        let total_closes = md.matches("```\n").count();
        // Each mermaid block has one open and one close
        assert!(
            mermaid_opens >= 2,
            "Expected at least 2 mermaid blocks (bar chart + time histogram), got {}",
            mermaid_opens
        );
        assert!(
            total_closes >= mermaid_opens,
            "Mermaid blocks not properly closed"
        );
    }

    #[test]
    fn test_quantile_calculation() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert!((calculate_quantile(&values, 0.0) - 1.0).abs() < f64::EPSILON);
        assert!((calculate_quantile(&values, 0.5) - 3.0).abs() < f64::EPSILON);
        assert!((calculate_quantile(&values, 1.0) - 5.0).abs() < f64::EPSILON);
        assert!((calculate_quantile(&values, 0.25) - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_quantile_single_value() {
        let values = vec![42.0];
        assert!((calculate_quantile(&values, 0.0) - 42.0).abs() < f64::EPSILON);
        assert!((calculate_quantile(&values, 0.5) - 42.0).abs() < f64::EPSILON);
        assert!((calculate_quantile(&values, 1.0) - 42.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_quantile_empty() {
        let values: Vec<f64> = vec![];
        assert!((calculate_quantile(&values, 0.5) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_state_name_mapping() {
        assert_eq!(state_name(&AgentState::Planning), "Planning");
        assert_eq!(state_name(&AgentState::Querying), "Querying");
        assert_eq!(state_name(&AgentState::Deciding), "Deciding");
        assert_eq!(state_name(&AgentState::Performing), "Performing");
        assert_eq!(
            state_name(&AgentState::CheckingCompletion),
            "CheckingCompletion"
        );
        assert_eq!(state_name(&AgentState::Completed), "Completed");
        assert_eq!(state_name(&AgentState::Error("oops".to_string())), "Error");
    }

    #[test]
    fn test_truncate_id() {
        assert_eq!(truncate_id("short"), "\"short\"");
        assert_eq!(
            truncate_id("very_long_scenario_identifier"),
            "\"very_long_sce...\""
        );
    }

    #[test]
    fn test_state_diagram_deduplicates() {
        let batch = make_batch(vec![
            make_result(
                "a",
                true,
                Duration::from_secs(1),
                0.9,
                0.9,
                1.0,
                vec![
                    AgentState::Planning,
                    AgentState::Performing,
                    AgentState::Completed,
                ],
                vec![],
            ),
            make_result(
                "b",
                true,
                Duration::from_secs(1),
                0.9,
                0.9,
                1.0,
                // Same transitions as "a"
                vec![
                    AgentState::Planning,
                    AgentState::Performing,
                    AgentState::Completed,
                ],
                vec![],
            ),
        ]);

        let report = EvalReport::new("Dedup", batch);
        let md = report.render_markdown();

        // Each unique transition should appear exactly once
        let planning_to_performing = md.matches("Planning --> Performing").count();
        assert_eq!(
            planning_to_performing, 1,
            "Duplicate transitions in state diagram"
        );
    }
}
