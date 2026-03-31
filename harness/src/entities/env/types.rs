//! Environment entity types
//!
//! Placeholder for environment entity type definitions.
//! Full implementation tracked in issue #25.

use crate::entities::{EntityMetadata, EntityType};
use crate::impl_entity;
use serde::{Deserialize, Serialize};

/// Environment/deployment entity (placeholder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    // Additional fields will be added in issue #25
}

impl_entity!(EnvEntity);

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
