//! Environment & Deployment Entities
//!
//! This module implements container configuration and deployment entities for
//! managing the development, sandbox, and release environments.
//!
//! See issue #25 and ARCHITECTURE.md for details.

pub mod bridge;
pub mod deployment;
pub mod security;
pub mod types;

// Named re-exports (followup from PR #260 review / issue #264).
// Prefer explicit names over glob re-exports so the public surface stays
// tractable and new additions are opt-in.
pub use bridge::{from_container_config, from_container_config_with_classifier, SecretClassifier};
pub use deployment::{
    ApprovalItem, DeploymentApproval, DeploymentManifest, DeploymentTarget, EnvError, ImpactLevel,
};
pub use security::{Capability, CapabilitySet, SecurityContext};
pub use types::{
    ContainerConfigEntity, EnvEntity, EnvVarRef, EnvVarSource, PortMapping, RuntimeConfig,
    VolumeMount,
};
