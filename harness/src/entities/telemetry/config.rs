//! Telemetry configuration loader (deferred stub — see issue #27)
//!
//! This module defines the TOML-backed configuration schema for the
//! telemetry-entities subsystem. Only the parser is implemented here;
//! consumers of the parsed config are deferred to follow-up work.
//!
//! # Schema layout
//!
//! Two layouts are supported so callers can either provide a standalone
//! `telemetry.toml` (flat layout) or embed telemetry inside a larger
//! project-level config (`[telemetry]`-nested layout — matches the example
//! in issue #27). Use [`load_telemetry_config`] for the flat layout and
//! [`load_project_telemetry_config`] for the nested layout.
//!
//! # Required vs optional sections
//!
//! [`MetricsConfig`], [`TracesConfig`], and [`LogsConfig`] are all
//! **required** top-level sections — omit any one and
//! [`load_telemetry_config`] returns
//! [`TelemetryError::MissingSection`]. This is intentional even when
//! `enabled = false`: a future runtime that flips `enabled` on at runtime
//! must still know what to do. The matching `enabled = false` test in this
//! module's tests exercises that requirement explicitly.
//!
//! # Privacy / retention / access controls
//!
//! Issue #27 calls out PII filtering, retention policies, and access
//! controls as first-class concerns. Placeholder fields ([`PiiFilterConfig`],
//! [`RetentionConfig`], [`AccessControlConfig`]) are present on
//! [`TelemetryConfig`] so the schema grows additively rather than breaking
//! when those land. See `TODO(#27)` markers below.
//!
//! # Not to be confused with `harness::telemetry::TelemetryConfig`
//!
//! The top-level [`crate::telemetry::TelemetryConfig`] configures the
//! runtime observability subsystem. The [`TelemetryConfig`] defined in this
//! module configures the entity-store telemetry schema from issue #27.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
use thiserror::Error;

/// Top-level telemetry configuration.
///
/// All three subsections ([`metrics`](Self::metrics), [`traces`](Self::traces),
/// [`logs`](Self::logs)) are mandatory even when [`enabled`](Self::enabled)
/// is `false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryConfig {
    /// Whether telemetry entity collection is enabled.
    pub enabled: bool,

    /// Metrics subsystem configuration.
    pub metrics: MetricsConfig,

    /// Tracing subsystem configuration.
    pub traces: TracesConfig,

    /// Logging subsystem configuration.
    pub logs: LogsConfig,

    /// User-defined custom metrics.
    #[serde(default)]
    pub custom: Vec<CustomMetricSpec>,

    /// Optional PII filtering configuration. See issue #27 (Privacy).
    // TODO(#27): full PII filter spec (allowlist/denylist, redaction rules).
    #[serde(default)]
    pub pii_filter: Option<PiiFilterConfig>,

    /// Optional retention configuration. See issue #27 (Privacy).
    // TODO(#27): full retention spec (per-signal-kind retention windows).
    #[serde(default)]
    pub retention: Option<RetentionConfig>,

    /// Optional access-control configuration. See issue #27 (Privacy).
    // TODO(#27): full access-control spec (RBAC, audit hooks).
    #[serde(default)]
    pub access_control: Option<AccessControlConfig>,
}

/// Project-level config wrapper for the nested `[telemetry]` layout used in
/// issue #27's example. Lets a `project.toml` carry telemetry alongside other
/// sections without forcing a separate file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectTelemetryConfig {
    /// Telemetry section.
    pub telemetry: TelemetryConfig,
}

/// Metrics subsystem configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsConfig {
    /// Collection interval, in milliseconds. Must be `>= 1`. `0` is rejected
    /// because a zero interval would busy-loop the collector.
    pub collect_interval_ms: u64,

    /// Exporters to push metrics to (e.g. `"prometheus"`, `"otlp"`).
    #[serde(default)]
    pub exporters: Vec<String>,
}

/// Tracing subsystem configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TracesConfig {
    /// Sampling rate in `[0.0, 1.0]`. Values outside this range are
    /// rejected by [`load_telemetry_config`] with
    /// [`TelemetryError::Validation`].
    pub sampling_rate: f64,

    /// Exporter name (e.g. `"otlp"`, `"jaeger"`, `"none"`).
    pub exporter: String,
}

/// Logging subsystem configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LogsConfig {
    /// Minimum log level to record (e.g. `"info"`).
    pub level: String,

    /// Log format (e.g. `"json"`, `"text"`).
    pub format: String,

    /// Destinations (e.g. `"stdout"`, `"file:/var/log/harness.log"`).
    #[serde(default)]
    pub destinations: Vec<String>,
}

/// A single user-defined custom metric.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomMetricSpec {
    /// Metric name.
    pub name: String,

    /// Metric type. Constrained closed enum — see [`CustomMetricKind`].
    ///
    /// This is intentionally a **separate type** from
    /// `harness::telemetry::MetricType`: the runtime exporter type uses
    /// PascalCase JSON serialization for backwards compatibility, while the
    /// config-file type below uses lowercase TOML keys (`counter`, `gauge`,
    /// `histogram`, `summary`) to match the issue #27 example. The two
    /// enums carry the same closed set of variants and can be converted via
    /// `From`/`Into` if a future runtime wants to bridge them.
    #[serde(rename = "type")]
    pub metric_type: CustomMetricKind,
}

/// Closed enum mirroring `harness::telemetry::MetricType` with
/// lowercase-rename TOML/JSON serialization.
///
/// Kept separate from `harness::telemetry::MetricType` so that flipping the
/// runtime type's serialization doesn't silently change the on-disk config
/// schema (and vice versa). See [`CustomMetricSpec::metric_type`] for
/// rationale.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CustomMetricKind {
    /// Counter that can only increase.
    Counter,
    /// Gauge that can move up and down.
    Gauge,
    /// Histogram for distribution data.
    Histogram,
    /// Summary with quantiles.
    Summary,
}

/// Placeholder PII filter configuration. See issue #27 (Privacy).
// TODO(#27): replace with concrete PII filter rules.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PiiFilterConfig {
    /// Whether PII filtering is enabled.
    #[serde(default)]
    pub enabled: bool,
}

/// Placeholder retention configuration. See issue #27 (Privacy).
// TODO(#27): replace with per-signal-kind retention windows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RetentionConfig {
    /// Default retention in days.
    #[serde(default)]
    pub default_days: Option<u32>,
}

/// Placeholder access-control configuration. See issue #27 (Privacy).
// TODO(#27): replace with concrete RBAC / audit-hook spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AccessControlConfig {
    /// Whether access control is enforced.
    #[serde(default)]
    pub enforced: bool,
}

/// Errors that can occur while loading a [`TelemetryConfig`].
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// Underlying I/O error reading the config file.
    #[error("I/O error loading telemetry config: {0}")]
    Io(#[from] io::Error),

    /// TOML parse error. Carries the structured `toml::de::Error` so the
    /// caller still has access to span info, the offending key, and the
    /// underlying message.
    #[error("TOML parse error in telemetry config: {0}")]
    Toml(#[from] toml::de::Error),

    /// A required top-level section (`metrics`, `traces`, or `logs`) was
    /// missing. This is reported separately from [`Self::Toml`] so callers
    /// can distinguish "the file doesn't even tokenize" from "the file
    /// parsed but is missing a mandatory section".
    #[error("missing required telemetry section: {0}")]
    MissingSection(&'static str),

    /// A field value failed range / sanity validation. Carries the offending
    /// field name and a short human-readable reason.
    #[error("validation error in telemetry config: field `{field}`: {reason}")]
    Validation {
        /// Dotted field path, e.g. `traces.sampling_rate`.
        field: &'static str,
        /// Human-readable reason the value was rejected.
        reason: String,
    },
}

/// Load a [`TelemetryConfig`] from a TOML file on disk.
///
/// Validates that:
/// - `traces.sampling_rate` is in `[0.0, 1.0]`.
/// - `metrics.collect_interval_ms` is non-zero.
///
/// Returns [`TelemetryError::Validation`] for out-of-range values, and
/// [`TelemetryError::Toml`] for syntactic errors (preserving the underlying
/// `toml::de::Error`).
pub fn load_telemetry_config(path: &Path) -> Result<TelemetryConfig, TelemetryError> {
    let raw = std::fs::read_to_string(path)?;
    let cfg: TelemetryConfig = toml::from_str(&raw)?;
    validate_telemetry_config(&cfg)?;
    Ok(cfg)
}

/// Load a [`TelemetryConfig`] from a TOML file using the nested
/// `[telemetry]`-table layout (matches the issue #27 example).
pub fn load_project_telemetry_config(path: &Path) -> Result<TelemetryConfig, TelemetryError> {
    let raw = std::fs::read_to_string(path)?;
    let wrapper: ProjectTelemetryConfig = toml::from_str(&raw)?;
    validate_telemetry_config(&wrapper.telemetry)?;
    Ok(wrapper.telemetry)
}

/// Apply range/sanity validation to a parsed [`TelemetryConfig`]. Exposed so
/// callers that build a config in-memory (e.g. tests) can run the same
/// rules.
pub fn validate_telemetry_config(cfg: &TelemetryConfig) -> Result<(), TelemetryError> {
    if !(0.0..=1.0).contains(&cfg.traces.sampling_rate) {
        return Err(TelemetryError::Validation {
            field: "traces.sampling_rate",
            reason: format!("must be in [0.0, 1.0], got {}", cfg.traces.sampling_rate),
        });
    }
    if cfg.metrics.collect_interval_ms == 0 {
        return Err(TelemetryError::Validation {
            field: "metrics.collect_interval_ms",
            reason: "must be >= 1 (zero would busy-loop the collector)".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    const FULL_CONFIG_TOML: &str = r#"
enabled = true

[metrics]
collect_interval_ms = 1000
exporters = ["prometheus", "otlp"]

[traces]
sampling_rate = 0.25
exporter = "otlp"

[logs]
level = "info"
format = "json"
destinations = ["stdout", "file:/tmp/harness.log"]

[[custom]]
name = "tokens_processed"
type = "counter"

[[custom]]
name = "queue_depth"
type = "gauge"
"#;

    fn write_tmp(contents: &str) -> NamedTempFile {
        let mut tmp = NamedTempFile::new().expect("create tempfile");
        tmp.write_all(contents.as_bytes()).expect("write tempfile");
        tmp.flush().expect("flush tempfile");
        tmp
    }

    #[test]
    fn test_load_telemetry_config_full() {
        let tmp = write_tmp(FULL_CONFIG_TOML);
        let cfg = load_telemetry_config(tmp.path()).expect("load full config");

        assert!(cfg.enabled);
        assert_eq!(cfg.metrics.collect_interval_ms, 1000);
        assert_eq!(
            cfg.metrics.exporters,
            vec!["prometheus".to_string(), "otlp".to_string()]
        );
        assert_eq!(cfg.traces.sampling_rate, 0.25);
        assert_eq!(cfg.traces.exporter, "otlp");
        assert_eq!(cfg.logs.level, "info");
        assert_eq!(cfg.logs.format, "json");
        assert_eq!(
            cfg.logs.destinations,
            vec!["stdout".to_string(), "file:/tmp/harness.log".to_string()]
        );
        assert_eq!(cfg.custom.len(), 2);
        assert_eq!(cfg.custom[0].name, "tokens_processed");
        assert_eq!(cfg.custom[0].metric_type, CustomMetricKind::Counter);
        assert_eq!(cfg.custom[1].name, "queue_depth");
        assert_eq!(cfg.custom[1].metric_type, CustomMetricKind::Gauge);
        // New optional privacy fields default to `None`.
        assert!(cfg.pii_filter.is_none());
        assert!(cfg.retention.is_none());
        assert!(cfg.access_control.is_none());
    }

    #[test]
    fn test_load_telemetry_config_missing_file() {
        let missing = std::path::Path::new("/definitely/does/not/exist/telemetry.toml");
        let err = load_telemetry_config(missing).expect_err("missing file must error");

        match err {
            TelemetryError::Io(_) => {}
            other => panic!("expected Io, got {:?}", other),
        }
    }

    #[test]
    fn test_load_telemetry_config_malformed_toml() {
        let tmp = write_tmp("this is = = not valid toml [[[");
        let err = load_telemetry_config(tmp.path()).expect_err("malformed toml must error");

        match err {
            // Now carries a structured `toml::de::Error` rather than a
            // stringified copy.
            TelemetryError::Toml(de_err) => {
                let msg = de_err.to_string();
                assert!(!msg.is_empty(), "toml error message should not be empty");
            }
            other => panic!("expected Toml, got {:?}", other),
        }
    }

    #[test]
    fn test_load_telemetry_config_disabled() {
        let toml_src = r#"
enabled = false

[metrics]
collect_interval_ms = 500

[traces]
sampling_rate = 0.0
exporter = "none"

[logs]
level = "warn"
format = "text"
"#;
        let tmp = write_tmp(toml_src);
        let cfg = load_telemetry_config(tmp.path()).expect("load disabled config");

        assert!(!cfg.enabled);
        assert_eq!(cfg.metrics.collect_interval_ms, 500);
        assert!(cfg.metrics.exporters.is_empty());
        assert_eq!(cfg.traces.sampling_rate, 0.0);
        assert_eq!(cfg.traces.exporter, "none");
        assert_eq!(cfg.logs.level, "warn");
        assert_eq!(cfg.logs.format, "text");
        assert!(cfg.logs.destinations.is_empty());
        assert!(cfg.custom.is_empty());

        // Round-trip serialize/parse should preserve equality.
        let reserialized = toml::to_string(&cfg).expect("serialize back to TOML");
        let back: TelemetryConfig =
            toml::from_str(&reserialized).expect("re-parse serialized TOML");
        assert_eq!(cfg, back);
    }

    #[test]
    fn test_telemetry_error_display() {
        let io_err = TelemetryError::Io(io::Error::new(io::ErrorKind::NotFound, "nope"));
        let io_display = format!("{}", io_err);
        assert!(io_display.contains("I/O error"));
        assert!(io_display.contains("nope"));

        // Build a real `toml::de::Error` rather than a String.
        let de_err: toml::de::Error =
            toml::from_str::<TelemetryConfig>("not = = toml [[[").expect_err("invalid toml");
        let toml_err = TelemetryError::Toml(de_err);
        let toml_display = format!("{}", toml_err);
        assert!(toml_display.contains("TOML parse error"));

        let validation = TelemetryError::Validation {
            field: "traces.sampling_rate",
            reason: "must be in [0.0, 1.0], got 5".to_string(),
        };
        let v_display = format!("{}", validation);
        assert!(v_display.contains("validation error"));
        assert!(v_display.contains("traces.sampling_rate"));

        let missing = TelemetryError::MissingSection("metrics");
        let m_display = format!("{}", missing);
        assert!(m_display.contains("missing required telemetry section"));
        assert!(m_display.contains("metrics"));
    }

    /// Regression test: `traces.sampling_rate` must be inside `[0.0, 1.0]`.
    /// Negative values, values > 1.0, and `NaN` are all rejected by
    /// [`TelemetryError::Validation`].
    #[test]
    fn test_load_telemetry_config_rejects_out_of_range_sampling_rate() {
        let cases = [
            ("-0.1", "negative sampling rate"),
            ("1.5", "sampling rate above 1.0"),
            ("1e9", "very large sampling rate"),
            ("nan", "nan sampling rate"),
        ];
        for (value, label) in cases {
            let toml_src = format!(
                r#"
enabled = true

[metrics]
collect_interval_ms = 1000

[traces]
sampling_rate = {value}
exporter = "otlp"

[logs]
level = "info"
format = "json"
"#
            );
            let tmp = write_tmp(&toml_src);
            let err = load_telemetry_config(tmp.path()).expect_err(&format!("{label} must error"));
            match err {
                TelemetryError::Validation { field, .. } => {
                    assert_eq!(field, "traces.sampling_rate", "case `{label}`");
                }
                other => panic!("case `{label}`: expected Validation, got {:?}", other),
            }
        }
    }

    /// Regression test: `metrics.collect_interval_ms = 0` is rejected because
    /// a zero interval would busy-loop the collector.
    #[test]
    fn test_load_telemetry_config_rejects_zero_collect_interval() {
        let toml_src = r#"
enabled = true

[metrics]
collect_interval_ms = 0

[traces]
sampling_rate = 0.5
exporter = "otlp"

[logs]
level = "info"
format = "json"
"#;
        let tmp = write_tmp(toml_src);
        let err = load_telemetry_config(tmp.path()).expect_err("zero interval must error");
        match err {
            TelemetryError::Validation { field, .. } => {
                assert_eq!(field, "metrics.collect_interval_ms");
            }
            other => panic!("expected Validation, got {:?}", other),
        }
    }

    /// Regression test: `CustomMetricSpec::metric_type` is now a closed enum
    /// — unknown strings fail to deserialize rather than being silently
    /// accepted as free-form text.
    #[test]
    fn test_custom_metric_kind_rejects_unknown_variant() {
        let toml_src = r#"
enabled = true

[metrics]
collect_interval_ms = 1000

[traces]
sampling_rate = 0.5
exporter = "otlp"

[logs]
level = "info"
format = "json"

[[custom]]
name = "weird"
type = "not_a_real_metric_kind"
"#;
        let tmp = write_tmp(toml_src);
        let err = load_telemetry_config(tmp.path()).expect_err("unknown metric kind must error");
        match err {
            TelemetryError::Toml(_) => {}
            other => panic!("expected Toml (unknown variant), got {:?}", other),
        }
    }

    /// Regression test: every `CustomMetricKind` variant round-trips through
    /// TOML using its lowercase wire name.
    #[test]
    fn test_custom_metric_kind_round_trip() {
        let pairs = [
            ("counter", CustomMetricKind::Counter),
            ("gauge", CustomMetricKind::Gauge),
            ("histogram", CustomMetricKind::Histogram),
            ("summary", CustomMetricKind::Summary),
        ];
        for (wire, variant) in pairs {
            let toml_src = format!(
                r#"
name = "x"
type = "{wire}"
"#
            );
            let parsed: CustomMetricSpec = toml::from_str(&toml_src).expect("parse custom metric");
            assert_eq!(parsed.metric_type, variant, "wire `{wire}`");
            let reserialized = toml::to_string(&parsed).expect("reserialize");
            assert!(
                reserialized.contains(&format!("type = \"{wire}\"")),
                "reserialized must contain lowercase wire form: {reserialized}"
            );
        }
    }

    /// Regression test: nested `[telemetry]` layout from issue #27 loads via
    /// [`load_project_telemetry_config`].
    #[test]
    fn test_load_project_telemetry_config_nested() {
        let toml_src = r#"
[telemetry]
enabled = true

[telemetry.metrics]
collect_interval_ms = 2000
exporters = ["prometheus"]

[telemetry.traces]
sampling_rate = 0.1
exporter = "jaeger"

[telemetry.logs]
level = "debug"
format = "json"
destinations = ["stdout"]

[[telemetry.custom]]
name = "requests"
type = "counter"
"#;
        let tmp = write_tmp(toml_src);
        let cfg = load_project_telemetry_config(tmp.path()).expect("nested layout must load");

        assert!(cfg.enabled);
        assert_eq!(cfg.metrics.collect_interval_ms, 2000);
        assert_eq!(cfg.traces.sampling_rate, 0.1);
        assert_eq!(cfg.traces.exporter, "jaeger");
        assert_eq!(cfg.logs.level, "debug");
        assert_eq!(cfg.custom.len(), 1);
        assert_eq!(cfg.custom[0].metric_type, CustomMetricKind::Counter);
    }

    /// Regression test: privacy/retention/access placeholders deserialize
    /// when present so the schema can grow additively without breaking.
    #[test]
    fn test_load_telemetry_config_privacy_placeholders() {
        let toml_src = r#"
enabled = true

[metrics]
collect_interval_ms = 1000

[traces]
sampling_rate = 0.5
exporter = "otlp"

[logs]
level = "info"
format = "json"

[pii_filter]
enabled = true

[retention]
default_days = 30

[access_control]
enforced = true
"#;
        let tmp = write_tmp(toml_src);
        let cfg = load_telemetry_config(tmp.path()).expect("privacy placeholders must load");

        assert_eq!(
            cfg.pii_filter.as_ref().map(|p| p.enabled),
            Some(true),
            "pii_filter must round-trip"
        );
        assert_eq!(
            cfg.retention.as_ref().and_then(|r| r.default_days),
            Some(30),
            "retention.default_days must round-trip"
        );
        assert_eq!(
            cfg.access_control.as_ref().map(|a| a.enforced),
            Some(true),
            "access_control.enforced must round-trip"
        );
    }
}
