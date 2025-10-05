//! Git entity types
//!
//! Placeholder for git entity type definitions.
//! Full implementation tracked in issue #22.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Git repository entity (placeholder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRepository {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #22
}

#[async_trait]
impl Entity for GitRepository {
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

impl GitRepository {
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Git),
        }
    }
}

impl Default for GitRepository {
    fn default() -> Self {
        Self::new()
    }
}
