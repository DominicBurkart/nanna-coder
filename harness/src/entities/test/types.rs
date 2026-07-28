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
    use crate::entities::{Entity, EntityType};

    #[test]
    fn test_test_entity_new_has_correct_type() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata.entity_type, EntityType::Test);
    }

    #[test]
    fn test_test_entity_default_equals_new() {
        let a = TestEntity::new();
        let b = TestEntity::default();
        assert_eq!(a.metadata.entity_type, b.metadata.entity_type);
    }

    #[test]
    fn test_test_entity_to_json_roundtrip() {
        let entity = TestEntity::new();
        let json = entity.to_json().expect("serialize TestEntity");
        let decoded: TestEntity = serde_json::from_str(&json).expect("deserialize TestEntity");
        assert_eq!(decoded.metadata.entity_type, EntityType::Test);
    }

    #[test]
    fn test_test_entity_trait_methods() {
        let entity = TestEntity::new();
        assert_eq!(entity.entity_type(), EntityType::Test);
        assert!(!entity.id().is_empty());
    }

    #[test]
    fn test_test_entity_as_box_dyn_entity() {
        let entity: Box<dyn Entity> = Box::new(TestEntity::new());
        assert_eq!(entity.entity_type(), EntityType::Test);
        assert!(entity.to_json().is_ok());
    }
}
