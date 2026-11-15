use crate::onboarding::fullstack::workspace_member_dirs;
use std::path::{Path, PathBuf};

/// Where a capability's signal files and directories are searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalScope {
    /// Only the workspace root.
    Root,
    /// The workspace root and every workspace member directory.
    RootAndMembers,
}

/// A cargo plugin capability: detectable from the workspace and provisionable
/// via the Nix flake.
pub struct CargoCapability {
    /// Unique identifier used as the agent tool name, e.g. `"cargo_deny"`.
    pub id: &'static str,
    /// The program that provides the capability, e.g. `"cargo"` or `"trunk"`.
    pub program: &'static str,
    /// The subcommand of `program`, e.g. `"deny"`.
    pub subcommand: &'static str,
    /// Nix package to add to `devContainerPackages`, or `None` for built-in
    /// cargo subcommands that need no extra package.
    pub nix_package: Option<&'static str>,
    /// Top-level file names whose presence signals this capability is in use.
    pub signal_files: &'static [&'static str],
    /// Top-level directory names whose presence signals this capability.
    pub signal_dirs: &'static [&'static str],
    /// Directories in which the signals are searched.
    pub scope: SignalScope,
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
        program: "cargo",
        subcommand: "deny",
        nix_package: Some("pkgs.cargo-deny"),
        signal_files: &["deny.toml"],
        signal_dirs: &[],
        scope: SignalScope::Root,
        description: "Check dependencies for license violations and security advisories. \
                      Reads deny.toml for policy. Example: call with {} to run all checks, \
                      or {\"check\": \"advisories\"} to run one category. \
                      Returns { stdout, stderr, success, command }.",
    },
    CargoCapability {
        id: "cargo_audit",
        program: "cargo",
        subcommand: "audit",
        nix_package: Some("pkgs.cargo-audit"),
        signal_files: &["audit.toml"],
        signal_dirs: &[],
        scope: SignalScope::Root,
        description: "Audit Cargo.lock for known security vulnerabilities. \
                      Example: call with {} to audit all dependencies. \
                      Returns { stdout, stderr, success, command }.",
    },
    CargoCapability {
        id: "trunk_build",
        program: "trunk",
        subcommand: "build",
        nix_package: Some("pkgs.trunk"),
        signal_files: &["Trunk.toml"],
        signal_dirs: &[],
        scope: SignalScope::RootAndMembers,
        description: "Build the web frontend with trunk: compiles the crate that owns \
                      Trunk.toml to wasm32-unknown-unknown and writes index.html, the \
                      JS glue and the .wasm bundle to its dist directory, which the \
                      backend serves. Example: call with {} for a debug build, or \
                      {\"release\": \"true\"} for an optimised one. \
                      Returns { stdout, stderr, success, command, working_dir }.",
    },
    CargoCapability {
        id: "sqlx_migrate",
        program: "sqlx",
        subcommand: "migrate",
        nix_package: Some("pkgs.sqlx-cli"),
        signal_files: &[],
        signal_dirs: &["migrations"],
        scope: SignalScope::RootAndMembers,
        description: "Apply the sqlx migrations in the migrations directory to the \
                      database in DATABASE_URL. Example: call with {} to run pending \
                      migrations, {\"command\": \"info\"} to list them, or \
                      {\"command\": \"revert\"} to undo the last one. \
                      Returns { stdout, stderr, success, command, working_dir }.",
    },
];

/// Returns the capabilities whose detection signals are present under
/// `workspace_root`.
///
/// Detection is purely based on filesystem presence — no network or container
/// access is required.
pub fn detect_capabilities(workspace_root: &Path) -> Vec<&'static CargoCapability> {
    detect_capability_locations(workspace_root)
        .into_iter()
        .map(|(cap, _)| cap)
        .collect()
}

/// Returns every detected capability together with the directory, relative
/// to `workspace_root`, in which its signal was found (empty for the root).
///
/// Capabilities with [`SignalScope::RootAndMembers`] are searched at the root
/// first and then in each workspace member in workspace order; the first hit
/// wins.
pub fn detect_capability_locations(
    workspace_root: &Path,
) -> Vec<(&'static CargoCapability, PathBuf)> {
    let members = workspace_member_dirs(workspace_root);
    CARGO_CAPABILITIES
        .iter()
        .filter_map(|cap| signal_location(workspace_root, &members, cap).map(|rel| (cap, rel)))
        .collect()
}

fn signal_location(root: &Path, members: &[PathBuf], cap: &CargoCapability) -> Option<PathBuf> {
    let member_dirs: &[PathBuf] = match cap.scope {
        SignalScope::Root => &[],
        SignalScope::RootAndMembers => members,
    };
    std::iter::once(root)
        .chain(member_dirs.iter().map(PathBuf::as_path))
        .find(|dir| signal_present_in(dir, cap))
        .map(|dir| dir.strip_prefix(root).unwrap_or(dir).to_path_buf())
}

fn signal_present_in(dir: &Path, cap: &CargoCapability) -> bool {
    cap.signal_files.iter().any(|f| dir.join(f).exists())
        || cap.signal_dirs.iter().any(|d| dir.join(d).is_dir())
}

/// Returns capabilities whose signals are present in a pre-collected list of
/// top-level directory entries. Use this when you already have the listing to
/// avoid a second `read_dir` call. Only root-level signals can be seen this
/// way; member-scoped signals need [`detect_capabilities`].
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
            assert!(!cap.program.is_empty(), "program must be non-empty");
            assert!(!cap.subcommand.is_empty(), "subcommand must be non-empty");
            assert!(
                !cap.signal_files.is_empty() || !cap.signal_dirs.is_empty(),
                "capability '{}' needs at least one signal",
                cap.id
            );
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

    fn workspace_with_member(member: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            format!("[workspace]\nmembers = [\"{member}\"]\n"),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join(member)).unwrap();
        dir
    }

    #[test]
    fn trunk_build_detected_from_trunk_toml_in_member() {
        let dir = workspace_with_member("ui");
        std::fs::write(dir.path().join("ui/Trunk.toml"), "").unwrap();
        let locations = detect_capability_locations(dir.path());
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].0.id, "trunk_build");
        assert_eq!(locations[0].1, PathBuf::from("ui"));
        assert!(detect_capabilities(dir.path())
            .iter()
            .any(|c| c.id == "trunk_build"));
    }

    #[test]
    fn trunk_build_detected_from_trunk_toml_at_root() {
        let dir = tmpdir_with(&["Trunk.toml"], &[]);
        let locations = detect_capability_locations(dir.path());
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].0.id, "trunk_build");
        assert_eq!(locations[0].1, PathBuf::new());
    }

    #[test]
    fn trunk_build_absent_without_trunk_toml() {
        let dir = workspace_with_member("ui");
        std::fs::write(dir.path().join("ui/index.html"), "").unwrap();
        assert!(!detect_capabilities(dir.path())
            .iter()
            .any(|c| c.id == "trunk_build"));
    }

    #[test]
    fn sqlx_migrate_detected_from_migrations_dir_at_root_or_in_member() {
        let root = tmpdir_with(&[], &["migrations"]);
        let at_root = detect_capability_locations(root.path());
        assert_eq!(at_root[0].0.id, "sqlx_migrate");
        assert_eq!(at_root[0].1, PathBuf::new());

        let member = workspace_with_member("api");
        std::fs::create_dir(member.path().join("api/migrations")).unwrap();
        let in_member = detect_capability_locations(member.path());
        assert_eq!(in_member[0].0.id, "sqlx_migrate");
        assert_eq!(in_member[0].1, PathBuf::from("api"));
    }

    #[test]
    fn sqlx_migrate_absent_when_migrations_is_a_file() {
        let dir = tmpdir_with(&["migrations"], &[]);
        assert!(detect_capabilities(dir.path()).is_empty());
    }

    #[test]
    fn root_scoped_signals_ignore_members() {
        let dir = workspace_with_member("api");
        std::fs::write(dir.path().join("api/deny.toml"), "").unwrap();
        assert!(!detect_capabilities(dir.path())
            .iter()
            .any(|c| c.id == "cargo_deny"));
    }

    #[test]
    fn member_scoped_signals_are_invisible_to_entry_detection() {
        let entries = vec!["Cargo.toml".to_string(), "ui".to_string()];
        assert!(detect_capabilities_from_entries(&entries).is_empty());
        let root_entries = vec!["Trunk.toml".to_string(), "migrations".to_string()];
        let ids: Vec<&str> = detect_capabilities_from_entries(&root_entries)
            .iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["trunk_build", "sqlx_migrate"]);
    }

    #[test]
    fn fixture_workspace_exposes_trunk_build_and_sqlx_migrate() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/fullstack");
        let locations = detect_capability_locations(&fixture);
        let found: Vec<(&str, &Path)> =
            locations.iter().map(|(c, p)| (c.id, p.as_path())).collect();
        assert_eq!(
            found,
            vec![
                ("trunk_build", Path::new("ui")),
                ("sqlx_migrate", Path::new("")),
            ]
        );
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
