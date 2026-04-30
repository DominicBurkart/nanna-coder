//! Telemetry configuration loader (deferred stub — see issue #27)
//!
//! This module defines the TOML-backed configuration schema for the
//! telemetry-entities subsystem. Only the parser is implemented here;
//! consumers of the parsed config are deferred to follow-up work.
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
}

/// Metrics subsystem configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsConfig {
    /// Collection interval, in milliseconds.
    pub collect_interval_ms: u64,

    /// Exporters to push metrics to (e.g. `"prometheus"`, `"otlp"`).
    #[serde(default)]
    pub exporters: Vec<String>,
}

/// Tracing subsystem configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TracesConfig {
    /// Sampling rate in `[0.0, 1.0]`.
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

    /// Metric type (e.g. `"counter"`, `"gauge"`, `"histogram"`).
    #[serde(rename = "type")]
    pub metric_type: String,
}

/// Errors that can occur while loading a [`TelemetryConfig`].
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// Underlying I/O error reading the config file.
    #[error("I/O error loading telemetry config: {0}")]
    Io(#[from] io::Error),

    /// TOML parse error.
    #[error("TOML parse error in telemetry config: {0}")]
    Toml(String),
}

/// Load a [`TelemetryConfig`] from a TOML file on disk.
pub fn load_telemetry_config(path: &Path) -> Result<TelemetryConfig, TelemetryError> {
    let raw = std::fs::read_to_string(path)?;
    toml::from_str::<TelemetryConfig>(&raw).map_err(|e| TelemetryError::Toml(e.to_string()))
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
        assert_eq!(cfg.custom[0].metric_type, "counter");
        assert_eq!(cfg.custom[1].name, "queue_depth");
        assert_eq!(cfg.custom[1].metric_type, "gauge");
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
            TelemetryError::Toml(msg) => {
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

        let toml_err = TelemetryError::Toml("bad token at line 1".to_string());
        let toml_display = format!("{}", toml_err);
        assert!(toml_display.contains("TOML parse error"));
        assert!(toml_display.contains("bad token"));
    }
}
