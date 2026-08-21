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
    fn test_test_entity_new_has_correct_type() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn test_test_entity_default_matches_new() {
        let from_new = TestEntity::new();
        let from_default = TestEntity::default();
        assert_eq!(
            from_new.metadata().entity_type,
            from_default.metadata().entity_type
        );
    }

    #[test]
    fn test_test_entity_to_json_succeeds() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        assert!(!json.is_empty());
    }

    #[test]
    fn test_test_entity_json_roundtrip() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        let deserialized: TestEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn test_test_entity_entity_type_method() {
        let entity = TestEntity::new();
        assert_eq!(entity.entity_type(), EntityType::Test);
    }

    #[test]
    fn test_test_entity_metadata_mut() {
        let mut entity = TestEntity::new();
        // Ensure metadata_mut returns a mutable reference
        let _meta_mut = entity.metadata_mut();
        // Just verify we can access it without panicking
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn test_test_entity_id_is_non_empty() {
        let entity = TestEntity::new();
        assert!(!entity.id().is_empty());
    }

    #[test]
    fn test_two_test_entities_have_distinct_ids() {
        let a = TestEntity::new();
        let b = TestEntity::new();
        assert_ne!(a.id(), b.id());
    }
}
