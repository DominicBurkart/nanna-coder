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
    fn test_test_entity_new() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
        assert_eq!(entity.entity_type(), EntityType::Test);
        assert!(!entity.id().is_empty());
    }

    #[test]
    fn test_test_entity_default_matches_new() {
        let from_new = TestEntity::new();
        let from_default = TestEntity::default();
        assert_eq!(from_new.entity_type(), from_default.entity_type());
    }

    #[test]
    fn test_test_entity_to_json_roundtrip() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        // entity_type flattened from EntityMetadata should be "Test"
        assert_eq!(parsed["entity_type"], "Test");
    }

    #[test]
    fn test_test_entity_ids_are_unique() {
        let e1 = TestEntity::new();
        let e2 = TestEntity::new();
        assert_ne!(e1.id(), e2.id());
    }

    #[test]
    fn test_test_entity_metadata_version_starts_at_one() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata().version, 1);
    }
}
