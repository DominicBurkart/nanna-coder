//! Markdown report generation for SWE-bench evaluation results.
//!
//! Produces GitHub-compatible markdown with Mermaid visualizations
//! from structured [`SweBenchRunResult`] data.
//!
//! # Example
//!
//! ```rust,no_run
//! use harness::eval::swebench::{SweBenchRunResult, SweBenchRunConfig, TokenUsage};
//! use harness::eval::swebench_report::SweBenchReport;
//!
//! # fn example(run_result: SweBenchRunResult) {
//! let report = SweBenchReport::new("SWE-bench Run", run_result);
//! let markdown = report.render_markdown();
//! println!("{}", markdown);
//! # }
//! ```

use super::swebench::SweBenchRunResult;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// A report built from SWE-bench run results, renderable as markdown.
#[derive(Debug, Clone)]
pub struct SweBenchReport {
    /// Report title
    pub title: String,
    /// When the report was generated
    pub generated_at: chrono::DateTime<chrono::Utc>,
    /// The SWE-bench run result to report on
    pub run_result: SweBenchRunResult,
}

impl SweBenchReport {
    /// Create a new report from a SWE-bench run result.
    pub fn new(title: impl Into<String>, run_result: SweBenchRunResult) -> Self {
        Self {
            title: title.into(),
            generated_at: chrono::Utc::now(),
            run_result,
        }
    }

    /// Render the full report as GitHub-compatible markdown.
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        self.render_header(&mut out);
        self.render_summary_table(&mut out);
        self.render_resolve_pie_chart(&mut out);
        self.render_wall_time_bar_chart(&mut out);
        self.render_token_bar_chart(&mut out);
        self.render_instance_detail_table(&mut out);
        out
    }

    /// Write the single-scenario report to a directory structure:
    /// `<base_dir>/<commit_sha>/<bench_name>/<scenario>/report.md`
    pub fn write_to_directory(&self, base_dir: &Path) -> std::io::Result<PathBuf> {
        let c = &self.run_result.config;
        let dir = base_dir
            .join(&c.commit_sha)
            .join(&c.bench_name)
            .join(&c.scenario);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("report.md");
        std::fs::write(&path, self.render_markdown())?;
        Ok(path)
    }

    /// Write a comparison report to a directory structure:
    /// `<base_dir>/<commit_sha>/<bench_name>/<scenarioA>_vs_<scenarioB>/comparison.md`
    pub fn write_comparison_to_directory(
        &self,
        other: &SweBenchRunResult,
        base_dir: &Path,
    ) -> std::io::Result<PathBuf> {
        let c = &self.run_result.config;
        let dir_name = format!("{}_vs_{}", c.scenario, other.config.scenario);
        let dir = base_dir
            .join(&c.commit_sha)
            .join(&c.bench_name)
            .join(dir_name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("comparison.md");
        std::fs::write(&path, self.render_comparison(other))?;
        Ok(path)
    }

    /// Render a comparison report between this run and another.
    pub fn render_comparison(&self, other: &SweBenchRunResult) -> String {
        let mut out = String::new();
        self.render_comparison_header(&mut out, other);
        self.render_comparison_summary(&mut out, other);
        self.render_comparison_bar_chart(&mut out, other);
        self.render_differing_instances(&mut out, other);
        out
    }

    fn render_header(&self, out: &mut String) {
        let c = &self.run_result.config;
        let _ = writeln!(out, "# {}\n", self.title);
        let _ = writeln!(
            out,
            "**Generated:** {}\n",
            self.generated_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
        let _ = writeln!(out, "| Field | Value |");
        let _ = writeln!(out, "|-------|-------|");
        let _ = writeln!(out, "| Scenario | {} |", c.scenario);
        let _ = writeln!(out, "| Commit | `{}` |", c.commit_sha);
        let _ = writeln!(out, "| Benchmark | {} |", c.bench_name);
        let _ = writeln!(
            out,
            "| Timestamp | {} |",
            c.timestamp.format("%Y-%m-%d %H:%M:%S UTC")
        );
        let _ = writeln!(
            out,
            "| Model | {} |",
            c.model_name.as_deref().unwrap_or("-")
        );
        let _ = writeln!(out);
    }

    fn render_summary_table(&self, out: &mut String) {
        let r = &self.run_result;
        let _ = writeln!(out, "## Summary\n");
        let _ = writeln!(out, "| Metric | Value |");
        let _ = writeln!(out, "|--------|-------|");
        let _ = writeln!(out, "| Total instances | {} |", r.total_count());
        let _ = writeln!(out, "| Resolved | {} |", r.resolved_count());
        let _ = writeln!(out, "| Resolve rate | {:.1}% |", r.resolve_rate() * 100.0);
        let _ = writeln!(out, "| Total Claude tokens | {} |", r.total_claude_tokens());
        let _ = writeln!(out, "| Avg wall time | {:.1}s |", r.avg_wall_time());
        let _ = writeln!(out, "| Tokens per resolved | {} |", r.tokens_per_resolved());
        let _ = writeln!(out);
    }

    fn render_resolve_pie_chart(&self, out: &mut String) {
        let r = &self.run_result;
        if r.instances.is_empty() {
            return;
        }

        let resolved = r.resolved_count();
        let unresolved = r.total_count() - resolved;

        let _ = writeln!(out, "## Resolution Status\n");
        let _ = writeln!(out, "```mermaid");
        let _ = writeln!(out, "pie title Resolution Status");
        let _ = writeln!(out, "    \"Resolved\" : {}", resolved);
        let _ = writeln!(out, "    \"Unresolved\" : {}", unresolved);
        let _ = writeln!(out, "```\n");
    }

    fn render_wall_time_bar_chart(&self, out: &mut String) {
        let instances = &self.run_result.instances;
        if instances.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Wall Time per Instance\n");
        let _ = writeln!(out, "```mermaid");
        let _ = writeln!(out, "xychart-beta");
        let _ = writeln!(out, "    title \"Wall Time by Instance\"");

        let labels: Vec<String> = instances
            .iter()
            .map(|i| truncate_id(&i.instance_id))
            .collect();
        let _ = writeln!(out, "    x-axis [{}]", labels.join(", "));

        let max_time = instances
            .iter()
            .map(|i| i.wall_time_secs)
            .fold(0.0_f64, f64::max);
        let y_max = if max_time < 0.01 { 1.0 } else { max_time * 1.2 };
        let _ = writeln!(out, "    y-axis \"Time (seconds)\" 0 --> {:.1}", y_max);

        let times: Vec<String> = instances
            .iter()
            .map(|i| format!("{:.1}", i.wall_time_secs))
            .collect();
        let _ = writeln!(out, "    bar [{}]", times.join(", "));

        let _ = writeln!(out, "```\n");
    }

    fn render_token_bar_chart(&self, out: &mut String) {
        let instances = &self.run_result.instances;
        if instances.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Token Usage per Instance\n");
        let _ = writeln!(out, "```mermaid");
        let _ = writeln!(out, "xychart-beta");
        let _ = writeln!(out, "    title \"Claude Token Usage by Instance\"");

        let labels: Vec<String> = instances
            .iter()
            .map(|i| truncate_id(&i.instance_id))
            .collect();
        let _ = writeln!(out, "    x-axis [{}]", labels.join(", "));

        let max_tokens = instances
            .iter()
            .map(|i| i.claude_token_usage.total_tokens)
            .max()
            .unwrap_or(1);
        let y_max = (max_tokens as f64 * 1.2) as u64;
        let _ = writeln!(out, "    y-axis \"Tokens\" 0 --> {}", y_max);

        let tokens: Vec<String> = instances
            .iter()
            .map(|i| i.claude_token_usage.total_tokens.to_string())
            .collect();
        let _ = writeln!(out, "    bar [{}]", tokens.join(", "));

        let _ = writeln!(out, "```\n");
    }

    fn render_instance_detail_table(&self, out: &mut String) {
        let instances = &self.run_result.instances;
        if instances.is_empty() {
            return;
        }

        let _ = writeln!(out, "## Instance Details\n");
        let _ = writeln!(
            out,
            "| Instance | Resolved | Claude Tokens | Wall Time | Error |"
        );
        let _ = writeln!(
            out,
            "|----------|----------|---------------|-----------|-------|"
        );

        for i in instances {
            let status = if i.resolved { "YES" } else { "NO" };
            let error = i.error.as_deref().unwrap_or("-");
            let _ = writeln!(
                out,
                "| {} | {} | {} | {:.1}s | {} |",
                i.instance_id, status, i.claude_token_usage.total_tokens, i.wall_time_secs, error,
            );
        }
        let _ = writeln!(out);
    }

    fn render_comparison_header(&self, out: &mut String, other: &SweBenchRunResult) {
        let _ = writeln!(
            out,
            "# Comparison: {} vs {}\n",
            self.run_result.config.scenario, other.config.scenario
        );
        let _ = writeln!(
            out,
            "**Generated:** {}\n",
            self.generated_at.format("%Y-%m-%d %H:%M:%S UTC")
        );
    }

    fn render_comparison_summary(&self, out: &mut String, other: &SweBenchRunResult) {
        let a = &self.run_result;
        let b = other;

        let rate_a = a.resolve_rate() * 100.0;
        let rate_b = b.resolve_rate() * 100.0;
        let rate_delta = rate_b - rate_a;

        let tokens_a = a.total_claude_tokens();
        let tokens_b = b.total_claude_tokens();
        let tokens_delta = tokens_b as i64 - tokens_a as i64;

        let time_a = a.avg_wall_time();
        let time_b = b.avg_wall_time();
        let time_delta = time_b - time_a;

        let _ = writeln!(out, "## Side-by-Side Summary\n");
        let _ = writeln!(
            out,
            "| Metric | {} | {} | Delta |",
            a.config.scenario, b.config.scenario
        );
        let _ = writeln!(out, "|--------|------|------|-------|");
        let _ = writeln!(
            out,
            "| Total instances | {} | {} | {} |",
            a.total_count(),
            b.total_count(),
            b.total_count() as i64 - a.total_count() as i64,
        );
        let _ = writeln!(
            out,
            "| Resolved | {} | {} | {} |",
            a.resolved_count(),
            b.resolved_count(),
            b.resolved_count() as i64 - a.resolved_count() as i64,
        );
        let _ = writeln!(
            out,
            "| Resolve rate | {:.1}% | {:.1}% | {:+.1}% |",
            rate_a, rate_b, rate_delta,
        );
        let _ = writeln!(
            out,
            "| Total Claude tokens | {} | {} | {:+} |",
            tokens_a, tokens_b, tokens_delta,
        );
        let _ = writeln!(
            out,
            "| Avg wall time | {:.1}s | {:.1}s | {:+.1}s |",
            time_a, time_b, time_delta,
        );
        let _ = writeln!(out);
    }

    fn render_comparison_bar_chart(&self, out: &mut String, other: &SweBenchRunResult) {
        let a = &self.run_result;
        let b = other;

        let _ = writeln!(out, "## Comparison Chart\n");
        let _ = writeln!(out, "```mermaid");
        let _ = writeln!(out, "xychart-beta");
        let _ = writeln!(
            out,
            "    title \"Scenario Comparison: Resolve Rate and Avg Time\""
        );
        let _ = writeln!(
            out,
            "    x-axis [\"{}\", \"{}\"]",
            truncate_label(&a.config.scenario),
            truncate_label(&b.config.scenario),
        );

        let max_rate = f64::max(a.resolve_rate(), b.resolve_rate()) * 100.0;
        let y_max = if max_rate < 0.01 {
            100.0
        } else {
            (max_rate * 1.2).min(100.0)
        };
        let _ = writeln!(out, "    y-axis \"Resolve Rate (%)\" 0 --> {:.0}", y_max);

        let _ = writeln!(
            out,
            "    bar [{:.1}, {:.1}]",
            a.resolve_rate() * 100.0,
            b.resolve_rate() * 100.0,
        );

        let _ = writeln!(out, "```\n");
    }

    fn render_differing_instances(&self, out: &mut String, other: &SweBenchRunResult) {
        let a = &self.run_result;
        let b = other;

        // Build lookup maps
        let a_map: std::collections::HashMap<&str, bool> = a
            .instances
            .iter()
            .map(|i| (i.instance_id.as_str(), i.resolved))
            .collect();
        let b_map: std::collections::HashMap<&str, bool> = b
            .instances
            .iter()
            .map(|i| (i.instance_id.as_str(), i.resolved))
            .collect();

        let mut diffs: Vec<(&str, &str, &str)> = Vec::new();

        for (id, &a_resolved) in &a_map {
            if let Some(&b_resolved) = b_map.get(id) {
                if a_resolved != b_resolved {
                    let a_status = if a_resolved { "YES" } else { "NO" };
                    let b_status = if b_resolved { "YES" } else { "NO" };
                    diffs.push((id, a_status, b_status));
                }
            }
        }

        diffs.sort_by_key(|(id, _, _)| *id);

        if diffs.is_empty() {
            let _ = writeln!(out, "## Differing Instances\n");
            let _ = writeln!(out, "No instances differ in resolution status.\n");
            return;
        }

        let _ = writeln!(out, "## Differing Instances\n");
        let _ = writeln!(
            out,
            "| Instance | {} | {} |",
            a.config.scenario, b.config.scenario,
        );
        let _ = writeln!(out, "|----------|------|------|");

        for (id, a_status, b_status) in &diffs {
            let _ = writeln!(out, "| {} | {} | {} |", id, a_status, b_status);
        }
        let _ = writeln!(out);
    }
}

/// Truncate an instance ID for chart x-axis labels, wrapping in quotes for Mermaid.
fn truncate_id(id: &str) -> String {
    let label = if id.len() > 16 {
        format!("{}...", &id[..13])
    } else {
        id.to_string()
    };
    format!("\"{}\"", label)
}

/// Truncate a scenario label for chart axes.
fn truncate_label(label: &str) -> String {
    if label.len() > 24 {
        format!("{}...", &label[..21])
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::swebench::*;

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

    fn make_run(scenario: &str, instances: Vec<SweBenchInstanceResult>) -> SweBenchRunResult {
        SweBenchRunResult {
            config: SweBenchRunConfig {
                commit_sha: "abc123".to_string(),
                bench_name: "swebench_verified".to_string(),
                scenario: scenario.to_string(),
                model_name: Some("claude-sonnet-4-20250514".to_string()),
                timestamp: chrono::Utc::now(),
            },
            instances,
        }
    }

    #[test]
    fn test_single_scenario_report() {
        let run = make_run(
            "claude_code__x86",
            vec![
                make_instance("django__django-16379", true, 5000, 45.2),
                make_instance("sympy__sympy-23824", false, 8000, 120.5),
                make_instance("flask__flask-4992", true, 3000, 30.0),
            ],
        );

        let report = SweBenchReport::new("SWE-bench Test Run", run);
        let md = report.render_markdown();

        // Header sections
        assert!(md.contains("# SWE-bench Test Run"));
        assert!(md.contains("claude_code__x86"));
        assert!(md.contains("`abc123`"));
        assert!(md.contains("swebench_verified"));
        assert!(md.contains("claude-sonnet-4-20250514"));

        // Summary table
        assert!(md.contains("## Summary"));
        assert!(md.contains("| Total instances | 3 |"));
        assert!(md.contains("| Resolved | 2 |"));
        assert!(md.contains("66.7%"));

        // Mermaid charts
        assert!(md.contains("```mermaid"));
        assert!(md.contains("pie title Resolution Status"));
        assert!(md.contains("xychart-beta"));

        // Instance detail table
        assert!(md.contains("## Instance Details"));
        assert!(md.contains("django__django-16379"));
        assert!(md.contains("YES"));
        assert!(md.contains("NO"));
    }

    #[test]
    fn test_comparison_report() {
        let run_a = make_run(
            "scenario_a",
            vec![
                make_instance("django__django-16379", true, 5000, 45.2),
                make_instance("sympy__sympy-23824", false, 8000, 120.5),
            ],
        );
        let run_b = make_run(
            "scenario_b",
            vec![
                make_instance("django__django-16379", false, 6000, 50.0),
                make_instance("sympy__sympy-23824", true, 7000, 100.0),
            ],
        );

        let report = SweBenchReport::new("Comparison", run_a);
        let md = report.render_comparison(&run_b);

        // Comparison header
        assert!(md.contains("Comparison: scenario_a vs scenario_b"));

        // Side-by-side summary
        assert!(md.contains("## Side-by-Side Summary"));
        assert!(md.contains("scenario_a"));
        assert!(md.contains("scenario_b"));
        assert!(md.contains("Delta"));

        // Grouped bar chart
        assert!(md.contains("```mermaid"));
        assert!(md.contains("xychart-beta"));

        // Differing instances
        assert!(md.contains("## Differing Instances"));
        assert!(md.contains("django__django-16379"));
        assert!(md.contains("sympy__sympy-23824"));
    }

    #[test]
    fn test_empty_results() {
        let run = make_run("empty_scenario", vec![]);
        let report = SweBenchReport::new("Empty Run", run);
        let md = report.render_markdown();

        assert!(md.contains("# Empty Run"));
        assert!(md.contains("## Summary"));
        assert!(md.contains("| Total instances | 0 |"));
        assert!(md.contains("| Resolve rate | 0.0% |"));
        // No mermaid charts for empty results
        assert!(!md.contains("```mermaid"));
        assert!(!md.contains("## Instance Details"));
    }

    #[test]
    fn test_mermaid_fencing() {
        let run = make_run(
            "fence_test",
            vec![
                make_instance("a", true, 1000, 10.0),
                make_instance("b", false, 2000, 20.0),
            ],
        );

        let report = SweBenchReport::new("Fencing Test", run);
        let md = report.render_markdown();

        let mermaid_opens = md.matches("```mermaid").count();
        let total_closes = md.matches("```\n").count();

        assert!(
            mermaid_opens >= 3,
            "Expected at least 3 mermaid blocks (pie + wall time + tokens), got {}",
            mermaid_opens,
        );
        assert!(
            total_closes >= mermaid_opens,
            "Mermaid blocks not properly closed: {} opens vs {} closes",
            mermaid_opens,
            total_closes,
        );
    }

    #[test]
    fn test_all_resolved() {
        let run = make_run(
            "all_pass",
            vec![
                make_instance("a", true, 1000, 10.0),
                make_instance("b", true, 2000, 20.0),
                make_instance("c", true, 3000, 30.0),
            ],
        );

        let report = SweBenchReport::new("All Resolved", run);
        let md = report.render_markdown();

        assert!(md.contains("| Resolve rate | 100.0% |"));
        assert!(md.contains("\"Resolved\" : 3"));
        assert!(md.contains("\"Unresolved\" : 0"));
    }

    #[test]
    fn test_none_resolved() {
        let run = make_run(
            "none_pass",
            vec![
                make_instance("a", false, 1000, 10.0),
                make_instance("b", false, 2000, 20.0),
            ],
        );

        let report = SweBenchReport::new("None Resolved", run);
        let md = report.render_markdown();

        assert!(md.contains("| Resolve rate | 0.0% |"));
        assert!(md.contains("\"Resolved\" : 0"));
        assert!(md.contains("\"Unresolved\" : 2"));
        assert!(md.contains("| Tokens per resolved | 0 |"));
    }

    #[test]
    fn test_comparison_no_diffs() {
        let run_a = make_run(
            "same_a",
            vec![
                make_instance("x", true, 1000, 10.0),
                make_instance("y", false, 2000, 20.0),
            ],
        );
        let run_b = make_run(
            "same_b",
            vec![
                make_instance("x", true, 1500, 12.0),
                make_instance("y", false, 2500, 22.0),
            ],
        );

        let report = SweBenchReport::new("No Diffs", run_a);
        let md = report.render_comparison(&run_b);

        assert!(md.contains("No instances differ in resolution status."));
    }

    #[test]
    fn test_truncate_id() {
        assert_eq!(truncate_id("short"), "\"short\"");
        assert_eq!(
            truncate_id("very_long_instance_identifier"),
            "\"very_long_ins...\""
        );
    }

    #[test]
    fn test_write_to_directory() {
        let run = make_run(
            "claude_code__x86",
            vec![
                make_instance("django__django-16379", true, 5000, 45.2),
                make_instance("sympy__sympy-23824", false, 8000, 120.5),
            ],
        );
        let report = SweBenchReport::new("Write Test", run);
        let tmp = tempfile::tempdir().unwrap();

        let path = report.write_to_directory(tmp.path()).unwrap();

        assert!(path.exists());
        assert!(path.ends_with("abc123/swebench_verified/claude_code__x86/report.md"));

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Write Test"));
        assert!(content.contains("django__django-16379"));
    }

    #[test]
    fn test_write_comparison_to_directory() {
        let run_a = make_run(
            "scenario_a",
            vec![
                make_instance("django__django-16379", true, 5000, 45.2),
                make_instance("sympy__sympy-23824", false, 8000, 120.5),
            ],
        );
        let run_b = make_run(
            "scenario_b",
            vec![
                make_instance("django__django-16379", false, 6000, 50.0),
                make_instance("sympy__sympy-23824", true, 7000, 100.0),
            ],
        );

        let report = SweBenchReport::new("Comparison Write Test", run_a);
        let tmp = tempfile::tempdir().unwrap();

        let path = report
            .write_comparison_to_directory(&run_b, tmp.path())
            .unwrap();

        assert!(path.exists());
        assert!(path.ends_with("abc123/swebench_verified/scenario_a_vs_scenario_b/comparison.md"));

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Comparison: scenario_a vs scenario_b"));
        assert!(content.contains("django__django-16379"));
    }
}
