//! The paths no agent may modify: Nanna's own configuration.
//!
//! [`ProtectedPaths`] is a fixed set of repository-relative globs (agent
//! identities under `.nanna/`, deployment and availability templates, the
//! CI workflows and `CODEOWNERS` that guard them, `codecov.yml`, and the
//! repository's own `.git` directory) plus Nanna's own configuration
//! directory when it happens to lie inside the repository. It sits above
//! [`crate::scope::PathScope`]: no identity can widen it, and every
//! write-capable tool, the dev container mounts and the patch extraction in
//! [`crate::workspace::TaskWorkspace`] consult it.
//!
//! `.git/**` matters even though it is a `Workspace`-class write (no action
//! auditor reviews it): the PR/issue tools in [`crate::pr_tools`] are
//! `Repository`-class and audited, but they resolve which repository to act
//! on by reading the worktree's live `origin` remote. Without this entry, an
//! unaudited `write_file` to `.git/config` could redirect that remote before
//! an audited tool call ever runs, silently aiming Nanna's GitHub credentials
//! at a different repository than the one the auditor approved.

use crate::identity::IdentityCatalog;
use glob::{MatchOptions, Pattern};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The fixed, repository-relative globs every agent is refused, matched
/// with `*` never crossing `/` and `**` spanning directories. A glob
/// ending in `/**` also matches the directory itself.
pub const PROTECTED_PATTERNS: &[&str] = &[
    ".nanna/**",
    "**/.nanna/**",
    ".git/**",
    ".github/workflows/**",
    ".github/CODEOWNERS",
    "codecov.yml",
    "windows.toml",
];

const MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

const RECURSIVE_SUFFIX: &str = "/**";

/// A write that reached a protected path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Error)]
#[error("`{path}` is protected by rule `{rule}`: Nanna may not modify its own configuration")]
pub struct ProtectedPathViolation {
    /// The repository-relative path that was refused.
    pub path: String,
    /// The glob from [`PROTECTED_PATTERNS`] (or the configuration
    /// directory rule) that matched.
    pub rule: String,
}

/// Receives every protected-path violation so an auditor can escalate it.
///
/// The default implementation does nothing; [`NoopAuditHook`] is the
/// implementor a workspace starts with.
pub trait AuditHook: Send + Sync {
    /// Called when `task_id` produced changes touching a protected path.
    fn on_protected_path_violation(&self, _task_id: &str, _violation: &ProtectedPathViolation) {}
}

/// An [`AuditHook`] that records nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopAuditHook;

impl AuditHook for NoopAuditHook {}

#[derive(Debug, Clone)]
struct Rule {
    glob: String,
    pattern: Pattern,
    directory: Option<Pattern>,
}

fn compile(glob: &str) -> Pattern {
    Pattern::new(glob).expect("protected glob is valid")
}

impl Rule {
    fn new(glob: String) -> Self {
        let pattern = compile(&glob);
        let directory = glob.strip_suffix(RECURSIVE_SUFFIX).map(compile);
        Self {
            glob,
            pattern,
            directory,
        }
    }

    fn matches(&self, relative: &Path) -> bool {
        let matches = |pattern: &Pattern| pattern.matches_path_with(relative, MATCH_OPTIONS);
        matches(&self.pattern) || self.directory.as_ref().is_some_and(matches)
    }

    fn mount_root(&self) -> Option<PathBuf> {
        if self.glob.starts_with("**") {
            return None;
        }
        let root = self
            .glob
            .strip_suffix(RECURSIVE_SUFFIX)
            .unwrap_or(&self.glob);
        Some(PathBuf::from(root))
    }
}

/// The set of paths no agent may write, regardless of identity.
///
/// ```
/// use harness::protected::ProtectedPaths;
/// use std::path::Path;
///
/// let protected = ProtectedPaths::for_repo(Path::new("."));
/// assert!(protected.is_protected(Path::new(".nanna/agents/rust-implementer.toml")));
/// assert!(protected.is_protected(Path::new(".github/workflows/ci.yml")));
/// assert!(protected.is_protected(Path::new("codecov.yml")));
/// assert!(!protected.is_protected(Path::new("src/main.rs")));
///
/// let violation = protected.check(Path::new(".nanna/deploy.toml")).unwrap_err();
/// assert_eq!(violation.rule, ".nanna/**");
/// ```
#[derive(Debug, Clone)]
pub struct ProtectedPaths {
    rules: Vec<Rule>,
    config_dir: Option<PathBuf>,
}

fn relative_config_dir(repo_root: &Path, config_dir: &Path) -> Option<PathBuf> {
    let canonical = repo_root.canonicalize().ok();
    std::iter::once(repo_root)
        .chain(canonical.as_deref())
        .find_map(|root| config_dir.strip_prefix(root).ok())
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

impl ProtectedPaths {
    /// The protected set for the repository at `repo_root`, with Nanna's
    /// configuration directory taken from
    /// [`IdentityCatalog::default_global_dir`].
    pub fn for_repo(repo_root: &Path) -> Self {
        let agents_dir = IdentityCatalog::default_global_dir();
        let config_dir = agents_dir.as_deref().and_then(Path::parent);
        Self::with_config_dir(repo_root, config_dir)
    }

    /// The protected set for `repo_root` with an explicit configuration
    /// directory (`None` when Nanna has no configuration directory).
    pub fn with_config_dir(repo_root: &Path, config_dir: Option<&Path>) -> Self {
        let mut rules: Vec<Rule> = PROTECTED_PATTERNS
            .iter()
            .map(|glob| Rule::new(glob.to_string()))
            .collect();
        if let Some(relative) = config_dir.and_then(|dir| relative_config_dir(repo_root, dir)) {
            let escaped = Pattern::escape(&relative.to_string_lossy());
            rules.push(Rule::new(format!("{escaped}{RECURSIVE_SUFFIX}")));
        }
        Self {
            rules,
            config_dir: config_dir.map(Path::to_path_buf),
        }
    }

    /// Whether the repository-relative `path` may not be written.
    pub fn is_protected(&self, path: &Path) -> bool {
        self.check(path).is_err()
    }

    /// The rule protecting the repository-relative `path`, as an error.
    pub fn check(&self, path: &Path) -> Result<(), ProtectedPathViolation> {
        match self.rules.iter().find(|rule| rule.matches(path)) {
            Some(rule) => Err(ProtectedPathViolation {
                path: path.to_string_lossy().into_owned(),
                rule: rule.glob.clone(),
            }),
            None => Ok(()),
        }
    }

    /// The globs in force, in matching order.
    pub fn rules(&self) -> Vec<&str> {
        self.rules.iter().map(|rule| rule.glob.as_str()).collect()
    }

    /// Nanna's own configuration directory, which is never mounted into a
    /// dev container.
    pub fn config_dir(&self) -> Option<&Path> {
        self.config_dir.as_deref()
    }

    /// The repository-relative directories and files that, mounted
    /// read-only, cover every rule with a literal prefix. Nested roots are
    /// folded into their ancestor.
    pub fn mount_roots(&self) -> Vec<PathBuf> {
        let candidates: Vec<PathBuf> = self.rules.iter().filter_map(Rule::mount_root).collect();
        let mut roots: Vec<PathBuf> = Vec::new();
        for candidate in candidates {
            let covered = roots.iter().any(|root| candidate.starts_with(root));
            if !covered && !roots.contains(&candidate) {
                roots.push(candidate);
            }
        }
        roots
    }

    /// The [`ProtectedPaths::mount_roots`] that exist under `repo_root`.
    pub fn existing_roots(&self, repo_root: &Path) -> Vec<PathBuf> {
        self.mount_roots()
            .into_iter()
            .filter(|root| repo_root.join(root).exists())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn protected() -> ProtectedPaths {
        ProtectedPaths::with_config_dir(Path::new("/repo"), None)
    }

    #[test]
    fn every_fixed_pattern_protects_its_example_path() {
        let table = [
            (".nanna/**", ".nanna/agents/rust-implementer.toml"),
            (".nanna/**", ".nanna/deploy.toml"),
            (".nanna/**", ".nanna/windows.toml"),
            (".nanna/**", ".nanna"),
            ("**/.nanna/**", "crates/api/.nanna/agents/x.toml"),
            ("**/.nanna/**", "crates/api/.nanna"),
            (".git/**", ".git/config"),
            (".git/**", ".git/hooks/pre-commit"),
            (".github/workflows/**", ".github/workflows/ci.yml"),
            (".github/workflows/**", ".github/workflows"),
            (".github/CODEOWNERS", ".github/CODEOWNERS"),
            ("codecov.yml", "codecov.yml"),
            ("windows.toml", "windows.toml"),
        ];
        let protected = protected();
        for (rule, path) in table {
            let violation = protected.check(Path::new(path)).unwrap_err();
            assert_eq!(violation.rule, rule, "{path}");
            assert_eq!(violation.path, path);
            assert!(protected.is_protected(Path::new(path)), "{path}");
        }
        for pattern in PROTECTED_PATTERNS {
            assert!(
                table.iter().any(|(rule, _)| rule == pattern),
                "{pattern} has no example in the table"
            );
        }
    }

    #[test]
    fn ordinary_repository_paths_are_not_protected() {
        let protected = protected();
        for path in [
            "src/main.rs",
            "README.md",
            ".github/dependabot.yml",
            ".github/ISSUE_TEMPLATE/bug.md",
            "docs/ci/protected-paths-guard.yml",
            "nanna/agents/x.toml",
            "codecov.yml.bak",
            "docs/windows.toml",
            ".nannax/agents/x.toml",
        ] {
            assert!(!protected.is_protected(Path::new(path)), "{path}");
            assert_eq!(protected.check(Path::new(path)), Ok(()));
        }
    }

    #[test]
    fn config_dir_inside_the_repo_is_protected_and_reported_as_a_rule() {
        let root = Path::new("/repo");
        let config = Path::new("/repo/home/.config/nanna");
        let protected = ProtectedPaths::with_config_dir(root, Some(config));
        let violation = protected
            .check(Path::new("home/.config/nanna/agents/x.toml"))
            .unwrap_err();
        assert_eq!(violation.rule, "home/.config/nanna/**");
        assert!(protected.is_protected(Path::new("home/.config/nanna")));
        assert!(!protected.is_protected(Path::new("home/.config/other")));
        assert_eq!(protected.config_dir(), Some(config));
    }

    #[test]
    fn config_dir_outside_the_repo_adds_no_rule() {
        let protected = ProtectedPaths::with_config_dir(
            Path::new("/repo"),
            Some(Path::new("/home/user/.config/nanna")),
        );
        assert_eq!(protected.rules().len(), PROTECTED_PATTERNS.len());
        assert_eq!(
            protected.config_dir(),
            Some(Path::new("/home/user/.config/nanna"))
        );
    }

    #[test]
    fn config_dir_with_glob_metacharacters_is_matched_literally() {
        let root = Path::new("/repo");
        let config = Path::new("/repo/[cfg]/nanna");
        let protected = ProtectedPaths::with_config_dir(root, Some(config));
        assert!(protected.is_protected(Path::new("[cfg]/nanna/agents/x.toml")));
        assert!(!protected.is_protected(Path::new("c/nanna/agents/x.toml")));
    }

    #[test]
    fn config_dir_under_a_symlinked_repo_root_is_resolved() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(real.join("cfg")).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let config = real.canonicalize().unwrap().join("cfg");
        let protected = ProtectedPaths::with_config_dir(&link, Some(&config));
        assert!(protected.is_protected(Path::new("cfg/agents/x.toml")));
    }

    #[test]
    fn for_repo_uses_the_catalog_config_dir() {
        let protected = ProtectedPaths::for_repo(Path::new("/repo"));
        let expected =
            IdentityCatalog::default_global_dir().and_then(|d| d.parent().map(Path::to_path_buf));
        assert_eq!(protected.config_dir(), expected.as_deref());
        assert!(protected.is_protected(Path::new(".nanna/agents/x.toml")));
    }

    #[test]
    fn mount_roots_are_the_literal_prefixes_without_nested_duplicates() {
        let protected =
            ProtectedPaths::with_config_dir(Path::new("/repo"), Some(Path::new("/repo/cfg")));
        assert_eq!(
            protected.mount_roots(),
            vec![
                PathBuf::from(".nanna"),
                PathBuf::from(".git"),
                PathBuf::from(".github/workflows"),
                PathBuf::from(".github/CODEOWNERS"),
                PathBuf::from("codecov.yml"),
                PathBuf::from("windows.toml"),
                PathBuf::from("cfg"),
            ]
        );
    }

    #[test]
    fn existing_roots_only_reports_paths_present_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".nanna/agents")).unwrap();
        std::fs::write(dir.path().join("codecov.yml"), "coverage: {}").unwrap();
        let protected = ProtectedPaths::with_config_dir(dir.path(), None);
        assert_eq!(
            protected.existing_roots(dir.path()),
            vec![PathBuf::from(".nanna"), PathBuf::from("codecov.yml")]
        );
    }

    #[test]
    fn violation_displays_path_and_rule_and_serialises() {
        let violation = ProtectedPathViolation {
            path: ".nanna/agents/x.toml".to_string(),
            rule: ".nanna/**".to_string(),
        };
        assert_eq!(
            violation.to_string(),
            "`.nanna/agents/x.toml` is protected by rule `.nanna/**`: Nanna may not modify its own configuration"
        );
        let json = serde_json::to_value(&violation).unwrap();
        assert_eq!(json["rule"], ".nanna/**");
        let back: ProtectedPathViolation = serde_json::from_value(json).unwrap();
        assert_eq!(back, violation);
    }

    #[test]
    fn noop_audit_hook_accepts_a_violation() {
        let violation = ProtectedPathViolation {
            path: "codecov.yml".to_string(),
            rule: "codecov.yml".to_string(),
        };
        NoopAuditHook.on_protected_path_violation("task-1", &violation);
        let hook: std::sync::Arc<dyn AuditHook> = std::sync::Arc::new(NoopAuditHook);
        hook.on_protected_path_violation("task-1", &violation);
    }

    #[test]
    fn the_ci_guard_lists_every_protected_pattern_and_reads_the_identity_marker() {
        let guard = include_str!("../../docs/ci/protected-paths-guard.yml");
        for pattern in PROTECTED_PATTERNS {
            let quoted = format!("\"{pattern}\"");
            assert!(
                guard.contains(&quoted),
                "{pattern} missing from the CI guard"
            );
        }
        assert!(guard.contains(crate::marker::IDENTITY_TRAILER));
        assert!(guard.contains("on:\n  pull_request:"));
    }
}
