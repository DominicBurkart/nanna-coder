//! Environment entity types
//!
//! Placeholder for environment entity type definitions.
//! Full implementation tracked in issue #25.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::security::SecurityContext;

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

// ---------------------------------------------------------------------------
// ContainerConfigEntity and supporting types (issue #25 first slice)
// ---------------------------------------------------------------------------

/// Container configuration entity
///
/// A typed, serde-friendly representation of a container configuration.
/// Built to replace ad-hoc `container::ContainerConfig` bags with a queryable
/// entity carrying an explicit `SecurityContext` and references (never
/// plaintext) to secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerConfigEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Container image reference (e.g. `"ollama/ollama:latest"`).
    pub image: String,

    /// Runtime resource and networking configuration.
    pub runtime: RuntimeConfig,

    /// Security posture for this container. Defaults to a restrictive
    /// configuration; callers must opt in to elevated capabilities.
    pub security: SecurityContext,

    /// Environment variable references. Secrets MUST be referenced via
    /// [`EnvVarSource::SecretRef`]; plaintext values are restricted to
    /// [`EnvVarSource::Literal`] and MUST NOT be used for secret material.
    pub env_refs: Vec<EnvVarRef>,
}

impl ContainerConfigEntity {
    /// Create a new ContainerConfigEntity with a restrictive default
    /// security context and empty runtime/env configuration.
    pub fn new(image: String) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Env),
            image,
            runtime: RuntimeConfig::default(),
            security: SecurityContext::default(),
            env_refs: Vec::new(),
        }
    }
}

#[async_trait]
impl Entity for ContainerConfigEntity {
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

/// Runtime configuration for a container (resources, networking, volumes).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// Optional CPU limit (number of cores, integer for now).
    pub cpu_limit: Option<u32>,

    /// Optional memory limit in megabytes.
    pub memory_limit_mb: Option<u64>,

    /// Port mappings (host -> container).
    pub ports: Vec<PortMapping>,

    /// Volume mounts.
    pub volumes: Vec<VolumeMount>,

    /// Optional container startup timeout (seconds).
    pub startup_timeout_secs: Option<u64>,

    /// Optional health-check timeout (seconds).
    pub health_check_timeout_secs: Option<u64>,
}

/// Host-to-container port mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMapping {
    pub host: u16,
    pub container: u16,
}

/// Volume mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMount {
    pub host_path: PathBuf,
    pub container_path: PathBuf,
    pub read_only: bool,
}

/// Environment variable reference — the variable name and its source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVarRef {
    pub name: String,
    pub source: EnvVarSource,
}

/// Source for an environment variable value.
///
/// `Literal` values are stored inline and MUST NOT carry secret material.
/// `SecretRef` stores only a `vault` identifier and a `key`; the actual
/// secret value is resolved at runtime by a secret backend and is never
/// serialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EnvVarSource {
    /// Inline, non-secret value.
    Literal { value: String },

    /// Reference to a secret in an external vault. Only the locator is
    /// persisted; the value is never stored or serialized here.
    SecretRef { vault: String, key: String },
}

impl EnvVarSource {
    /// Construct a [`EnvVarSource::Literal`] from a value.
    pub fn literal(value: impl Into<String>) -> Self {
        EnvVarSource::Literal {
            value: value.into(),
        }
    }

    /// Construct a [`EnvVarSource::SecretRef`] from a vault and key.
    pub fn secret_ref(vault: impl Into<String>, key: impl Into<String>) -> Self {
        EnvVarSource::SecretRef {
            vault: vault.into(),
            key: key.into(),
        }
    }
}

#[cfg(test)]
mod new_types_tests {
    use super::*;
    use crate::entities::env::security::{Capability, CapabilitySet};

    #[test]
    fn test_container_config_entity_serde_roundtrip() {
        let mut entity = ContainerConfigEntity::new("alpine:3.19".to_string());
        entity.runtime.cpu_limit = Some(2);
        entity.runtime.memory_limit_mb = Some(512);
        entity.runtime.ports.push(PortMapping {
            host: 8080,
            container: 80,
        });
        entity.runtime.volumes.push(VolumeMount {
            host_path: PathBuf::from("/host"),
            container_path: PathBuf::from("/container"),
            read_only: true,
        });
        entity.runtime.startup_timeout_secs = Some(30);
        entity.runtime.health_check_timeout_secs = Some(10);
        entity.env_refs.push(EnvVarRef {
            name: "LOG_LEVEL".to_string(),
            source: EnvVarSource::literal("info"),
        });
        entity.env_refs.push(EnvVarRef {
            name: "DB_PASSWORD".to_string(),
            source: EnvVarSource::secret_ref("primary", "db_password"),
        });
        entity.security.capabilities = CapabilitySet::Minimal(vec![Capability::NetBindService]);

        let json = entity.to_json().expect("serialize");
        let decoded: ContainerConfigEntity = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(decoded.image, entity.image);
        assert_eq!(decoded.runtime.cpu_limit, Some(2));
        assert_eq!(decoded.runtime.memory_limit_mb, Some(512));
        assert_eq!(decoded.runtime.ports, entity.runtime.ports);
        assert_eq!(decoded.runtime.volumes, entity.runtime.volumes);
        assert_eq!(decoded.runtime.startup_timeout_secs, Some(30));
        assert_eq!(decoded.runtime.health_check_timeout_secs, Some(10));
        assert_eq!(decoded.env_refs, entity.env_refs);
        assert!(decoded.security.read_only_root_fs);
        assert!(decoded.security.run_as_non_root);
        match decoded.security.capabilities {
            CapabilitySet::Minimal(caps) => {
                assert_eq!(caps, vec![Capability::NetBindService]);
            }
            other => panic!("expected Minimal, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_container_config_entity_implements_entity() {
        let entity = ContainerConfigEntity::new("alpine:3.19".to_string());
        assert_eq!(entity.metadata().entity_type, EntityType::Env);
        assert_eq!(entity.entity_type(), EntityType::Env);
        assert!(entity.to_json().is_ok());

        // Usable as a trait object.
        let boxed: Box<dyn Entity> = Box::new(entity);
        assert_eq!(boxed.entity_type(), EntityType::Env);
    }

    #[test]
    fn test_env_var_source_secret_never_inlines_value() {
        let secret = EnvVarSource::secret_ref("primary", "api_token");
        let encoded = serde_json::to_string(&secret).expect("serialize SecretRef");

        // Confirm only the locator fields are present — no `value` key and
        // no plaintext token material.
        assert!(encoded.contains("\"vault\""));
        assert!(encoded.contains("\"key\""));
        assert!(encoded.contains("primary"));
        assert!(encoded.contains("api_token"));
        assert!(
            !encoded.to_lowercase().contains("value"),
            "SecretRef must not serialize a `value` field: {}",
            encoded
        );
        assert!(
            !encoded.to_lowercase().contains("literal"),
            "SecretRef must not serialize as a Literal variant: {}",
            encoded
        );
    }
}
