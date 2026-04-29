//! Bridge between `container::ContainerConfig` and `ContainerConfigEntity`.
//!
//! Converts the loosely-typed container config used by the runtime layer
//! into a typed, serde-friendly entity.
//!
//! Follow-ups (tracked in PR body, not carried in this slice):
//! - `cfg.test_image`, `cfg.container_name`, `cfg.model_to_pull`,
//!   `cfg.additional_args` are not yet represented on the entity.

use super::deployment::EnvError;
use super::security::SecurityContext;
use super::types::{ContainerConfigEntity, EnvVarRef, EnvVarSource, PortMapping, RuntimeConfig};

/// Decides whether a given env var name looks like a secret and therefore
/// MUST NOT be persisted as a plaintext [`EnvVarSource::Literal`].
///
/// Finding #264-6: every env var coming through the bridge used to be
/// converted to a `Literal`, regardless of whether the name suggested it
/// carried secret material (e.g. `DB_PASSWORD`, `API_TOKEN`). Pushing that
/// classification responsibility onto every caller is a security-critical
/// gap. The default classifier ([`HeuristicSecretClassifier`]) is used by
/// [`from_container_config`] and refuses obvious secret-shaped names so
/// they cannot silently slip into persisted entities. Callers that need
/// the legacy permissive behavior (e.g. for migration) can opt in via
/// [`from_container_config_with_classifier`] with a
/// [`PermissiveSecretClassifier`].
pub trait SecretClassifier {
    /// Returns `true` if the named variable is likely a secret and should
    /// be refused as a `Literal`.
    fn is_likely_secret(&self, var_name: &str) -> bool;
}

/// Default classifier: refuses any env var whose name (case-insensitive)
/// contains one of `PASSWORD`, `TOKEN`, `SECRET`, `KEY`, or `CREDENTIAL`.
///
/// This is intentionally a coarse, best-effort heuristic — false-positives
/// (e.g. `MY_PUBLIC_KEY_PATH`) are preferred to false-negatives because
/// the failure mode for false-negatives is a leaked credential.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeuristicSecretClassifier;

impl SecretClassifier for HeuristicSecretClassifier {
    fn is_likely_secret(&self, var_name: &str) -> bool {
        let upper = var_name.to_ascii_uppercase();
        const NEEDLES: &[&str] = &["PASSWORD", "TOKEN", "SECRET", "KEY", "CREDENTIAL"];
        NEEDLES.iter().any(|needle| upper.contains(needle))
    }
}

/// Permissive classifier that never flags a name as secret. Use only for
/// migration paths or tests that intentionally exercise the legacy
/// behavior. NEVER use in production code paths.
#[derive(Debug, Default, Clone, Copy)]
pub struct PermissiveSecretClassifier;

impl SecretClassifier for PermissiveSecretClassifier {
    fn is_likely_secret(&self, _var_name: &str) -> bool {
        false
    }
}

/// Convert a [`crate::container::ContainerConfig`] into a
/// [`ContainerConfigEntity`] using the default
/// [`HeuristicSecretClassifier`].
///
/// Mapping (explicit):
/// - `cfg.base_image` -> `entity.image`
/// - `cfg.port_mapping` -> `entity.runtime.ports` (0 or 1 entry)
/// - `cfg.env_vars` -> `entity.env_refs` as [`EnvVarSource::Literal`] entries
///   (refused with [`EnvError::LikelySecretInLiteral`] for secret-shaped names)
/// - `cfg.startup_timeout` (seconds) -> `entity.runtime.startup_timeout_secs`
/// - `cfg.health_check_timeout` (seconds) -> `entity.runtime.health_check_timeout_secs`
/// - `cpu_limit` / `memory_limit_mb` -> `None` (not in source)
/// - `volumes` -> `vec![]` (not in source)
/// - `security` -> [`SecurityContext::default()`] (restrictive); enforced
///   by [`SecurityContext::validate`] (finding #264-8)
///
/// Errors:
/// - [`EnvError::LikelySecretInLiteral`] if any env var name matches the
///   default heuristic but the value would be stored as a literal. Caller
///   must rewrite the offending var to use [`EnvVarSource::SecretRef`].
/// - [`EnvError::InsecureSecurityContext`] if the constructed default
///   security context fails [`SecurityContext::validate`] (defensive — the
///   default is always valid; this guards against future regressions).
pub fn from_container_config(
    cfg: &crate::container::ContainerConfig,
) -> Result<ContainerConfigEntity, EnvError> {
    from_container_config_with_classifier(cfg, &HeuristicSecretClassifier)
}

/// Convert a [`crate::container::ContainerConfig`] into a
/// [`ContainerConfigEntity`] using a caller-supplied
/// [`SecretClassifier`]. See [`from_container_config`] for the default
/// behavior.
pub fn from_container_config_with_classifier<C: SecretClassifier>(
    cfg: &crate::container::ContainerConfig,
    classifier: &C,
) -> Result<ContainerConfigEntity, EnvError> {
    let ports = match cfg.port_mapping {
        Some((host, container)) => vec![PortMapping { host, container }],
        None => Vec::new(),
    };

    let mut env_refs = Vec::with_capacity(cfg.env_vars.len());
    for (name, value) in &cfg.env_vars {
        if classifier.is_likely_secret(name) {
            return Err(EnvError::LikelySecretInLiteral {
                var_name: name.clone(),
            });
        }
        env_refs.push(EnvVarRef {
            name: name.clone(),
            source: EnvVarSource::literal(value.clone()),
        });
    }

    let runtime = RuntimeConfig {
        cpu_limit: None,
        memory_limit_mb: None,
        ports,
        volumes: Vec::new(),
        startup_timeout_secs: Some(cfg.startup_timeout.as_secs()),
        health_check_timeout_secs: Some(cfg.health_check_timeout.as_secs()),
    };

    let security = SecurityContext::default();
    // Finding #264-8: enforce the SecurityContext invariant at the bridge
    // boundary so a future change to `Default` cannot silently produce an
    // insecure entity.
    security.validate()?;

    let mut entity = ContainerConfigEntity::new(cfg.base_image.clone());
    entity.runtime = runtime;
    entity.security = security;
    entity.env_refs = env_refs;
    Ok(entity)
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
        let entity = from_container_config(&cfg).expect("safe env vars");

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
        let entity_no_ports = from_container_config(&cfg_no_ports).expect("safe env vars");
        assert!(entity_no_ports.runtime.ports.is_empty());
    }

    #[test]
    fn test_from_container_config_security_defaults_restrictive() {
        let cfg = sample_config();
        let entity = from_container_config(&cfg).expect("safe env vars");

        assert!(entity.security.read_only_root_fs);
        assert!(entity.security.run_as_non_root);
        assert_eq!(entity.security.capabilities, CapabilitySet::None);
        assert!(entity.security.allowed_paths.is_empty());
    }

    #[test]
    fn test_from_container_config_env_vars_literal() {
        let cfg = sample_config();
        let entity = from_container_config(&cfg).expect("safe env vars");

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
        let entity = from_container_config(&cfg).expect("safe env vars");

        assert_eq!(entity.runtime.startup_timeout_secs, Some(30));
        assert_eq!(entity.runtime.health_check_timeout_secs, Some(5));
    }

    // -----------------------------------------------------------------
    // Finding #264-6: secret-shaped env-var refusal
    // -----------------------------------------------------------------

    #[test]
    fn test_heuristic_classifier_flags_secret_shaped_names() {
        let c = HeuristicSecretClassifier;
        for name in [
            "DB_PASSWORD",
            "API_TOKEN",
            "MY_SECRET",
            "PRIVATE_KEY",
            "AWS_CREDENTIAL",
            "password",
            "token",
            "secret",
            "key",
            "credential",
            "MixedCasePassWord",
        ] {
            assert!(
                c.is_likely_secret(name),
                "expected `{}` to be flagged as secret-shaped",
                name
            );
        }
    }

    #[test]
    fn test_heuristic_classifier_passes_safe_names() {
        let c = HeuristicSecretClassifier;
        for name in ["LOG_LEVEL", "DB_URL", "PORT", "RUST_LOG", "HOSTNAME"] {
            assert!(
                !c.is_likely_secret(name),
                "expected `{}` to NOT be flagged",
                name
            );
        }
    }

    #[test]
    fn test_permissive_classifier_passes_everything() {
        let c = PermissiveSecretClassifier;
        for name in ["DB_PASSWORD", "API_TOKEN", "MY_SECRET", "FOO", "BAR"] {
            assert!(!c.is_likely_secret(name));
        }
    }

    #[test]
    fn test_from_container_config_refuses_secret_shaped_env_var() {
        // SECURITY (finding #264-6): a config carrying `DB_PASSWORD` as
        // a plain env_var must be REFUSED, not silently demoted to a
        // Literal.
        let mut cfg = sample_config();
        cfg.env_vars
            .push(("DB_PASSWORD".to_string(), "hunter2".to_string()));

        match from_container_config(&cfg) {
            Err(EnvError::LikelySecretInLiteral { var_name }) => {
                assert_eq!(var_name, "DB_PASSWORD");
            }
            other => panic!(
                "expected LikelySecretInLiteral for DB_PASSWORD, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_from_container_config_with_permissive_preserves_legacy() {
        // The permissive classifier exists for migration paths — it must
        // accept secret-shaped names as Literals.
        let mut cfg = sample_config();
        cfg.env_vars
            .push(("API_TOKEN".to_string(), "abc123".to_string()));

        let entity = from_container_config_with_classifier(&cfg, &PermissiveSecretClassifier)
            .expect("permissive classifier accepts secret-shaped names");
        assert_eq!(entity.env_refs.len(), 3);
        assert_eq!(entity.env_refs[2].name, "API_TOKEN");
        match &entity.env_refs[2].source {
            EnvVarSource::Literal { value } => assert_eq!(value, "abc123"),
            other => panic!("expected Literal, got {:?}", other),
        }
    }

    #[test]
    fn test_secret_value_never_appears_when_refused() {
        // Defense-in-depth: when the bridge refuses a secret-shaped var,
        // the secret VALUE must never appear in the returned error.
        let mut cfg = sample_config();
        cfg.env_vars.push((
            "DB_PASSWORD".to_string(),
            "super-secret-sentinel-value".to_string(),
        ));

        let err = from_container_config(&cfg).unwrap_err();
        let msg = format!("{err}");
        assert!(
            !msg.contains("super-secret-sentinel-value"),
            "error message must not include the secret value: {msg}"
        );
    }
}
