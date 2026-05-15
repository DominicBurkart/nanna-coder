//! Telemetry entity types (deferred stub — see issue #27)
//!
//! This module provides a typed stub for telemetry entities. Real collection
//! and emission are deferred to subsequent issues — see the prerequisite list
//! in [`crate::entities::telemetry`].
//!
//! # Compatibility
//!
//! [`TelemetryEntity::new`] and its [`Default`] impl are preserved from the
//! original placeholder so existing call sites (notably
//! `harness::agent::eval`) keep compiling. New fields are given sensible
//! defaults: a [`TelemetrySignalKind::SystemEvent`] signal kind, an empty
//! name, `Utc::now()` timestamp, `0.0` value, and an empty attribute map.
//!
//! # Not to be confused with `harness::telemetry`
//!
//! The top-level [`crate::telemetry`] module is the runtime observability
//! subsystem. This module (`harness::entities::telemetry`) is an entity
//! representation that records telemetry samples as first-class entities
//! in the entity store.

use crate::entities::{EntityMetadata, EntityType};
use crate::impl_entity;
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

    /// Numeric value of the sample. For non-numeric signals (e.g. error logs)
    /// this is `0.0` and callers should use [`Self::attributes`].
    pub value: f64,

    /// Free-form key/value attributes (dimensions, labels, log fields, ...).
    pub attributes: HashMap<String, String>,
}

impl_entity!(TelemetryEntity);

impl TelemetryEntity {
    /// Create a new placeholder telemetry entity.
    ///
    /// This constructor takes no arguments for backwards compatibility with
    /// `harness::agent::eval`, which uses it to seed a representative
    /// telemetry entity. All new fields receive sensible defaults:
    ///
    /// - `signal_kind`: [`TelemetrySignalKind::SystemEvent`]
    /// - `name`: empty string
    /// - `recorded_at`: [`Utc::now`]
    /// - `value`: `0.0`
    /// - `attributes`: empty map
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Telemetry),
            signal_kind: TelemetrySignalKind::SystemEvent,
            name: String::new(),
            recorded_at: Utc::now(),
            value: 0.0,
            attributes: HashMap::new(),
        }
    }
}

impl Default for TelemetryEntity {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::Entity;

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
        assert!(entity.attributes.is_empty());

        // `Default` must produce an equivalent value.
        let defaulted = TelemetryEntity::default();
        assert_eq!(defaulted.signal_kind, entity.signal_kind);
        assert_eq!(defaulted.name, entity.name);
        assert_eq!(defaulted.value, entity.value);
        assert_eq!(defaulted.attributes, entity.attributes);
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

        // Round-trip via `to_json()` preserves the new fields.
        let json = entity.to_json().expect("serialize via Entity::to_json");
        let back: TelemetryEntity = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.name, "cpu.usage");
        assert_eq!(back.signal_kind, TelemetrySignalKind::ResourceUtilization);
        assert_eq!(back.value, 42.5);
        assert_eq!(back.attributes.get("host"), Some(&"worker-1".to_string()));
        assert_eq!(back.metadata.entity_type, EntityType::Telemetry);
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
}
