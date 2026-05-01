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

    #[test]
    fn test_entity_new_has_test_type() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata.entity_type, EntityType::Test);
    }

    #[test]
    fn test_entity_default_matches_new() {
        let a = TestEntity::new();
        let b = TestEntity::default();
        assert_eq!(a.metadata.entity_type, b.metadata.entity_type);
    }

    #[test]
    fn test_entity_to_json_succeeds() {
        let entity = TestEntity::new();
        let result = entity.to_json();
        assert!(result.is_ok(), "TestEntity should serialize to JSON");
        let json = result.unwrap();
        assert!(json.contains("\"entity_type\""), "JSON should contain entity_type");
    }

    #[test]
    fn test_entity_json_roundtrip() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        let deserialized: TestEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(entity.metadata.entity_type, deserialized.metadata.entity_type);
    }

    #[test]
    fn test_entity_metadata_id_is_nonempty() {
        let entity = TestEntity::new();
        assert!(!entity.metadata.id.is_empty(), "Entity ID should not be empty");
    }

    #[test]
    fn test_two_entities_have_distinct_ids() {
        let a = TestEntity::new();
        let b = TestEntity::new();
        assert_ne!(a.metadata.id, b.metadata.id, "Each entity should have a unique ID");
    }

    #[tokio::test]
    async fn test_entity_trait_methods() {
        let mut entity = TestEntity::new();

        // metadata() returns the right type
        assert_eq!(entity.metadata().entity_type, EntityType::Test);

        // metadata_mut() gives mutable access
        entity.metadata_mut().entity_type = EntityType::Test; // no-op change, just verifies access

        // to_json() works
        assert!(entity.to_json().is_ok());
    }

    #[test]
    fn test_entity_usable_as_trait_object() {
        let entity = TestEntity::new();
        let boxed: Box<dyn Entity> = Box::new(entity);
        assert_eq!(boxed.metadata().entity_type, EntityType::Test);
        assert!(boxed.to_json().is_ok());
    }
}
