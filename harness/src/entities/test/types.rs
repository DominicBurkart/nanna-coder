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
    fn test_new_has_test_entity_type() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn test_default_equals_new() {
        let a = TestEntity::new();
        let b = TestEntity::default();
        assert_eq!(a.metadata().entity_type, b.metadata().entity_type);
        assert_eq!(a.metadata().version, b.metadata().version);
    }

    #[test]
    fn test_to_json_is_valid_json() {
        let entity = TestEntity::new();
        let json_str = entity.to_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert!(parsed.get("entity_type").is_some());
    }

    #[test]
    fn test_metadata_mut_allows_tag_addition() {
        let mut entity = TestEntity::new();
        entity.metadata_mut().tags.push("coverage".to_string());
        assert_eq!(entity.metadata().tags, vec!["coverage"]);
    }

    #[test]
    fn test_clone_preserves_entity_type() {
        let entity = TestEntity::new();
        let cloned = entity.clone();
        assert_eq!(cloned.metadata().entity_type, EntityType::Test);
    }
}
