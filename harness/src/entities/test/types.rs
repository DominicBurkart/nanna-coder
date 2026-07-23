//! Test entity types
//!
//! Placeholder for test entity type definitions.
//! Full implementation tracked in issue #24.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Test result entity (placeholder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #24
}

#[async_trait]
impl Entity for TestEntity {
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

impl TestEntity {
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Test),
        }
    }
}

impl Default for TestEntity {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::EntityType;

    #[test]
    fn test_new_creates_test_entity() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata.entity_type, EntityType::Test);
        assert_eq!(entity.metadata.version, 1);
        assert!(entity.metadata.tags.is_empty());
    }

    #[test]
    fn test_default_matches_new() {
        let entity = TestEntity::default();
        assert_eq!(entity.metadata.entity_type, EntityType::Test);
    }

    #[test]
    fn test_to_json_serializes_entity_type() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        assert!(json.contains("\"entity_type\""));
        assert!(json.contains("Test"));
    }

    #[test]
    fn test_metadata_returns_reference() {
        let entity = TestEntity::new();
        let metadata = entity.metadata();
        assert_eq!(metadata.entity_type, EntityType::Test);
    }

    #[test]
    fn test_metadata_mut_allows_modification() {
        let mut entity = TestEntity::new();
        entity.metadata_mut().tags.push("ci".to_string());
        assert_eq!(entity.metadata.tags, vec!["ci".to_string()]);
    }
}
