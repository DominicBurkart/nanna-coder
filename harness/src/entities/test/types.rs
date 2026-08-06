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
    fn new_has_test_entity_type() {
        let entity = TestEntity::new();
        assert_eq!(
            entity.metadata().entity_type,
            EntityType::Test,
            "TestEntity::new() must produce a Test entity type"
        );
    }

    #[test]
    fn default_is_equivalent_to_new() {
        let from_new = TestEntity::new();
        let from_default = TestEntity::default();
        // Both must be Test entities (we can't compare ids, which are unique UUIDs).
        assert_eq!(from_new.metadata().entity_type, from_default.metadata().entity_type);
    }

    #[test]
    fn entity_type_method_returns_test() {
        let entity = TestEntity::new();
        assert_eq!(entity.entity_type(), EntityType::Test);
    }

    #[test]
    fn id_is_non_empty() {
        let entity = TestEntity::new();
        assert!(!entity.id().is_empty(), "entity id must not be empty");
    }

    #[test]
    fn to_json_succeeds() {
        let entity = TestEntity::new();
        let json = entity.to_json().expect("TestEntity must serialize to JSON");
        // The JSON must contain the entity type identifier.
        assert!(
            json.contains("Test"),
            "JSON must contain the entity type; got: {json}"
        );
    }

    #[test]
    fn json_round_trip_preserves_entity_type() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        let deserialized: TestEntity = serde_json::from_str(&json)
            .expect("TestEntity JSON must deserialize back");
        assert_eq!(deserialized.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn metadata_mut_allows_mutation() {
        let mut entity = TestEntity::new();
        // Verify that metadata_mut() returns a mutable reference we can use.
        let meta = entity.metadata_mut();
        // EntityMetadata carries entity_type; confirm it is still Test after the
        // borrow.
        assert_eq!(meta.entity_type, EntityType::Test);
    }

    #[tokio::test]
    async fn usable_as_boxed_entity_trait_object() {
        let entity = TestEntity::new();
        let boxed: Box<dyn Entity> = Box::new(entity);
        assert_eq!(boxed.entity_type(), EntityType::Test);
        assert!(!boxed.id().is_empty());
        assert!(boxed.to_json().is_ok());
    }
}
