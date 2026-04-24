//! Deployment manifest entity and approval workflow
//!
//! Defines [`DeploymentManifest`] together with its target, impact, and
//! approval types. Release deployments require an explicit approval record;
//! dev and sandbox deployments do not.

use crate::entities::{Entity, EntityError, EntityId, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A deployment of a [`super::types::ContainerConfigEntity`] to a target
/// environment.
///
/// The `container` field holds the id of the `ContainerConfigEntity` being
/// deployed (typed as [`EntityId`], which is a string alias today).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentManifest {
    #[serde(flatten)]
    pub metadata: EntityMetadata,

    /// Target environment for this deployment.
    pub target: DeploymentTarget,

    /// Service name (e.g. `"harness-api"`).
    pub service_name: String,

    /// Desired replica count.
    pub replicas: u32,

    /// Id of the `ContainerConfigEntity` being deployed. Typed as
    /// [`EntityId`] (finding #264-4).
    pub container: EntityId,
}

impl DeploymentManifest {
    /// Create a new deployment manifest.
    ///
    /// Debug-asserts that `container` is a non-empty [`EntityId`] so that
    /// misuse from test fixtures is caught loudly in debug builds.
    pub fn new(
        target: DeploymentTarget,
        service_name: String,
        replicas: u32,
        container: EntityId,
    ) -> Self {
        debug_assert!(
            !container.is_empty(),
            "DeploymentManifest::new requires a non-empty container EntityId"
        );
        Self {
            metadata: EntityMetadata::new(EntityType::Env),
            target,
            service_name,
            replicas,
            container,
        }
    }

    /// Coarse impact classification derived from [`Self::target`].
    ///
    /// This is the *intrinsic* impact of the target environment and ignores
    /// the `approval_required` flag — the flag is a workflow knob, not a
    /// license to downgrade impact. See finding #264-1.
    pub fn impact_level(&self) -> ImpactLevel {
        match self.target {
            DeploymentTarget::Dev => ImpactLevel::DevOnly,
            DeploymentTarget::Sandbox => ImpactLevel::SandboxIsolated,
            DeploymentTarget::Release { .. } => ImpactLevel::ProductionCritical,
        }
    }

    /// Whether this deployment requires an approval record before it can
    /// proceed.
    ///
    /// Any [`ImpactLevel::ProductionCritical`] deployment requires approval
    /// regardless of the `approval_required` flag — this is a security
    /// cross-check against accidental or malicious
    /// `DeploymentTarget::Release { approval_required: false }` records.
    /// See finding #264-1.
    pub fn requires_approval(&self) -> bool {
        if self.impact_level() == ImpactLevel::ProductionCritical {
            return true;
        }
        match self.target {
            DeploymentTarget::Dev | DeploymentTarget::Sandbox => false,
            DeploymentTarget::Release { approval_required } => approval_required,
        }
    }

    /// Validate this manifest against an optional approval record.
    ///
    /// - If approval is not required, `approval` is ignored and `Ok(())` is
    ///   returned.
    /// - If approval is required and `approval` is `None`, returns
    ///   [`EnvError::ApprovalRequired`].
    /// - If approval is provided but any checklist item is unverified,
    ///   returns [`EnvError::InvalidConfig`].
    ///
    /// Additionally validates the embedded [`super::security::SecurityContext`]
    /// posture of any container referenced by this manifest is handled by
    /// the bridge; this function focuses on the approval workflow.
    pub fn validate_with_approval(
        &self,
        approval: Option<&DeploymentApproval>,
    ) -> Result<(), EnvError> {
        if !self.requires_approval() {
            return Ok(());
        }

        let approval = approval.ok_or(EnvError::ApprovalRequired)?;

        if let Some(unverified) = approval.checklist.iter().find(|item| !item.verified) {
            return Err(EnvError::InvalidConfig(format!(
                "approval checklist item not verified: {}",
                unverified.requirement
            )));
        }

        Ok(())
    }
}

#[async_trait]
impl Entity for DeploymentManifest {
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

/// Target environment for a deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeploymentTarget {
    /// Developer workstation / inner-loop.
    Dev,

    /// Shared sandbox environment. Still isolated from production.
    Sandbox,

    /// Production release target. May require approval.
    Release { approval_required: bool },
}

/// Coarse impact classification. Ordered so that higher impact is greater.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImpactLevel {
    /// Affects only developer environments.
    DevOnly,
    /// Affects only sandbox, which is isolated from production.
    SandboxIsolated,
    /// Touches production traffic or data.
    ProductionCritical,
}

/// Record of an approval for a deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeploymentApproval {
    /// Id of the [`DeploymentManifest`] being approved. Typed as
    /// [`EntityId`] (finding #264-4).
    pub deployment_id: EntityId,

    /// Approver identity.
    pub approver: String,

    /// When the approval was recorded.
    pub timestamp: DateTime<Utc>,

    /// Checklist of items the approver attested to.
    pub checklist: Vec<ApprovalItem>,
}

/// One item in an approval checklist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalItem {
    /// Human-readable requirement (e.g. `"runbook updated"`).
    pub requirement: String,

    /// Whether the requirement was verified.
    pub verified: bool,

    /// Optional evidence (URL, ticket id, etc.).
    pub evidence: Option<String>,
}

/// Errors raised by env-entity validation.
#[derive(Debug, Error)]
pub enum EnvError {
    /// Deployment requires approval but none was supplied.
    #[error("deployment approval required")]
    ApprovalRequired,

    /// Manifest or related record was rejected.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Referenced entity could not be found.
    #[error("not found: {0}")]
    NotFound(String),

    /// An env var was rejected because its name looks like a secret but its
    /// source is plaintext [`super::types::EnvVarSource::Literal`]. See
    /// finding #264-6.
    #[error("refusing to store secret-shaped env var `{var_name}` as a Literal; use EnvVarSource::SecretRef")]
    LikelySecretInLiteral {
        /// The environment variable name that was rejected.
        var_name: String,
    },

    /// Security posture for a container is too permissive for the caller's
    /// target. See finding #264-8.
    #[error("security context rejected: {0}")]
    InsecureSecurityContext(String),

    /// Error propagated from the generic entity layer.
    #[error("entity error: {0}")]
    Entity(#[from] EntityError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(target: DeploymentTarget) -> DeploymentManifest {
        DeploymentManifest::new(target, "svc".to_string(), 1, "container-id".to_string())
    }

    #[test]
    fn test_deployment_target_dev_no_approval() {
        let m = manifest(DeploymentTarget::Dev);
        assert!(!m.requires_approval());
    }

    #[test]
    fn test_deployment_target_sandbox_no_approval() {
        let m = manifest(DeploymentTarget::Sandbox);
        assert!(!m.requires_approval());
    }

    #[test]
    fn test_deployment_target_release_approval_required() {
        let m = manifest(DeploymentTarget::Release {
            approval_required: true,
        });
        assert!(m.requires_approval());

        // SECURITY (finding #264-1): Even with `approval_required: false`,
        // a Release target is ProductionCritical and therefore REQUIRES
        // approval — the flag cannot silently bypass the check.
        let m2 = manifest(DeploymentTarget::Release {
            approval_required: false,
        });
        assert!(
            m2.requires_approval(),
            "ProductionCritical must require approval regardless of approval_required"
        );
    }

    #[test]
    fn test_impact_level_is_intrinsic_to_target() {
        assert_eq!(
            manifest(DeploymentTarget::Dev).impact_level(),
            ImpactLevel::DevOnly
        );
        assert_eq!(
            manifest(DeploymentTarget::Sandbox).impact_level(),
            ImpactLevel::SandboxIsolated
        );
        assert_eq!(
            manifest(DeploymentTarget::Release {
                approval_required: false,
            })
            .impact_level(),
            ImpactLevel::ProductionCritical
        );
        assert_eq!(
            manifest(DeploymentTarget::Release {
                approval_required: true,
            })
            .impact_level(),
            ImpactLevel::ProductionCritical
        );
    }

    #[test]
    fn test_validate_production_critical_flag_bypass_rejected() {
        // SECURITY (finding #264-1): a `Release { approval_required: false }`
        // manifest MUST be rejected without approval.
        let m = manifest(DeploymentTarget::Release {
            approval_required: false,
        });
        match m.validate_with_approval(None) {
            Err(EnvError::ApprovalRequired) => {}
            other => panic!(
                "expected ApprovalRequired (ProductionCritical bypass rejection), got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_env_error_from_entity_error() {
        // finding #264-3: `From<EntityError> for EnvError` lets the env layer
        // lift generic entity failures transparently.
        let entity_err = EntityError::NotFound("entity-missing".to_string());
        let env_err: EnvError = entity_err.into();
        match env_err {
            EnvError::Entity(EntityError::NotFound(id)) => assert_eq!(id, "entity-missing"),
            other => panic!("expected Entity(NotFound), got {:?}", other),
        }
    }

    #[test]
    fn test_validate_dev_without_approval_ok() {
        let m = manifest(DeploymentTarget::Dev);
        assert!(m.validate_with_approval(None).is_ok());
    }

    #[test]
    fn test_validate_release_without_approval_errors() {
        let m = manifest(DeploymentTarget::Release {
            approval_required: true,
        });
        match m.validate_with_approval(None) {
            Err(EnvError::ApprovalRequired) => {}
            other => panic!("expected ApprovalRequired, got {:?}", other),
        }
    }

    #[test]
    fn test_validate_release_with_approval_ok() {
        let m = manifest(DeploymentTarget::Release {
            approval_required: true,
        });
        let approval = DeploymentApproval {
            deployment_id: m.metadata.id.clone(),
            approver: "alice".to_string(),
            timestamp: Utc::now(),
            checklist: vec![ApprovalItem {
                requirement: "runbook updated".to_string(),
                verified: true,
                evidence: Some("https://example.com/runbook".to_string()),
            }],
        };
        assert!(m.validate_with_approval(Some(&approval)).is_ok());

        // An unverified checklist item rejects the approval.
        let bad = DeploymentApproval {
            deployment_id: m.metadata.id.clone(),
            approver: "bob".to_string(),
            timestamp: Utc::now(),
            checklist: vec![ApprovalItem {
                requirement: "smoke tests".to_string(),
                verified: false,
                evidence: None,
            }],
        };
        match m.validate_with_approval(Some(&bad)) {
            Err(EnvError::InvalidConfig(msg)) => assert!(msg.contains("smoke tests")),
            other => panic!("expected InvalidConfig, got {:?}", other),
        }
    }

    #[test]
    fn test_approval_item_verified_evidence_populated() {
        let item = ApprovalItem {
            requirement: "change ticket".to_string(),
            verified: true,
            evidence: Some("CHG-42".to_string()),
        };
        assert!(item.verified);
        assert_eq!(item.evidence.as_deref(), Some("CHG-42"));

        let json = serde_json::to_string(&item).expect("serialize");
        let decoded: ApprovalItem = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.requirement, item.requirement);
        assert_eq!(decoded.verified, item.verified);
        assert_eq!(decoded.evidence, item.evidence);
    }

    #[test]
    fn test_impact_level_ordering() {
        assert!(ImpactLevel::DevOnly < ImpactLevel::SandboxIsolated);
        assert!(ImpactLevel::SandboxIsolated < ImpactLevel::ProductionCritical);
        assert!(ImpactLevel::DevOnly < ImpactLevel::ProductionCritical);

        let mut levels = vec![
            ImpactLevel::ProductionCritical,
            ImpactLevel::DevOnly,
            ImpactLevel::SandboxIsolated,
        ];
        levels.sort();
        assert_eq!(
            levels,
            vec![
                ImpactLevel::DevOnly,
                ImpactLevel::SandboxIsolated,
                ImpactLevel::ProductionCritical,
            ]
        );
    }

    #[tokio::test]
    async fn test_deployment_manifest_implements_entity() {
        let m = manifest(DeploymentTarget::Sandbox);
        assert_eq!(m.metadata().entity_type, EntityType::Env);
        assert_eq!(m.entity_type(), EntityType::Env);
        assert!(m.to_json().is_ok());

        let boxed: Box<dyn Entity> = Box::new(m);
        assert_eq!(boxed.entity_type(), EntityType::Env);
    }

    #[test]
    fn test_env_error_display() {
        let approval = EnvError::ApprovalRequired;
        assert_eq!(approval.to_string(), "deployment approval required");

        let invalid = EnvError::InvalidConfig("bad shape".to_string());
        assert_eq!(invalid.to_string(), "invalid configuration: bad shape");

        let nf = EnvError::NotFound("entity-123".to_string());
        assert_eq!(nf.to_string(), "not found: entity-123");
    }
}
