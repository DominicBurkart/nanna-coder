//! AST entity types
//!
//! Placeholder for AST entity type definitions.
//! Full implementation tracked in issue #23.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// AST entity (placeholder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #23
}

#[async_trait]
impl Entity for AstEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self).map_err(|e| {
            crate::entities::EntityError::SerializationError(e.to_string())
        })
    }
}

impl AstEntity {
    pub fn new() -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Ast),
        }
    }
}

impl Default for AstEntity {
    fn default() -> Self {
        Self::new()
    }
}
