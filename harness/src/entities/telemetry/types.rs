//! Telemetry entity types (deferred stub — see issue #27)
//!
//! This module provides a typed stub for telemetry entities. Real collection
//! and emission are deferred to subsequent issues — see the prerequisite list
//! in [`crate::entities::telemetry`].
//!
//! # Compatibility
//!
//! [`TelemetryEntity::new`] (alias [`TelemetryEntity::placeholder`]) and its
//! [`Default`] impl are preserved from the original placeholder so existing
//! call sites (notably `harness::agent::eval`) keep compiling.
//!
//! New code should prefer [`TelemetryEntity::new_signal`] which takes a
//! [`TelemetrySignalKind`], a name, and a [`TelemetrySample`]. The latter is
//! a typed enum (`Counter`, `Gauge`, `Histogram`, `Trace`, `Log`, `Event`)
//! so a "no numeric value" signal (e.g. an error log) is no longer
//! indistinguishable from `value = 0.0`.
//!
//! Once `harness::agent::eval` is migrated off the zero-arg placeholder,
//! [`TelemetryEntity::placeholder`] should be removed in favor of
//! [`TelemetryEntity::new_signal`]. Tracked by issue #27 and the
//! `// TODO(#27)` marker on [`TelemetryEntity::placeholder`].
//!
//! # `value` field semantics
//!
//! The bare numeric [`TelemetryEntity::value`] field is preserved for
//! backwards compatibility. It is populated by
//! [`TelemetryEntity::new_signal`] from the sample's scalar value (Counter /
//! Gauge), but a hand-built or deserialized entity can carry an arbitrary
//! `value` regardless of [`TelemetryEntity::sample`] — the two fields are
//! independent on the wire, and this struct does **not** enforce an
//! invariant between them. New code should call
//! [`TelemetryEntity::sample_value`], which prefers the typed sample's
//! scalar value and only falls back to the legacy bare field when no sample
//! is set; that way drift between the two fields cannot silently mislead
//! consumers.
//!
//! # Not to be confused with `harness::telemetry`
//!
//! The top-level [`crate::telemetry`] module is the runtime observability
//! subsystem. This module (`harness::entities::telemetry`) is an entity
//! representation that records telemetry samples as first-class entities
//! in the entity store.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Kind of telemetry signal recorded by a [`TelemetryEntity`].
///
/// Mirrors the high-level taxonomy described in issue #27: runtime metrics,
/// performance traces, resource utilization, error logs, generic system
/// events, and custom user-defined metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TelemetrySignalKind {
    /// Runtime metric (counter, gauge, histogram sample, ...)
    RuntimeMetric,

    /// Performance trace / span sample
    PerformanceTrace,

    /// Resource utilization sample (CPU, memory, disk, ...)
    ResourceUtilization,

    /// Error log sample
    ErrorLog,

    /// Generic system event (default for placeholder entities)
    SystemEvent,

    /// User-defined custom metric (see [`crate::entities::telemetry::config::CustomMetricSpec`])
    CustomMetric,
}

/// A typed telemetry sample. Models the issue #27 taxonomy faithfully:
/// numeric counters/gauges, histogram buckets, traces, logs, and arbitrary
/// JSON-shaped events all have distinct constructors so callers cannot
/// accidentally encode "no value" as `0.0`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "sample_kind", rename_all = "snake_case")]
pub enum TelemetrySample {
    /// Monotonic counter — non-decreasing `u64`.
    Counter {
        /// Counter value at sample time.
        value: u64,
    },
    /// Gauge — `f64` that may move up or down.
    Gauge {
        /// Gauge value at sample time.
        value: f64,
    },
    /// Histogram — sequence of bucket samples.
    Histogram {
        /// Histogram bucket samples.
        values: Vec<f64>,
    },
    /// Performance trace span — opaque string payload until issue #27
    /// formalizes the trace shape. The string is intentionally generic so
    /// the schema doesn't ossify around any one tracing library.
    Trace {
        /// Trace span identifier.
        span_id: String,
    },
    /// Log record — `level` + `message`. Distinct from the runtime
    /// `tracing` log subsystem; this is the entity-store representation.
    Log {
        /// Log level (e.g. `"info"`, `"warn"`, `"error"`).
        level: String,
        /// Log message body.
        message: String,
    },
    /// Generic system event — free-form attribute map only. Used when none
    /// of the above shapes fit (matches the legacy zero-arg placeholder).
    Event,
}

impl TelemetrySample {
    /// Bare numeric `f64` view of the sample, for the legacy
    /// [`TelemetryEntity::value`] field. Returns `None` for samples that do
    /// not have a single scalar value (`Histogram`, `Trace`, `Log`,
    /// `Event`).
    pub fn scalar_value(&self) -> Option<f64> {
        match self {
            TelemetrySample::Counter { value } => Some(*value as f64),
            TelemetrySample::Gauge { value } => Some(*value),
            TelemetrySample::Histogram { .. }
            | TelemetrySample::Trace { .. }
            | TelemetrySample::Log { .. }
            | TelemetrySample::Event => None,
        }
    }
}

/// Telemetry entity — a single sampled telemetry signal.
///
/// This is a deferred stub: the fields below are the schema we intend to use,
/// but no runtime collection is wired up yet. See issue #27.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// What kind of telemetry signal this sample represents.
    pub signal_kind: TelemetrySignalKind,

    /// Human-readable name of the signal (e.g. `"cpu.usage"`).
    pub name: String,

    /// Timestamp the sample was recorded.
    pub recorded_at: DateTime<Utc>,

    /// Bare numeric value of the sample.
    ///
    /// **Not an enforced invariant relative to [`Self::sample`].** The
    /// constructors in this module keep the two fields consistent (see
    /// [`Self::new_signal`]), but a hand-built or deserialized entity can
    /// carry any `value` regardless of [`Self::sample`]. New code should
    /// prefer [`Self::sample_value`], which consults the typed sample first
    /// and falls back to this bare field only when no sample is attached.
    /// Tracked by issue #27.
    // TODO(#27): once `harness::agent::eval` migrates to the typed
    // constructor, drop this bare field and rely solely on `sample`.
    pub value: f64,

    /// Typed sample payload. Optional for backwards-compat with the legacy
    /// zero-arg placeholder constructor; new call sites built via
    /// [`Self::new_signal`] always populate this.
    #[serde(default)]
    pub sample: Option<TelemetrySample>,

    /// Free-form key/value attributes (dimensions, labels, log fields, ...).
    pub attributes: HashMap<String, String>,
}

#[async_trait]
impl Entity for TelemetryEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

impl TelemetryEntity {
    /// Create a new placeholder telemetry entity (zero-arg).
    ///
    /// Preserved for backwards compatibility with `harness::agent::eval`.
    /// New call sites should prefer [`Self::new_signal`].
    ///
    /// All new fields receive sensible defaults:
    ///
    /// - `signal_kind`: [`TelemetrySignalKind::SystemEvent`]
    /// - `name`: empty string
    /// - `recorded_at`: [`Utc::now`]
    /// - `value`: `0.0` (semantically *no scalar value defined* — see
    ///   [`Self::value`])
    /// - `sample`: `None`
    /// - `attributes`: empty map
    // TODO(#27): replace zero-arg placeholder with typed constructor when
    // `harness::agent::eval` migrates off the placeholder shape.
    pub fn placeholder() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Telemetry),
            signal_kind: TelemetrySignalKind::SystemEvent,
            name: String::new(),
            recorded_at: Utc::now(),
            value: 0.0,
            sample: None,
            attributes: HashMap::new(),
        }
    }

    /// Backwards-compatible alias for [`Self::placeholder`].
    ///
    /// Prefer [`Self::new_signal`] for new code; this alias is kept so
    /// existing zero-arg call sites in `harness::agent::eval` keep
    /// compiling.
    // TODO(#27): replace with typed constructor when eval.rs migrates.
    pub fn new() -> Self {
        Self::placeholder()
    }

    /// Create a new typed telemetry entity from a signal kind, a name, and a
    /// typed sample payload. The bare [`Self::value`] field is populated
    /// from `sample.scalar_value()` when meaningful (Counter / Gauge); for
    /// other sample kinds it remains `0.0` and callers must consult
    /// [`Self::sample`].
    pub fn new_signal(
        signal_kind: TelemetrySignalKind,
        name: impl Into<String>,
        sample: TelemetrySample,
    ) -> Self {
        let value = sample.scalar_value().unwrap_or(0.0);
        Self {
            metadata: EntityMetadata::new(EntityType::Telemetry),
            signal_kind,
            name: name.into(),
            recorded_at: Utc::now(),
            value,
            sample: Some(sample),
            attributes: HashMap::new(),
        }
    }

    /// Drift-safe scalar accessor for the sample's numeric value.
    ///
    /// Prefers the typed [`Self::sample`]'s `scalar_value()`. Returns:
    ///
    /// - `Some(v)` when `sample` is `Counter` or `Gauge` (using the typed
    ///   sample, never the bare `value` field, so a hand-built or
    ///   deserialized entity that has both fields out of sync still
    ///   reports the typed value).
    /// - `None` when `sample` is `Histogram`, `Trace`, `Log`, or `Event`
    ///   (signals with no single scalar — callers must inspect
    ///   [`Self::sample`] for the structured payload).
    /// - `Some(self.value)` when `sample` is `None` (legacy placeholder
    ///   path, where the bare field is the only thing we have).
    pub fn sample_value(&self) -> Option<f64> {
        match &self.sample {
            Some(sample) => sample.scalar_value(),
            None => Some(self.value),
        }
    }
}

impl Default for TelemetryEntity {
    fn default() -> Self {
        Self::placeholder()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_entity_default_preserves_placeholder_behavior() {
        // `TelemetryEntity::new()` must remain a zero-arg constructor so
        // `harness::agent::eval::...` keeps compiling. All fields should be
        // filled with sensible defaults.
        let entity = TelemetryEntity::new();

        assert_eq!(entity.metadata.entity_type, EntityType::Telemetry);
        assert_eq!(entity.signal_kind, TelemetrySignalKind::SystemEvent);
        assert!(entity.name.is_empty());
        assert_eq!(entity.value, 0.0);
        assert!(entity.sample.is_none());
        assert!(entity.attributes.is_empty());

        // `Default` must produce an equivalent value.
        let defaulted = TelemetryEntity::default();
        assert_eq!(defaulted.signal_kind, entity.signal_kind);
        assert_eq!(defaulted.name, entity.name);
        assert_eq!(defaulted.value, entity.value);
        assert_eq!(defaulted.sample, entity.sample);
        assert_eq!(defaulted.attributes, entity.attributes);

        // `placeholder()` and `new()` must be equivalent.
        let placeholder = TelemetryEntity::placeholder();
        assert_eq!(placeholder.signal_kind, entity.signal_kind);
        assert_eq!(placeholder.value, entity.value);
        assert_eq!(placeholder.sample, entity.sample);
    }

    #[test]
    fn test_telemetry_signal_kind_serde() {
        // Each variant must round-trip through JSON.
        let variants = [
            TelemetrySignalKind::RuntimeMetric,
            TelemetrySignalKind::PerformanceTrace,
            TelemetrySignalKind::ResourceUtilization,
            TelemetrySignalKind::ErrorLog,
            TelemetrySignalKind::SystemEvent,
            TelemetrySignalKind::CustomMetric,
        ];

        for variant in variants {
            let json = serde_json::to_string(&variant).expect("serialize");
            let back: TelemetrySignalKind = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(variant, back, "round-trip mismatch for {:?}", variant);
        }
    }

    #[tokio::test]
    async fn test_telemetry_entity_implements_entity() {
        let mut entity = TelemetryEntity::new();
        entity.name = "cpu.usage".to_string();
        entity.signal_kind = TelemetrySignalKind::ResourceUtilization;
        entity.value = 42.5;
        entity
            .attributes
            .insert("host".to_string(), "worker-1".to_string());

        // `Entity` trait surface.
        assert_eq!(entity.entity_type(), EntityType::Telemetry);
        assert!(!entity.id().is_empty());

        let pre_id = entity.metadata.id.clone();
        let pre_version = entity.metadata.version;
        let pre_created_at = entity.metadata.created_at;
        let pre_entity_type = entity.metadata.entity_type.clone();

        // Round-trip via `to_json()` preserves the new fields.
        let json = entity.to_json().expect("serialize via Entity::to_json");
        let back: TelemetryEntity = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.name, "cpu.usage");
        assert_eq!(back.signal_kind, TelemetrySignalKind::ResourceUtilization);
        assert_eq!(back.value, 42.5);
        assert_eq!(back.attributes.get("host"), Some(&"worker-1".to_string()));

        // Regression: `EntityMetadata` round-trips fully (id, version,
        // created_at, entity_type, tags) so a future telemetry field name
        // colliding with an `EntityMetadata` field would fail loudly here.
        assert_eq!(back.metadata.id, pre_id, "metadata.id must round-trip");
        assert_eq!(
            back.metadata.version, pre_version,
            "metadata.version must round-trip"
        );
        assert_eq!(
            back.metadata.created_at, pre_created_at,
            "metadata.created_at must round-trip"
        );
        assert_eq!(
            back.metadata.entity_type, pre_entity_type,
            "metadata.entity_type must round-trip"
        );
        assert_eq!(back.metadata.tags, entity.metadata.tags);
    }

    #[test]
    fn test_telemetry_entity_attributes_populated() {
        let mut entity = TelemetryEntity::new();
        entity
            .attributes
            .insert("service".to_string(), "harness".to_string());
        entity
            .attributes
            .insert("env".to_string(), "ci".to_string());

        assert_eq!(entity.attributes.len(), 2);
        assert_eq!(
            entity.attributes.get("service"),
            Some(&"harness".to_string())
        );
        assert_eq!(entity.attributes.get("env"), Some(&"ci".to_string()));
    }

    /// Regression test: typed `new_signal` constructor populates all fields
    /// and bridges scalar samples into the legacy [`TelemetryEntity::value`]
    /// slot.
    #[test]
    fn test_telemetry_entity_new_signal_counter_populates_value() {
        let entity = TelemetryEntity::new_signal(
            TelemetrySignalKind::RuntimeMetric,
            "tokens_processed",
            TelemetrySample::Counter { value: 17 },
        );
        assert_eq!(entity.signal_kind, TelemetrySignalKind::RuntimeMetric);
        assert_eq!(entity.name, "tokens_processed");
        assert_eq!(entity.value, 17.0);
        assert_eq!(entity.sample, Some(TelemetrySample::Counter { value: 17 }));
    }

    /// Regression test: non-scalar samples leave `value = 0.0` and force
    /// callers to look at [`TelemetryEntity::sample`] (which is exactly the
    /// guarantee the new docs make).
    #[test]
    fn test_telemetry_entity_new_signal_log_leaves_value_zero() {
        let entity = TelemetryEntity::new_signal(
            TelemetrySignalKind::ErrorLog,
            "panic.recovered",
            TelemetrySample::Log {
                level: "error".to_string(),
                message: "boom".to_string(),
            },
        );
        assert_eq!(entity.value, 0.0, "log samples have no scalar value");
        match entity.sample {
            Some(TelemetrySample::Log {
                ref level,
                ref message,
            }) => {
                assert_eq!(level, "error");
                assert_eq!(message, "boom");
            }
            other => panic!("expected Log sample, got {:?}", other),
        }
    }

    /// Regression test: every `TelemetrySample` variant round-trips through
    /// JSON.
    #[test]
    fn test_telemetry_sample_round_trip() {
        let cases = vec![
            TelemetrySample::Counter { value: 42 },
            TelemetrySample::Gauge { value: -2.5 },
            TelemetrySample::Histogram {
                values: vec![1.0, 2.0, 3.0],
            },
            TelemetrySample::Trace {
                span_id: "abc123".to_string(),
            },
            TelemetrySample::Log {
                level: "warn".to_string(),
                message: "slow".to_string(),
            },
            TelemetrySample::Event,
        ];
        for sample in cases {
            let json = serde_json::to_string(&sample).expect("serialize sample");
            let back: TelemetrySample = serde_json::from_str(&json).expect("deserialize sample");
            assert_eq!(sample, back, "round-trip mismatch for {sample:?}");
        }
    }

    /// Regression test: `TelemetryEntity::sample_value` prefers the typed
    /// sample over the bare `value` field, so drift between the two cannot
    /// silently mislead consumers. Pins the contract the new docs make on
    /// the bare `value` field.
    #[test]
    fn test_telemetry_entity_sample_value_prefers_typed_sample() {
        // Drifted hand-built entity: bare `value = 99.0` but the typed
        // sample says the counter is at 7.
        let mut drifted = TelemetryEntity::new_signal(
            TelemetrySignalKind::RuntimeMetric,
            "drifted",
            TelemetrySample::Counter { value: 7 },
        );
        drifted.value = 99.0;
        assert_eq!(
            drifted.sample_value(),
            Some(7.0),
            "typed sample wins over a drifted bare value"
        );

        // Non-scalar sample reports `None` even if `value` is non-zero on
        // the bare field.
        let mut non_scalar = TelemetryEntity::new_signal(
            TelemetrySignalKind::ErrorLog,
            "panic",
            TelemetrySample::Log {
                level: "error".to_string(),
                message: "boom".to_string(),
            },
        );
        non_scalar.value = 42.0;
        assert_eq!(
            non_scalar.sample_value(),
            None,
            "log samples have no scalar regardless of the bare value field"
        );

        // Legacy placeholder (no typed sample) falls back to the bare
        // field.
        let mut legacy = TelemetryEntity::new();
        legacy.value = 3.5;
        assert_eq!(
            legacy.sample_value(),
            Some(3.5),
            "no typed sample falls back to the bare value field"
        );
    }

    /// Regression test: `TelemetrySample::scalar_value` only returns `Some`
    /// for `Counter`/`Gauge`, encoding the "no scalar value" semantics.
    #[test]
    fn test_telemetry_sample_scalar_value_semantics() {
        assert_eq!(
            TelemetrySample::Counter { value: 5 }.scalar_value(),
            Some(5.0)
        );
        assert_eq!(
            TelemetrySample::Gauge { value: 1.5 }.scalar_value(),
            Some(1.5)
        );
        assert_eq!(
            TelemetrySample::Histogram {
                values: vec![0.0, 1.0]
            }
            .scalar_value(),
            None
        );
        assert_eq!(
            TelemetrySample::Trace {
                span_id: "x".to_string()
            }
            .scalar_value(),
            None
        );
        assert_eq!(
            TelemetrySample::Log {
                level: "info".into(),
                message: "m".into()
            }
            .scalar_value(),
            None
        );
        assert_eq!(TelemetrySample::Event.scalar_value(), None);
    }
}
