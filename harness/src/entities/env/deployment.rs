//! Deployment manifest entity and approval workflow
//!
//! Defines [`DeploymentManifest`] together with its target, impact, and
//! approval types. Release deployments require an explicit approval record;
//! dev and sandbox deployments do not.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A deployment of a [`super::types::ContainerConfigEntity`] to a target
/// environment.
///
/// The `container` field holds the id of the `ContainerConfigEntity` being
/// deployed (string id, matching [`crate::entities::EntityId`]).
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

    /// Id of the `ContainerConfigEntity` being deployed.
    pub container: String,
}

impl DeploymentManifest {
    /// Create a new deployment manifest.
    pub fn new(
        target: DeploymentTarget,
        service_name: String,
        replicas: u32,
        container: String,
    ) -> Self {
        Self {
            metadata: EntityMetadata::new(EntityType::Env),
            target,
            service_name,
            replicas,
            container,
        }
    }

    /// Whether this deployment requires an approval record before it can
    /// proceed.
    pub fn requires_approval(&self) -> bool {
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
    /// Id of the [`DeploymentManifest`] being approved.
    pub deployment_id: String,

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

        let m2 = manifest(DeploymentTarget::Release {
            approval_required: false,
        });
        assert!(!m2.requires_approval());
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
