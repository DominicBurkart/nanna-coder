//! Sandbox Telemetry Entities (deferred — TODO, tracked in issue #27)
//!
//! This module provides a **typed stub** for telemetry entities alongside
//! a TOML-backed configuration loader. The runtime wiring that actually
//! collects and emits samples is intentionally deferred.
//!
//! # Status
//!
//! This is marked TODO. Only the schema, the entity trait impl, and the
//! config parser are implemented in this slice. No runtime collection is
//! wired up.
//!
//! # Prerequisite issues
//!
//! Full telemetry-entity support depends on the following work landing
//! first:
//!
//! - #23 — entity store persistence layer
//! - #24 — runtime instrumentation surface
//! - #25 — cross-entity correlation / relationship indexing
//!
//! Until those land, [`TelemetryEntity::placeholder`] (alias
//! [`TelemetryEntity::new`]) remains a zero-argument placeholder
//! constructor so that existing call sites (notably
//! `harness::agent::eval`) keep compiling.
//!
//! # Do not confuse with `harness::telemetry`
//!
//! The top-level [`crate::telemetry`] module is the runtime observability
//! subsystem (exporters, trace contexts, etc.). This module
//! (`harness::entities::telemetry`) is the **entity-store** representation
//! of telemetry samples — a separate abstraction that lets telemetry be
//! queried, related, and persisted like any other entity.

pub mod config;
pub mod types;

// Explicit re-exports (no glob) so the public surface of this module is
// auditable and so the colliding names that this module shares with
// `harness::telemetry` (TelemetryError, TelemetryConfig) stay grep-able.
// Adding glob re-exports actively makes `use harness::entities::telemetry::TelemetryError;`
// visually indistinguishable from `use harness::telemetry::TelemetryError;` at
// call sites — which is exactly the confusion the module-level docs warn
// against.
pub use config::{
    load_project_telemetry_config, load_telemetry_config, validate_telemetry_config,
    AccessControlConfig, CustomMetricKind, CustomMetricSpec, LogsConfig, MetricsConfig,
    PiiFilterConfig, ProjectTelemetryConfig, RetentionConfig, TelemetryConfig, TelemetryError,
    TracesConfig,
};
pub use types::{AttributeValue, TelemetryEntity, TelemetrySample, TelemetrySignalKind};
