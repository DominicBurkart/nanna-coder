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
    fn new_has_test_entity_type() {
        let e = TestEntity::new();
        assert_eq!(e.metadata.entity_type, EntityType::Test);
    }

    #[test]
    fn default_equals_new() {
        let a = TestEntity::new();
        let b = TestEntity::default();
        assert_eq!(a.metadata.entity_type, b.metadata.entity_type);
    }

    #[test]
    fn entity_type_accessor() {
        let e = TestEntity::new();
        assert_eq!(e.entity_type(), EntityType::Test);
    }

    #[test]
    fn id_is_nonempty() {
        let e = TestEntity::new();
        assert!(!e.id().is_empty());
    }

    #[test]
    fn to_json_round_trips() {
        let e = TestEntity::new();
        let json = e.to_json().expect("serialize");
        let back: TestEntity = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.metadata.entity_type, e.metadata.entity_type);
    }

    #[test]
    fn metadata_mut_allows_mutation() {
        let mut e = TestEntity::new();
        let id_before = e.id().to_string();
        e.metadata_mut().entity_type = EntityType::Test;
        assert_eq!(e.id(), id_before);
    }
}
