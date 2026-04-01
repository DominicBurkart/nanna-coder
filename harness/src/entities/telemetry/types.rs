//! Telemetry entity types (TODO)
//!
//! Placeholder for telemetry entity type definitions.
//! Full implementation tracked in issue #27 (deferred).

use crate::entities::{EntityMetadata, EntityType};
use crate::impl_entity;
use serde::{Deserialize, Serialize};

/// Telemetry entity (placeholder - TODO)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #27
}

impl_entity!(TelemetryEntity);

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
