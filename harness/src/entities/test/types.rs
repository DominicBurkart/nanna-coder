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
        assert_eq!(entity.metadata().entity_type, crate::entities::EntityType::Test);
    }

    #[test]
    fn test_test_entity_default() {
        let entity = TestEntity::default();
        assert_eq!(entity.metadata().entity_type, crate::entities::EntityType::Test);
    }

    #[test]
    fn test_test_entity_to_json() {
        let entity = TestEntity::new();
        let json = entity.to_json().unwrap();
        assert!(json.contains("\"Test\""));
    }

    #[test]
    fn test_test_entity_metadata_mut() {
        let mut entity = TestEntity::new();
        entity.metadata_mut().tags.push("unit-test".to_string());
        assert_eq!(entity.metadata().tags, vec!["unit-test"]);
    }
}
