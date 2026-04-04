//! Telemetry entity types
//!
//! Defines the `TelemetryEntity` used to persist observability data (spans,
//! metrics, log records) produced during an agent run as first-class entities
//! in the entity store.  This allows the RAG layer to incorporate past
//! performance data when planning future modifications.
//!
//! ## Planned fields (tracked in issue #27)
//!
//! - `span_id` / `trace_id` — OpenTelemetry correlation identifiers
//! - `operation` — name of the agent operation being measured
//! - `duration_ms` — wall-clock duration of the operation
//! - `status` — success / failure / timeout
//! - `attributes` — arbitrary key-value pairs forwarded from the tracer
//!
//! Until issue #27 is resolved, `TelemetryEntity` carries only the shared
//! `EntityMetadata` (id, type, timestamps).  No telemetry-specific data is
//! stored yet.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A persisted telemetry record produced during an agent run.
///
/// Stores observability data (spans, metrics, log records) as an entity so
/// that the RAG layer can surface historical performance information during
/// planning.  Additional fields (span/trace IDs, duration, status,
/// attributes) will be added once the surrounding observability pipeline is
/// stabilised — see issue #27.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields (span_id, trace_id, operation, duration_ms, status,
    // attributes) will be added in issue #27 once the observability pipeline
    // is stabilised.
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
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Telemetry),
        }
    }
}

impl Default for TelemetryEntity {
    fn default() -> Self {
        Self::new()
    }
}
