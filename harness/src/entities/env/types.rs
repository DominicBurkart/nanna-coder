//! Environment entity types
//!
//! Placeholder for environment entity type definitions.
//! Full implementation tracked in issue #25.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Environment/deployment entity (placeholder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #25
}

#[async_trait]
impl Entity for EnvEntity {
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

impl EnvEntity {
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Env),
        }
    }
}

impl Default for EnvEntity {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_entity_new() {
        let entity = EnvEntity::new();
        assert_eq!(entity.metadata().entity_type, crate::entities::EntityType::Env);
    }

    #[test]
    fn test_env_entity_default() {
        let entity = EnvEntity::default();
        assert_eq!(entity.metadata().entity_type, crate::entities::EntityType::Env);
    }

    #[test]
    fn test_env_entity_to_json() {
        let entity = EnvEntity::new();
        let json = entity.to_json().unwrap();
        assert!(json.contains("\"Env\""));
    }

    #[test]
    fn test_env_entity_metadata_mut() {
        let mut entity = EnvEntity::new();
        entity.metadata_mut().tags.push("test".to_string());
        assert_eq!(entity.metadata().tags, vec!["test"]);
    }
}
