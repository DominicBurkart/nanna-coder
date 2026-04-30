//! Bridge between `container::ContainerConfig` and `ContainerConfigEntity`.
//!
//! Converts the loosely-typed container config used by the runtime layer
//! into a typed, serde-friendly entity.
//!
//! Follow-ups (tracked in PR body, not carried in this slice):
//! - `cfg.test_image`, `cfg.container_name`, `cfg.model_to_pull`,
//!   `cfg.additional_args` are not yet represented on the entity.

use super::security::SecurityContext;
use super::types::{ContainerConfigEntity, EnvVarRef, EnvVarSource, PortMapping, RuntimeConfig};

/// Convert a [`crate::container::ContainerConfig`] into a
/// [`ContainerConfigEntity`].
///
/// Mapping (explicit):
/// - `cfg.base_image` -> `entity.image`
/// - `cfg.port_mapping` -> `entity.runtime.ports` (0 or 1 entry)
/// - `cfg.env_vars` -> `entity.env_refs` as [`EnvVarSource::Literal`] entries
/// - `cfg.startup_timeout` (seconds) -> `entity.runtime.startup_timeout_secs`
/// - `cfg.health_check_timeout` (seconds) -> `entity.runtime.health_check_timeout_secs`
/// - `cpu_limit` / `memory_limit_mb` -> `None` (not in source)
/// - `volumes` -> `vec![]` (not in source)
/// - `security` -> [`SecurityContext::default()`] (restrictive)
///
/// Callers that need plaintext env values treated as secrets must rewrite
/// the resulting [`EnvVarRef`] entries to use [`EnvVarSource::SecretRef`];
/// this bridge makes no such inference.
pub fn from_container_config(cfg: &crate::container::ContainerConfig) -> ContainerConfigEntity {
    let ports = match cfg.port_mapping {
        Some((host, container)) => vec![PortMapping { host, container }],
        None => Vec::new(),
    };

    let env_refs = cfg
        .env_vars
        .iter()
        .map(|(name, value)| EnvVarRef {
            name: name.clone(),
            source: EnvVarSource::literal(value.clone()),
        })
        .collect();

    let runtime = RuntimeConfig {
        cpu_limit: None,
        memory_limit_mb: None,
        ports,
        volumes: Vec::new(),
        startup_timeout_secs: Some(cfg.startup_timeout.as_secs()),
        health_check_timeout_secs: Some(cfg.health_check_timeout.as_secs()),
    };

    let mut entity = ContainerConfigEntity::new(cfg.base_image.clone());
    entity.runtime = runtime;
    entity.security = SecurityContext::default();
    entity.env_refs = env_refs;
    entity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::ContainerConfig;
    use crate::entities::env::security::CapabilitySet;
    use std::time::Duration;

    fn sample_config() -> ContainerConfig {
        ContainerConfig {
            base_image: "alpine:3.19".to_string(),
            test_image: None,
            container_name: "svc".to_string(),
            port_mapping: Some((8080, 80)),
            model_to_pull: None,
            startup_timeout: Duration::from_secs(30),
            health_check_timeout: Duration::from_secs(5),
            env_vars: vec![
                ("LOG_LEVEL".to_string(), "info".to_string()),
                ("DB_URL".to_string(), "postgres://...".to_string()),
            ],
            additional_args: Vec::new(),
        }
    }

    #[test]
    fn test_from_container_config_populates_image_and_ports() {
        let cfg = sample_config();
        let entity = from_container_config(&cfg);

        assert_eq!(entity.image, "alpine:3.19");
        assert_eq!(entity.runtime.ports.len(), 1);
        assert_eq!(
            entity.runtime.ports[0],
            PortMapping {
                host: 8080,
                container: 80,
            }
        );

        // No source for cpu / memory / volumes — must be unset/empty.
        assert_eq!(entity.runtime.cpu_limit, None);
        assert_eq!(entity.runtime.memory_limit_mb, None);
        assert!(entity.runtime.volumes.is_empty());

        // With no port_mapping, ports must be empty.
        let mut cfg_no_ports = sample_config();
        cfg_no_ports.port_mapping = None;
        let entity_no_ports = from_container_config(&cfg_no_ports);
        assert!(entity_no_ports.runtime.ports.is_empty());
    }

    #[test]
    fn test_from_container_config_security_defaults_restrictive() {
        let cfg = sample_config();
        let entity = from_container_config(&cfg);

        assert!(entity.security.read_only_root_fs);
        assert!(entity.security.run_as_non_root);
        assert_eq!(entity.security.capabilities, CapabilitySet::None);
        assert!(entity.security.allowed_paths.is_empty());
    }

    #[test]
    fn test_from_container_config_env_vars_literal() {
        let cfg = sample_config();
        let entity = from_container_config(&cfg);

        assert_eq!(entity.env_refs.len(), 2);
        assert_eq!(entity.env_refs[0].name, "LOG_LEVEL");
        match &entity.env_refs[0].source {
            EnvVarSource::Literal { value } => assert_eq!(value, "info"),
            other => panic!("expected Literal, got {:?}", other),
        }
        assert_eq!(entity.env_refs[1].name, "DB_URL");
        match &entity.env_refs[1].source {
            EnvVarSource::Literal { value } => assert_eq!(value, "postgres://..."),
            other => panic!("expected Literal, got {:?}", other),
        }
    }

    #[test]
    fn test_from_container_config_timeouts_mapped() {
        let cfg = sample_config();
        let entity = from_container_config(&cfg);

        assert_eq!(entity.runtime.startup_timeout_secs, Some(30));
        assert_eq!(entity.runtime.health_check_timeout_secs, Some(5));
    }
}
