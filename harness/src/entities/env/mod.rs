//! Environment & Deployment Entities
//!
//! Container configuration and deployment entities for managing the
//! development, sandbox, and release environments described in
//! ARCHITECTURE.md ("Container Topology"). The submodules cover:
//!
//! - [`types`] — `EnvEntity`, `ContainerConfigEntity`, `RuntimeConfig`,
//!   `PortMapping`, `VolumeMount`, `EnvVarRef`, and `EnvVarSource` schema
//! - [`security`] — security-context primitives applied to a deployment
//! - [`deployment`] — concrete deployment configuration helpers
//! - [`bridge`] — converts a `container::ContainerConfig` into a
//!   `ContainerConfigEntity` (see `crate::container`)
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
