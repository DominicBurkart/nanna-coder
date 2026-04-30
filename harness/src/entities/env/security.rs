//! Security context for container configuration entities
//!
//! Defines the restrictive-by-default security posture applied to
//! [`crate::entities::env::types::ContainerConfigEntity`] values.
//!
//! The default is intentionally locked down: read-only root filesystem,
//! non-root UID, no extra capabilities, no additional allowed paths.
//! Callers MUST explicitly opt in to relax this posture.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Security posture for a container.
///
/// [`Default`] returns a restrictive configuration:
/// - `read_only_root_fs: true`
/// - `run_as_non_root: true`
/// - `capabilities: CapabilitySet::None`
/// - `allowed_paths: vec![]`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityContext {
    /// Mount the container root filesystem read-only.
    pub read_only_root_fs: bool,

    /// Require a non-root UID inside the container.
    pub run_as_non_root: bool,

    /// Linux capabilities granted to the container. Defaults to
    /// [`CapabilitySet::None`].
    pub capabilities: CapabilitySet,

    /// Paths the container is permitted to access beyond the default
    /// read-only root filesystem. Empty by default.
    pub allowed_paths: Vec<PathBuf>,
}

impl Default for SecurityContext {
    fn default() -> Self {
        Self {
            read_only_root_fs: true,
            run_as_non_root: true,
            capabilities: CapabilitySet::None,
            allowed_paths: Vec::new(),
        }
    }
}

/// Set of Linux capabilities granted to a container.
///
/// `None` grants no capabilities. `Minimal` documents a curated minimal
/// set (e.g. `NetBindService` for privileged ports). `Custom` carries any
/// caller-specified list — callers are responsible for justifying the use
/// in review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "caps", rename_all = "snake_case")]
pub enum CapabilitySet {
    /// No capabilities granted.
    None,

    /// Minimal, vetted capability set.
    Minimal(Vec<Capability>),

    /// Caller-specified capability set.
    Custom(Vec<Capability>),
}

/// Closed enum of Linux capabilities recognized by the entity layer.
///
/// This intentionally does NOT include a `Custom(String)` variant —
/// capabilities outside this list require a code change and review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// `CAP_NET_BIND_SERVICE` — bind to privileged ports (<1024).
    NetBindService,
    /// `CAP_CHOWN` — change file ownership.
    Chown,
    /// `CAP_DAC_OVERRIDE` — bypass file permission checks.
    DacOverride,
    /// `CAP_SYS_ADMIN` — wide-ranging admin operations (avoid).
    SysAdmin,
    /// `CAP_NET_RAW` — use raw sockets.
    NetRaw,
    /// `CAP_SETUID` — set process UID.
    SetUid,
    /// `CAP_SETGID` — set process GID.
    SetGid,
    /// `CAP_SYS_TIME` — set system clock.
    SysTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_security_context_default_is_restrictive() {
        let sc = SecurityContext::default();
        assert!(sc.read_only_root_fs, "read_only_root_fs must default true");
        assert!(sc.run_as_non_root, "run_as_non_root must default true");
        assert_eq!(
            sc.capabilities,
            CapabilitySet::None,
            "capabilities must default to None"
        );
        assert!(
            sc.allowed_paths.is_empty(),
            "allowed_paths must default empty"
        );
    }

    #[test]
    fn test_capability_set_none() {
        let cs = CapabilitySet::None;
        let json = serde_json::to_string(&cs).expect("serialize");
        let decoded: CapabilitySet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, cs);
    }

    #[test]
    fn test_capability_set_minimal() {
        let cs = CapabilitySet::Minimal(vec![Capability::NetBindService, Capability::Chown]);
        let json = serde_json::to_string(&cs).expect("serialize");
        let decoded: CapabilitySet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, cs);
    }

    #[test]
    fn test_capability_set_custom() {
        let cs = CapabilitySet::Custom(vec![
            Capability::SysAdmin,
            Capability::NetRaw,
            Capability::SetUid,
            Capability::SetGid,
            Capability::DacOverride,
            Capability::SysTime,
        ]);
        let json = serde_json::to_string(&cs).expect("serialize");
        let decoded: CapabilitySet = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, cs);
    }
}
