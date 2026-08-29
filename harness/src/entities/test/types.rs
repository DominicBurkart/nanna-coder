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
    fn new_creates_entity_with_test_type() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn default_matches_new() {
        let entity = TestEntity::default();
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn to_json_roundtrip_preserves_type() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        let back: TestEntity = serde_json::from_str(&json).unwrap();
        assert_eq!(back.metadata.entity_type, EntityType::Test);
    }

    #[test]
    fn metadata_mut_allows_tag_mutation() {
        let mut entity = TestEntity::new();
        entity.metadata_mut().tags.push("smoke".to_string());
        assert_eq!(entity.metadata().tags, vec!["smoke".to_string()]);
    }

    #[test]
    fn entity_type_convenience_returns_test() {
        let entity = TestEntity::new();
        assert_eq!(entity.entity_type(), EntityType::Test);
    }

    #[test]
    fn clone_produces_independent_copy() {
        let original = TestEntity::new();
        let cloned = original.clone();
        assert_eq!(cloned.metadata.entity_type, EntityType::Test);
        assert_eq!(original.metadata().id, cloned.metadata().id);
    }
}
