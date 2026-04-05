//! Telemetry entity types (TODO)
//!
//! Placeholder for telemetry entity type definitions.
//! Full implementation tracked in issue #27 (deferred).

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Telemetry entity (placeholder - TODO)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #27
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_entity_new() {
        let entity = TelemetryEntity::new();
        assert_eq!(entity.metadata().entity_type, crate::entities::EntityType::Telemetry);
    }

    #[test]
    fn test_telemetry_entity_default() {
        let entity = TelemetryEntity::default();
        assert_eq!(entity.metadata().entity_type, crate::entities::EntityType::Telemetry);
    }

    #[test]
    fn test_telemetry_entity_to_json() {
        let entity = TelemetryEntity::new();
        let json = entity.to_json().unwrap();
        assert!(json.contains("\"Telemetry\""));
    }

    #[test]
    fn test_telemetry_entity_metadata_mut() {
        let mut entity = TelemetryEntity::new();
        entity.metadata_mut().tags.push("telemetry".to_string());
        assert_eq!(entity.metadata().tags, vec!["telemetry"]);
    }
}
