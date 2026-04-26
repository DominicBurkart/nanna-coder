//! Environment & Deployment Entities
//!
//! Container configuration and deployment entities for managing the
//! development, sandbox, and release environments described in
//! ARCHITECTURE.md ("Container Topology"). The submodules cover:
//!
//! - [`types`] — `EnvEntity` and `DeploymentTier` schema
//! - [`security`] — security-context primitives applied to a deployment
//! - [`deployment`] — concrete deployment configuration helpers
//! - [`bridge`] — bridge between an `EnvEntity` and the runtime container
//!   handle (see `crate::container`)
//!
//! Tracked in issue #25.

pub mod bridge;
pub mod deployment;
pub mod security;
pub mod types;

pub use bridge::*;
pub use deployment::*;
pub use security::*;
pub use types::*;
