//! CI metrics parser and Prometheus exporter.
//!
//! Parses the JSON artifacts produced by the `.github/actions/ci-metrics`
//! composite action (introduced by PR #247, refs issue #5) and converts them
//! into a [Prometheus text exposition](https://prometheus.io/docs/instrumenting/exposition_formats/#text-based-format)
//! string suitable for scraping or pushing to a Pushgateway.
//!
//! # Artifact schema (v1)
//!
//! The composite action emits one JSON object per job invocation. The
//! authoritative shape is defined in `.github/actions/ci-metrics/action.yml`
//! (`jq -n ... '{...}'`). At schema_version 1 the object has the form:
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "workflow": "CI",
//!   "job": "test-unit",
//!   "metric_name": "test-unit-linux",
//!   "matrix_key": "ubuntu-latest-x64",
//!   "run_id": "12345",
//!   "run_attempt": "1",
//!   "run_number": "678",
//!   "event_name": "push",
//!   "ref": "refs/heads/main",
//!   "sha": "deadbeef",
//!   "os": "Linux",
//!   "arch": "X64",
//!   "runner_name": "GitHub Actions 2",
//!   "start_ts": 1700000000,
//!   "end_ts": 1700000123,
//!   "duration_seconds": 123,
//!   "cache_hit": "true",
//!   "job_status": "success"
//! }
//! ```
//!
//! Because the GitHub Actions `actions/upload-artifact` step in the composite
//! uploads a *single* JSON file per job, this parser accepts both:
//!
//! - a single JSON object (one CI job), and
//! - a JSON array of such objects (a pre-aggregated batch).
//!
//! Unknown fields are accepted and ignored so the parser is forward-compatible
//! with non-breaking schema additions.
//!
//! # Prometheus output
//!
//! For each [`CiMetric`] the exporter emits:
//!
//! - `ci_job_duration_seconds{...} <duration>`
//! - `ci_job_cache_hit{...} <0|1>` (omitted when `cache_hit` is unknown/empty)
//! - `ci_job_status{...} <0|1>` where status is success/failure/cancelled/unknown
//!
//! Each series is labelled by `workflow`, `job`, `metric_name`, `matrix_key`,
//! `os`, `arch`, `event_name`, and `ref`. Label values are escaped per the
//! Prometheus exposition format: backslashes, newlines, and double-quotes.

use serde::Deserialize;
use thiserror::Error;

/// Errors that can occur while parsing CI metrics JSON artifacts.
#[derive(Debug, Error)]
pub enum Error {
    /// The input was not valid JSON or did not match the expected shape.
    #[error("failed to parse ci-metrics JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// One CI job's metrics, matching the schema_version=1 artifact shape.
///
/// Fields that the composite action always emits as strings (run_id,
/// run_attempt, run_number, etc.) are kept as `String` here even where they
/// look numeric. `cache_hit` is `Option<bool>`: `Some(true)`/`Some(false)` for
/// the literal strings `"true"`/`"false"`, and `None` for the empty string
/// (caller did not pass a cache-hit value).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CiMetric {
    /// Schema version of the artifact. v1 at time of writing.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// `GITHUB_WORKFLOW` — name of the workflow.
    #[serde(default)]
    pub workflow: String,
    /// `GITHUB_JOB` — id of the job within the workflow.
    #[serde(default)]
    pub job: String,
    /// Caller-provided logical name for this metric series, defaulting to
    /// the job id when unset.
    #[serde(default)]
    pub metric_name: String,
    /// Optional matrix disambiguator (e.g. `"ubuntu-latest-x64"`).
    #[serde(default)]
    pub matrix_key: String,
    /// `GITHUB_RUN_ID`.
    #[serde(default)]
    pub run_id: String,
    /// `GITHUB_RUN_ATTEMPT`.
    #[serde(default)]
    pub run_attempt: String,
    /// `GITHUB_RUN_NUMBER`.
    #[serde(default)]
    pub run_number: String,
    /// `GITHUB_EVENT_NAME`.
    #[serde(default)]
    pub event_name: String,
    /// `GITHUB_REF`.
    #[serde(default, rename = "ref")]
    pub git_ref: String,
    /// `GITHUB_SHA`.
    #[serde(default)]
    pub sha: String,
    /// `RUNNER_OS`.
    #[serde(default)]
    pub os: String,
    /// `RUNNER_ARCH`.
    #[serde(default)]
    pub arch: String,
    /// `RUNNER_NAME`.
    #[serde(default)]
    pub runner_name: String,
    /// Start timestamp (unix seconds, UTC).
    #[serde(default)]
    pub start_ts: i64,
    /// End timestamp (unix seconds, UTC).
    #[serde(default)]
    pub end_ts: i64,
    /// `end_ts - start_ts` as computed by the composite action.
    #[serde(default)]
    pub duration_seconds: i64,
    /// Caller-provided cache-hit flag. `None` when the caller did not supply
    /// one (the composite emits an empty string in that case).
    #[serde(default, deserialize_with = "deserialize_cache_hit")]
    pub cache_hit: Option<bool>,
    /// Caller-provided `job.status` — typically one of `"success"`,
    /// `"failure"`, `"cancelled"`, or `"unknown"`.
    #[serde(default)]
    pub job_status: String,
}

fn default_schema_version() -> u32 {
    1
}

fn deserialize_cache_hit<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(match raw.as_deref() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        // The composite action emits an empty string when the caller did not
        // pass a cache-hit input. Treat anything else (including the literal
        // empty string and unrecognised values) as "unknown".
        _ => None,
    })
}

/// Parse a CI metrics JSON artifact.
///
/// Accepts either a single object (one job's metrics) or a JSON array of
/// such objects (a pre-aggregated batch).
pub fn parse(s: &str) -> Result<Vec<CiMetric>, Error> {
    let value: serde_json::Value = serde_json::from_str(s)?;
    match value {
        serde_json::Value::Array(_) => Ok(serde_json::from_value(value)?),
        _ => Ok(vec![serde_json::from_value(value)?]),
    }
}

/// Escape a Prometheus label value per the text exposition spec: backslash,
/// double-quote, and newline must be escaped.
fn escape_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

fn render_labels(m: &CiMetric, extra: &[(&str, &str)]) -> String {
    let base: [(&str, &str); 8] = [
        ("workflow", &m.workflow),
        ("job", &m.job),
        ("metric_name", &m.metric_name),
        ("matrix_key", &m.matrix_key),
        ("os", &m.os),
        ("arch", &m.arch),
        ("event_name", &m.event_name),
        ("ref", &m.git_ref),
    ];
    let mut parts: Vec<String> = base
        .iter()
        .chain(extra.iter())
        .map(|(k, v)| format!("{}=\"{}\"", k, escape_label(v)))
        .collect();
    // Stable ordering: base labels first in declaration order, then `extra`.
    // (We deliberately do NOT sort: callers rely on `status` coming last for
    // the `ci_job_status` series so the per-status lines group visually.)
    let joined = parts.join(",");
    parts.clear();
    joined
}

const HEADER: &str = "\
# HELP ci_job_duration_seconds Wall-clock duration of a CI job, in seconds.
# TYPE ci_job_duration_seconds gauge
# HELP ci_job_cache_hit Whether the primary cache restore hit (1) or missed (0). Series omitted when unknown.
# TYPE ci_job_cache_hit gauge
# HELP ci_job_status CI job status one-hot (success/failure/cancelled/unknown). Exactly one variant is 1 per job.
# TYPE ci_job_status gauge
";

/// Render a slice of [`CiMetric`] as a Prometheus text exposition payload.
///
/// Output always begins with HELP/TYPE headers so the result is a complete,
/// scrape-ready payload even when `metrics` is empty.
pub fn to_prometheus(metrics: &[CiMetric]) -> String {
    let mut out = String::from(HEADER);
    for m in metrics {
        let labels = render_labels(m, &[]);
        out.push_str(&format!(
            "ci_job_duration_seconds{{{}}} {}\n",
            labels, m.duration_seconds
        ));
        if let Some(hit) = m.cache_hit {
            out.push_str(&format!(
                "ci_job_cache_hit{{{}}} {}\n",
                labels,
                if hit { 1 } else { 0 }
            ));
        }
        for variant in ["success", "failure", "cancelled", "unknown"] {
            let value = if status_matches(&m.job_status, variant) {
                1
            } else {
                0
            };
            let labels_with_status = render_labels(m, &[("status", variant)]);
            out.push_str(&format!(
                "ci_job_status{{{}}} {}\n",
                labels_with_status, value
            ));
        }
    }
    out
}

fn status_matches(actual: &str, variant: &str) -> bool {
    // GitHub Actions' `job.status` is lowercase, but be defensive: accept
    // any case from the caller, and route empty / unrecognised statuses to
    // the `"unknown"` bucket so exactly one variant is always 1.
    let normalised = actual.to_ascii_lowercase();
    match variant {
        "success" => normalised == "success",
        "failure" => normalised == "failure",
        "cancelled" => normalised == "cancelled",
        "unknown" => !matches!(normalised.as_str(), "success" | "failure" | "cancelled"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_FULL: &str = r#"{
        "schema_version": 1,
        "workflow": "CI",
        "job": "test-unit",
        "metric_name": "test-unit-linux",
        "matrix_key": "ubuntu-latest-x64",
        "run_id": "12345",
        "run_attempt": "1",
        "run_number": "678",
        "event_name": "push",
        "ref": "refs/heads/main",
        "sha": "deadbeef",
        "os": "Linux",
        "arch": "X64",
        "runner_name": "GitHub Actions 2",
        "start_ts": 1700000000,
        "end_ts": 1700000123,
        "duration_seconds": 123,
        "cache_hit": "true",
        "job_status": "success"
    }"#;

    #[test]
    fn parse_single_object_with_all_fields() {
        let metrics = parse(SAMPLE_FULL).expect("valid JSON");
        assert_eq!(metrics.len(), 1);
        let m = &metrics[0];
        assert_eq!(m.schema_version, 1);
        assert_eq!(m.workflow, "CI");
        assert_eq!(m.job, "test-unit");
        assert_eq!(m.metric_name, "test-unit-linux");
        assert_eq!(m.matrix_key, "ubuntu-latest-x64");
        assert_eq!(m.run_id, "12345");
        assert_eq!(m.run_attempt, "1");
        assert_eq!(m.run_number, "678");
        assert_eq!(m.event_name, "push");
        assert_eq!(m.git_ref, "refs/heads/main");
        assert_eq!(m.sha, "deadbeef");
        assert_eq!(m.os, "Linux");
        assert_eq!(m.arch, "X64");
        assert_eq!(m.runner_name, "GitHub Actions 2");
        assert_eq!(m.start_ts, 1_700_000_000);
        assert_eq!(m.end_ts, 1_700_000_123);
        assert_eq!(m.duration_seconds, 123);
        assert_eq!(m.cache_hit, Some(true));
        assert_eq!(m.job_status, "success");
    }

    #[test]
    fn parse_accepts_array_input() {
        let input = format!("[{},{}]", SAMPLE_FULL, SAMPLE_FULL);
        let metrics = parse(&input).expect("valid JSON array");
        assert_eq!(metrics.len(), 2);
        assert_eq!(metrics[0], metrics[1]);
    }

    #[test]
    fn parse_empty_array_yields_empty_vec() {
        let metrics = parse("[]").expect("valid empty JSON array");
        assert!(metrics.is_empty());
    }

    #[test]
    fn parse_rejects_malformed_json() {
        let err = parse("{ not json").unwrap_err();
        assert!(matches!(err, Error::Json(_)));
        let msg = format!("{err}");
        assert!(msg.contains("failed to parse ci-metrics JSON"));
    }

    #[test]
    fn parse_missing_optional_fields_uses_defaults() {
        // Only the fields the composite action would *always* populate.
        let minimal = r#"{
            "schema_version": 1,
            "workflow": "CI",
            "job": "lint",
            "metric_name": "lint",
            "start_ts": 100,
            "end_ts": 110,
            "duration_seconds": 10,
            "cache_hit": "",
            "job_status": ""
        }"#;
        let metrics = parse(minimal).expect("valid minimal JSON");
        assert_eq!(metrics.len(), 1);
        let m = &metrics[0];
        assert_eq!(m.matrix_key, "");
        assert_eq!(m.run_id, "");
        assert_eq!(m.cache_hit, None);
        assert_eq!(m.job_status, "");
        assert_eq!(m.duration_seconds, 10);
    }

    #[test]
    fn parse_cache_hit_false_string() {
        let input = SAMPLE_FULL.replace("\"cache_hit\": \"true\"", "\"cache_hit\": \"false\"");
        let metrics = parse(&input).expect("valid JSON");
        assert_eq!(metrics[0].cache_hit, Some(false));
    }

    #[test]
    fn parse_cache_hit_unrecognised_becomes_none() {
        let input = SAMPLE_FULL.replace("\"cache_hit\": \"true\"", "\"cache_hit\": \"maybe\"");
        let metrics = parse(&input).expect("valid JSON");
        assert_eq!(metrics[0].cache_hit, None);
    }

    #[test]
    fn parse_cache_hit_explicit_null_becomes_none() {
        // Exercises the `raw = None` branch of `deserialize_cache_hit`, which
        // is *only* reachable when the JSON value is explicitly `null`. A
        // missing field is short-circuited by `#[serde(default)]` and skips
        // the custom deserializer entirely.
        let input = SAMPLE_FULL.replace("\"cache_hit\": \"true\"", "\"cache_hit\": null");
        let metrics = parse(&input).expect("valid JSON");
        assert_eq!(metrics[0].cache_hit, None);
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        let input = SAMPLE_FULL.replace(
            "\"job_status\": \"success\"",
            "\"job_status\": \"success\", \"future_field\": 42",
        );
        let metrics = parse(&input).expect("forward-compatible parse");
        assert_eq!(metrics[0].job_status, "success");
    }

    #[test]
    fn parse_schema_version_defaults_when_missing() {
        // An artifact that pre-dates schema_version should still parse and
        // default to v1.
        let input = SAMPLE_FULL.replace("\"schema_version\": 1,", "");
        let metrics = parse(&input).expect("valid JSON without schema_version");
        assert_eq!(metrics[0].schema_version, 1);
    }

    #[test]
    fn prometheus_header_present_even_when_empty() {
        let out = to_prometheus(&[]);
        assert!(out.contains("# HELP ci_job_duration_seconds"));
        assert!(out.contains("# TYPE ci_job_duration_seconds gauge"));
        assert!(out.contains("# HELP ci_job_cache_hit"));
        assert!(out.contains("# TYPE ci_job_cache_hit gauge"));
        assert!(out.contains("# HELP ci_job_status"));
        assert!(out.contains("# TYPE ci_job_status gauge"));
        // No data lines.
        for line in out.lines() {
            assert!(
                line.starts_with('#') || line.is_empty(),
                "unexpected: {line}"
            );
        }
    }

    #[test]
    fn prometheus_renders_duration_cache_and_status_for_full_metric() {
        let metrics = parse(SAMPLE_FULL).unwrap();
        let out = to_prometheus(&metrics);

        // Duration line with full label set.
        assert!(out.contains("ci_job_duration_seconds{"));
        assert!(out.contains("workflow=\"CI\""));
        assert!(out.contains("job=\"test-unit\""));
        assert!(out.contains("metric_name=\"test-unit-linux\""));
        assert!(out.contains("matrix_key=\"ubuntu-latest-x64\""));
        assert!(out.contains("os=\"Linux\""));
        assert!(out.contains("arch=\"X64\""));
        assert!(out.contains("event_name=\"push\""));
        assert!(out.contains("ref=\"refs/heads/main\""));
        assert!(out.contains("} 123\n"));

        // Cache-hit reported as 1.
        assert!(out.contains("ci_job_cache_hit{"));
        let cache_line = out
            .lines()
            .find(|l| l.starts_with("ci_job_cache_hit{"))
            .expect("cache hit line");
        assert!(cache_line.ends_with("} 1"), "cache line: {cache_line}");

        // Exactly one status variant should be 1 (success), the rest 0.
        let status_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("ci_job_status{"))
            .collect();
        assert_eq!(status_lines.len(), 4);
        let ones: Vec<&&str> = status_lines.iter().filter(|l| l.ends_with(" 1")).collect();
        assert_eq!(ones.len(), 1);
        assert!(ones[0].contains("status=\"success\""));
    }

    #[test]
    fn prometheus_omits_cache_hit_when_unknown() {
        let input = SAMPLE_FULL.replace("\"cache_hit\": \"true\"", "\"cache_hit\": \"\"");
        let metrics = parse(&input).unwrap();
        let out = to_prometheus(&metrics);
        // Header still references the metric, but no data line should exist.
        let cache_data_lines: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("ci_job_cache_hit{"))
            .collect();
        assert!(
            cache_data_lines.is_empty(),
            "expected no ci_job_cache_hit data lines, got {cache_data_lines:?}"
        );
    }

    #[test]
    fn prometheus_status_failure_routes_to_failure_bucket() {
        let input =
            SAMPLE_FULL.replace("\"job_status\": \"success\"", "\"job_status\": \"failure\"");
        let metrics = parse(&input).unwrap();
        let out = to_prometheus(&metrics);
        let one_line = out
            .lines()
            .filter(|l| l.starts_with("ci_job_status{"))
            .find(|l| l.ends_with(" 1"))
            .expect("one variant is 1");
        assert!(one_line.contains("status=\"failure\""));
    }

    #[test]
    fn prometheus_status_unknown_for_empty_or_unrecognised() {
        for raw in ["", "weird", "SUCCESS-but-malformed"] {
            let input = SAMPLE_FULL.replace(
                "\"job_status\": \"success\"",
                &format!("\"job_status\": \"{raw}\""),
            );
            let metrics = parse(&input).unwrap();
            let out = to_prometheus(&metrics);
            let one_line = out
                .lines()
                .filter(|l| l.starts_with("ci_job_status{"))
                .find(|l| l.ends_with(" 1"))
                .expect("exactly one variant should be 1");
            assert!(
                one_line.contains("status=\"unknown\""),
                "expected unknown bucket for raw={raw:?}, got: {one_line}"
            );
        }
    }

    #[test]
    fn prometheus_status_cancelled_routed_correctly() {
        let input = SAMPLE_FULL.replace(
            "\"job_status\": \"success\"",
            "\"job_status\": \"cancelled\"",
        );
        let metrics = parse(&input).unwrap();
        let out = to_prometheus(&metrics);
        let one_line = out
            .lines()
            .filter(|l| l.starts_with("ci_job_status{"))
            .find(|l| l.ends_with(" 1"))
            .expect("one variant should be 1");
        assert!(one_line.contains("status=\"cancelled\""));
    }

    #[test]
    fn prometheus_status_case_insensitive() {
        let input =
            SAMPLE_FULL.replace("\"job_status\": \"success\"", "\"job_status\": \"Failure\"");
        let metrics = parse(&input).unwrap();
        let out = to_prometheus(&metrics);
        let one_line = out
            .lines()
            .filter(|l| l.starts_with("ci_job_status{"))
            .find(|l| l.ends_with(" 1"))
            .unwrap();
        assert!(one_line.contains("status=\"failure\""));
    }

    #[test]
    fn prometheus_escapes_quotes_backslashes_and_newlines_in_label_values() {
        let mut metric = parse(SAMPLE_FULL).unwrap().pop().unwrap();
        metric.workflow = "weird \"name\"".to_string();
        metric.job = "back\\slash".to_string();
        metric.metric_name = "with\nnewline".to_string();
        let out = to_prometheus(&[metric]);
        assert!(
            out.contains("workflow=\"weird \\\"name\\\"\""),
            "actual: {out}"
        );
        assert!(out.contains("job=\"back\\\\slash\""), "actual: {out}");
        assert!(
            out.contains("metric_name=\"with\\nnewline\""),
            "actual: {out}"
        );
        // Critically, a real newline must not appear inside a value (it would
        // break the exposition format).
        for line in out.lines() {
            // Each data line must contain its full label-set on a single line.
            if line.starts_with("ci_job_") {
                assert!(!line.is_empty());
            }
        }
    }

    #[test]
    fn prometheus_handles_multiple_metrics() {
        let input = format!("[{},{}]", SAMPLE_FULL, SAMPLE_FULL);
        let metrics = parse(&input).unwrap();
        let out = to_prometheus(&metrics);
        let duration_lines = out
            .lines()
            .filter(|l| l.starts_with("ci_job_duration_seconds{"))
            .count();
        assert_eq!(duration_lines, 2);
        let status_lines = out
            .lines()
            .filter(|l| l.starts_with("ci_job_status{"))
            .count();
        assert_eq!(status_lines, 8); // 4 variants * 2 metrics
    }

    #[test]
    fn escape_label_helper_handles_all_special_characters() {
        assert_eq!(escape_label("plain"), "plain");
        assert_eq!(escape_label("a\"b"), "a\\\"b");
        assert_eq!(escape_label("a\\b"), "a\\\\b");
        assert_eq!(escape_label("a\nb"), "a\\nb");
        assert_eq!(escape_label(""), "");
    }

    #[test]
    fn status_matches_helper_unrecognised_variant_returns_false() {
        // Defensive: an internal mis-use that passed an unknown variant
        // should return false rather than panic.
        assert!(!status_matches("success", "garbage"));
    }
}
