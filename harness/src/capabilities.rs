use std::path::Path;

/// A cargo plugin capability: detectable from the workspace and provisionable
/// via the Nix flake.
pub struct CargoCapability {
    /// Unique identifier used as the agent tool name, e.g. `"cargo_deny"`.
    pub id: &'static str,
    /// The `cargo` subcommand, e.g. `"deny"`.
    pub subcommand: &'static str,
    /// Nix package to add to `devContainerPackages`, or `None` for built-in
    /// cargo subcommands that need no extra package.
    pub nix_package: Option<&'static str>,
    /// Top-level file names whose presence signals this capability is in use.
    pub signal_files: &'static [&'static str],
    /// Top-level directory names whose presence signals this capability.
    pub signal_dirs: &'static [&'static str],
    /// Human-readable description shown to the agent.
    pub description: &'static str,
}

/// Static catalog of known cargo capabilities.
///
/// Each entry maps a detection signal (file or directory at the workspace
/// root) to the Nix package that provides the binary and the tool name the
/// agent will see.
///
/// Entries with `nix_package: None` are built-in cargo subcommands; they do
/// not need flake provisioning but may still be conditionally registered based
/// on project signals.
pub static CARGO_CAPABILITIES: &[CargoCapability] = &[
    CargoCapability {
        id: "cargo_deny",
        subcommand: "deny",
        nix_package: Some("pkgs.cargo-deny"),
        signal_files: &["deny.toml"],
        signal_dirs: &[],
        description: "Check dependencies for license violations and security advisories. \
                      Reads deny.toml for policy. Example: call with {} to run all checks, \
                      or {\"check\": \"advisories\"} to run one category. \
                      Returns { stdout, stderr, success, command }.",
    },
    CargoCapability {
        id: "cargo_audit",
        subcommand: "audit",
        nix_package: Some("pkgs.cargo-audit"),
        signal_files: &["audit.toml"],
        signal_dirs: &[],
        description: "Audit Cargo.lock for known security vulnerabilities. \
                      Example: call with {} to audit all dependencies. \
                      Returns { stdout, stderr, success, command }.",
    },
];

/// Returns the capabilities whose detection signals are present under
/// `workspace_root`.
///
/// Detection is purely based on filesystem presence — no network or container
/// access is required.
pub fn detect_capabilities(workspace_root: &Path) -> Vec<&'static CargoCapability> {
    CARGO_CAPABILITIES
        .iter()
        .filter(|cap| {
            cap.signal_files
                .iter()
                .any(|f| workspace_root.join(f).exists())
                || cap
                    .signal_dirs
                    .iter()
                    .any(|d| workspace_root.join(d).is_dir())
        })
        .collect()
}

/// Returns capabilities whose signals are present in a pre-collected list of
/// top-level directory entries. Use this when you already have the listing to
/// avoid a second `read_dir` call.
pub fn detect_capabilities_from_entries(entries: &[String]) -> Vec<&'static CargoCapability> {
    CARGO_CAPABILITIES
        .iter()
        .filter(|cap| {
            cap.signal_files
                .iter()
                .any(|f| entries.iter().any(|e| e == f))
                || cap
                    .signal_dirs
                    .iter()
                    .any(|d| entries.iter().any(|e| e == d))
        })
        .collect()
}

/// Returns the capability with the given `id`, or `None`.
pub fn find_capability(id: &str) -> Option<&'static CargoCapability> {
    CARGO_CAPABILITIES.iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmpdir_with(files: &[&str], dirs: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for f in files {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        for d in dirs {
            std::fs::create_dir(dir.path().join(d)).unwrap();
        }
        dir
    }

    #[test]
    fn catalog_entries_are_consistent() {
        for cap in CARGO_CAPABILITIES {
            assert!(!cap.id.is_empty(), "capability id must be non-empty");
            assert!(!cap.subcommand.is_empty(), "subcommand must be non-empty");
            assert!(!cap.description.is_empty(), "description must be non-empty");
            if let Some(pkg) = cap.nix_package {
                assert!(!pkg.is_empty(), "nix_package must be non-empty when Some");
            }
        }
    }

    #[test]
    fn catalog_ids_are_unique() {
        let ids: Vec<&str> = CARGO_CAPABILITIES.iter().map(|c| c.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "capability ids must be unique");
    }

    #[test]
    fn cargo_deny_detected_from_deny_toml() {
        let dir = tmpdir_with(&["deny.toml"], &[]);
        let caps = detect_capabilities(dir.path());
        assert!(caps.iter().any(|c| c.id == "cargo_deny"));
    }

    #[test]
    fn cargo_deny_absent_without_deny_toml() {
        let dir = tmpdir_with(&[], &[]);
        let caps = detect_capabilities(dir.path());
        assert!(!caps.iter().any(|c| c.id == "cargo_deny"));
    }

    #[test]
    fn cargo_audit_detected_from_audit_toml() {
        let dir = tmpdir_with(&["audit.toml"], &[]);
        let caps = detect_capabilities(dir.path());
        assert!(caps.iter().any(|c| c.id == "cargo_audit"));
    }

    #[test]
    fn cargo_audit_absent_without_audit_toml() {
        let dir = tmpdir_with(&[], &[]);
        let caps = detect_capabilities(dir.path());
        assert!(!caps.iter().any(|c| c.id == "cargo_audit"));
    }

    #[test]
    fn detect_from_entries_matches_filesystem_detection() {
        let dir = tmpdir_with(&["deny.toml"], &[]);
        let entries = vec!["deny.toml".to_string(), "Cargo.toml".to_string()];
        let from_fs = detect_capabilities(dir.path());
        let from_entries = detect_capabilities_from_entries(&entries);
        let fs_ids: Vec<&str> = from_fs.iter().map(|c| c.id).collect();
        let entry_ids: Vec<&str> = from_entries.iter().map(|c| c.id).collect();
        assert_eq!(fs_ids, entry_ids);
    }

    #[test]
    fn find_capability_returns_correct_entry() {
        let cap = find_capability("cargo_deny").unwrap();
        assert_eq!(cap.subcommand, "deny");
        assert_eq!(cap.nix_package, Some("pkgs.cargo-deny"));
    }

    #[test]
    fn find_capability_returns_none_for_unknown() {
        assert!(find_capability("cargo_frobnicate").is_none());
    }

    #[test]
    fn cargo_deny_nix_package_is_set() {
        let cap = find_capability("cargo_deny").unwrap();
        assert_eq!(cap.nix_package, Some("pkgs.cargo-deny"));
    }

    #[test]
    fn deny_description_contains_example() {
        let cap = find_capability("cargo_deny").unwrap();
        assert!(
            cap.description.contains("Example"),
            "description should contain a usage example"
        );
    }

    #[test]
    fn empty_workspace_has_no_capabilities() {
        let dir = TempDir::new().unwrap();
        assert!(detect_capabilities(dir.path()).is_empty());
    }

    #[test]
    fn all_capabilities_have_descriptions_with_returns_clause() {
        for cap in CARGO_CAPABILITIES {
            assert!(
                cap.description.contains("Returns"),
                "capability '{}' description should describe return value",
                cap.id
            );
        }
    }
}
