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
    fn test_entity_new_has_test_kind() {
        let entity = TestEntity::new();
        assert_eq!(entity.metadata().entity_type, EntityType::Test);
    }

    #[test]
    fn test_entity_default_matches_new() {
        let from_new = TestEntity::new();
        let from_default = TestEntity::default();
        assert_eq!(
            from_new.metadata().entity_type,
            from_default.metadata().entity_type
        );
    }

    #[test]
    fn test_entity_to_json_is_valid_json() {
        let entity = TestEntity::new();
        let json_str = entity.to_json().expect("to_json must succeed");
        let parsed: serde_json::Value =
            serde_json::from_str(&json_str).expect("output must be valid JSON");
        assert!(parsed.is_object(), "JSON output should be an object");
    }

    #[test]
    fn test_entity_metadata_mut_allows_update() {
        let mut entity = TestEntity::new();
        let id_before = entity.metadata().id.clone();
        // Mutate via metadata_mut to confirm the method is reachable
        entity.metadata_mut().entity_type = EntityType::Test;
        assert_eq!(entity.metadata().id, id_before);
    }
}
