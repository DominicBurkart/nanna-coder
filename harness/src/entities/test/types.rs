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
    fn new_creates_test_entity_with_correct_type() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn default_equals_new() {
        let a = TestEntity::new();
        let b = TestEntity::default();
        assert_eq!(a.metadata().entity_type, b.metadata().entity_type);
    }

    #[test]
    fn to_json_round_trips() {
        let entity = TestEntity::new();
        let json = entity.to_json().expect("serialize");
        let decoded: TestEntity = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.metadata().entity_type, EntityType::Test);
    }

    #[tokio::test]
    async fn metadata_mut_allows_tag_update() {
        let mut entity = TestEntity::new();
        entity.metadata_mut().tags.push("smoke".to_string());
        assert_eq!(entity.metadata().tags, vec!["smoke"]);
    }

    #[test]
    fn entity_type_helper_returns_test() {
        let entity = TestEntity::new();
        assert_eq!(entity.entity_type(), EntityType::Test);
    }
}
