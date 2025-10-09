//! Context entity types
//!
//! Placeholder for context entity type definitions.
//! Full implementation tracked in issue #26.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Project context entity (placeholder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #26
}

#[async_trait]
impl Entity for ContextEntity {
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

impl ContextEntity {
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Context),
        }
    }
}

impl Default for ContextEntity {
    fn default() -> Self {
        Self::new()
    }
}
