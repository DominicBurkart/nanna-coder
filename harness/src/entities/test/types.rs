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

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn new_produces_test_entity_type() {
        let e = TestEntity::new();
        assert_eq!(e.entity_type(), EntityType::Test);
    }

    #[test]
    fn default_matches_new() {
        let e = TestEntity::default();
        assert_eq!(e.entity_type(), EntityType::Test);
        // The ID is randomly generated; just confirm it is non-empty.
        assert!(!e.id().is_empty());
    }

    // ── Entity trait methods ─────────────────────────────────────────────────

    #[test]
    fn id_is_non_empty_and_unique() {
        let e1 = TestEntity::new();
        let e2 = TestEntity::new();
        assert!(!e1.id().is_empty());
        // Two separate instances must not share the same UUID.
        assert_ne!(e1.id(), e2.id());
    }

    #[test]
    fn entity_type_returns_test() {
        let e = TestEntity::new();
        assert_eq!(e.entity_type(), EntityType::Test);
    }

    #[test]
    fn metadata_ref_consistent_with_id() {
        let e = TestEntity::new();
        assert_eq!(e.metadata().id, e.id());
    }

    #[test]
    fn metadata_mut_allows_tag_mutation() {
        let mut e = TestEntity::new();
        assert!(e.metadata().tags.is_empty());
        e.metadata_mut().tags.push("ci".to_string());
        assert_eq!(e.metadata().tags, vec!["ci"]);
    }

    // ── Serialization ────────────────────────────────────────────────────────

    #[test]
    fn to_json_succeeds_and_contains_id() {
        let e = TestEntity::new();
        let id = e.id().to_string();
        let json = e.to_json().expect("to_json must succeed");
        assert!(
            json.contains(&id),
            "expected JSON to contain entity id {id:?}, got: {json}"
        );
    }

    #[test]
    fn to_json_roundtrip_preserves_entity_type() {
        let e = TestEntity::new();
        let json = e.to_json().expect("serialization");
        let restored: TestEntity =
            serde_json::from_str(&json).expect("deserialization must succeed");
        assert_eq!(restored.entity_type(), EntityType::Test);
        assert_eq!(restored.id(), e.id());
    }

    // ── Clone ────────────────────────────────────────────────────────────────

    #[test]
    fn clone_produces_equal_metadata() {
        let e = TestEntity::new();
        let c = e.clone();
        assert_eq!(c.id(), e.id());
        assert_eq!(c.entity_type(), e.entity_type());
    }
}
