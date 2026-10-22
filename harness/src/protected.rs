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
use unicode_normalization::UnicodeNormalization;

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
    "tarpaulin.toml",
    "deny.toml",
    "flake.nix",
    "flake.lock",
    ".github/rulesets/**",
    ".githooks/**",
    ".cargo/**",
    ".devcontainer/**",
    "AGENTS.md",
    "evals/cases/**",
    "harness/src/protected.rs",
    ".gitattributes",
    ".gitmodules",
    "harness/src/scope.rs",
    "CODEOWNERS",
    "docs/CODEOWNERS",
    "harness/src/auditor/**",
    ".github/**",
    ".cargo-husky/**",
    ".envrc",
    ".config/**",
    ".claude/**",
    "nix/**",
    "CLAUDE.md",
    "docs/ci/protected-paths-guard.yml",
    "harness/src/workspace.rs",
    "harness/src/tools.rs",
    "harness/src/marker.rs",
    "harness/src/identity/**",
    "harness/src/action_auditor/**",
    "harness/src/windows.rs",
    "harness/src/capabilities.rs",
    "harness/src/container.rs",
    "harness/src/harness_adapter/isolation.rs",
    "harness/src/harness_adapter/checked.rs",
    "scripts/**",
    "tests/security/**",
    "tests/lib/**",
    "tests/run-all-tests.sh",
    "harness/dev-container.toml",
    "harness/src/rollout/executor.rs",
    "harness/src/rollout/hooks.rs",
    "harness/src/rollout/serverless.rs",
    "harness/src/rollout/state.rs",
    "harness/src/effects.rs",
    "harness/src/leases/**",
    "auto-issue-pr.sh",
    "test-agentic-security.sh",
];

const MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: false,
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

fn nfc(text: &str) -> String {
    text.nfc().collect()
}

fn fold_path(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(text) => PathBuf::from(nfc(text)),
        None => path.to_path_buf(),
    }
}

impl Rule {
    fn new(glob: String) -> Self {
        let folded = nfc(&glob);
        let pattern = compile(&folded);
        let directory = folded.strip_suffix(RECURSIVE_SUFFIX).map(compile);
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

/// A literal protected root that can be mounted read-only into a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountRoot {
    /// The repository-relative path of the root.
    pub path: PathBuf,
    /// Whether the root is a directory (a `/**` rule) rather than a file.
    pub directory: bool,
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
    let folded = fold_path(config_dir);
    std::iter::once(repo_root)
        .chain(canonical.as_deref())
        .find(|root| folded.starts_with(fold_path(root)))
        .map(|root| {
            config_dir
                .components()
                .skip(root.components().count())
                .collect::<PathBuf>()
        })
        .filter(|relative| !relative.as_os_str().is_empty())
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
            let escaped = Pattern::escape(&crate::scope::denial_path(&relative));
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
        let folded = fold_path(path);
        match self.rules.iter().find(|rule| rule.matches(&folded)) {
            Some(rule) => Err(ProtectedPathViolation {
                path: crate::scope::denial_path(path),
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
    /// read-only, cover every rule with a literal prefix, with whether each
    /// is a directory. Nested roots are folded into their ancestor.
    pub fn mount_targets(&self) -> Vec<MountRoot> {
        let mut roots: Vec<MountRoot> = Vec::new();
        for rule in &self.rules {
            let Some(path) = rule.mount_root() else {
                continue;
            };
            if roots.iter().any(|root| path.starts_with(&root.path)) {
                continue;
            }
            roots.retain(|root| !root.path.starts_with(&path));
            let directory = rule.glob.ends_with(RECURSIVE_SUFFIX);
            roots.push(MountRoot { path, directory });
        }
        roots
    }

    /// The repository-relative paths of [`ProtectedPaths::mount_targets`].
    pub fn mount_roots(&self) -> Vec<PathBuf> {
        self.mount_targets()
            .into_iter()
            .map(|root| root.path)
            .collect()
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

    const EXAMPLES: &[(&str, &str)] = &[
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
        ("tarpaulin.toml", "tarpaulin.toml"),
        ("deny.toml", "deny.toml"),
        ("flake.nix", "flake.nix"),
        ("flake.lock", "flake.lock"),
        (".github/rulesets/**", ".github/rulesets/main.json"),
        (".github/rulesets/**", ".github/rulesets"),
        (".githooks/**", ".githooks/pre-commit"),
        (".cargo/**", ".cargo/config.toml"),
        (".devcontainer/**", ".devcontainer/devcontainer.json"),
        ("AGENTS.md", "AGENTS.md"),
        ("evals/cases/**", "evals/cases/happy-path-001/case.toml"),
        ("harness/src/protected.rs", "harness/src/protected.rs"),
        (".gitattributes", ".gitattributes"),
        (".gitmodules", ".gitmodules"),
        ("harness/src/scope.rs", "harness/src/scope.rs"),
        ("CODEOWNERS", "CODEOWNERS"),
        ("docs/CODEOWNERS", "docs/CODEOWNERS"),
        ("harness/src/auditor/**", "harness/src/auditor/mod.rs"),
        (".github/**", ".github/actions/setup/action.yml"),
        (".github/**", ".github/dependabot.yml"),
        (".github/**", ".github/ISSUE_TEMPLATE/bug.md"),
        (".cargo-husky/**", ".cargo-husky/hooks/pre-commit"),
        (".envrc", ".envrc"),
        (".config/**", ".config/nextest.toml"),
        (".claude/**", ".claude/settings.json"),
        ("nix/**", "nix/containers.nix"),
        ("CLAUDE.md", "CLAUDE.md"),
        (
            "docs/ci/protected-paths-guard.yml",
            "docs/ci/protected-paths-guard.yml",
        ),
        ("harness/src/workspace.rs", "harness/src/workspace.rs"),
        ("harness/src/tools.rs", "harness/src/tools.rs"),
        ("harness/src/marker.rs", "harness/src/marker.rs"),
        ("harness/src/identity/**", "harness/src/identity/catalog.rs"),
        (
            "harness/src/action_auditor/**",
            "harness/src/action_auditor/gate.rs",
        ),
        ("harness/src/windows.rs", "harness/src/windows.rs"),
        ("harness/src/capabilities.rs", "harness/src/capabilities.rs"),
        ("harness/src/container.rs", "harness/src/container.rs"),
        (
            "harness/src/harness_adapter/isolation.rs",
            "harness/src/harness_adapter/isolation.rs",
        ),
        (
            "harness/src/harness_adapter/checked.rs",
            "harness/src/harness_adapter/checked.rs",
        ),
        ("scripts/**", "scripts/install.sh"),
        ("scripts/**", "scripts/check-flake-source-filter.nix"),
        ("tests/security/**", "tests/security/test-ai-security.sh"),
        ("tests/lib/**", "tests/lib/test-helpers.sh"),
        ("tests/run-all-tests.sh", "tests/run-all-tests.sh"),
        ("harness/dev-container.toml", "harness/dev-container.toml"),
        (
            "harness/src/rollout/executor.rs",
            "harness/src/rollout/executor.rs",
        ),
        (
            "harness/src/rollout/hooks.rs",
            "harness/src/rollout/hooks.rs",
        ),
        (
            "harness/src/rollout/serverless.rs",
            "harness/src/rollout/serverless.rs",
        ),
        (
            "harness/src/rollout/state.rs",
            "harness/src/rollout/state.rs",
        ),
        ("harness/src/effects.rs", "harness/src/effects.rs"),
        ("harness/src/leases/**", "harness/src/leases/store.rs"),
        ("auto-issue-pr.sh", "auto-issue-pr.sh"),
        ("test-agentic-security.sh", "test-agentic-security.sh"),
    ];

    #[test]
    fn every_fixed_pattern_protects_its_example_path() {
        let protected = protected();
        for (rule, path) in EXAMPLES {
            let violation = protected.check(Path::new(path)).unwrap_err();
            assert_eq!(violation.rule, *rule, "{path}");
            assert_eq!(violation.path, *path);
            assert!(protected.is_protected(Path::new(path)), "{path}");
        }
        for pattern in PROTECTED_PATTERNS {
            assert!(
                EXAMPLES.iter().any(|(rule, _)| rule == pattern),
                "{pattern} has no example in the table"
            );
        }
    }

    fn codeowners_paths() -> Vec<String> {
        include_str!("../../.github/CODEOWNERS")
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_whitespace().next())
            .map(|path| path.trim_start_matches('/').to_string())
            .collect()
    }

    #[test]
    fn every_codeowners_entry_is_protected() {
        let protected = protected();
        let entries = codeowners_paths();
        assert!(!entries.is_empty());
        for entry in entries {
            let concrete = entry.replace('*', "example.yml");
            assert!(protected.is_protected(Path::new(&concrete)), "{entry}");
            if entry.ends_with("/*") {
                let nested = format!("{}/nested/file", entry.trim_end_matches("/*"));
                assert!(protected.is_protected(Path::new(&nested)), "{nested}");
            }
        }
    }

    #[test]
    fn every_previously_unprotected_path_is_now_protected() {
        let protected = protected();
        for path in [
            ".gitattributes",
            ".gitmodules",
            "harness/src/scope.rs",
            "CODEOWNERS",
            "docs/CODEOWNERS",
            ".github/CODEOWNERS",
            "harness/src/auditor/mod.rs",
            "harness/src/auditor/review/rules.rs",
            "harness/src/auditor",
            ".github/actions/setup/action.yml",
            ".github/dependabot.yml",
            ".cargo-husky/hooks/pre-commit",
            ".envrc",
            ".config/nextest.toml",
            ".claude/settings.json",
            "nix/containers.nix",
            "CLAUDE.md",
            "docs/ci/protected-paths-guard.yml",
            "harness/src/workspace.rs",
            "harness/src/tools.rs",
            "harness/src/marker.rs",
            "harness/src/identity/schema.rs",
            "harness/src/action_auditor/rules.rs",
            "harness/src/action_auditor",
            "scripts/install.ps1",
            "tests/security/test-environment.sh",
            "harness/src/leases/acquire.rs",
            "harness/src/leases",
            "auto-issue-pr.sh",
        ] {
            assert!(protected.is_protected(Path::new(path)), "{path}");
        }
    }

    fn agents_md_protected_paths_line() -> String {
        include_str!("../../AGENTS.md")
            .lines()
            .find(|line| line.contains("PROTECTED_PATTERNS"))
            .expect("AGENTS.md documents the protected paths")
            .to_string()
    }

    #[test]
    fn agents_md_documents_every_protected_pattern() {
        let line = agents_md_protected_paths_line();
        for pattern in PROTECTED_PATTERNS {
            assert!(
                line.contains(&format!("`{pattern}`")),
                "{pattern} missing from AGENTS.md"
            );
        }
    }

    #[test]
    fn matching_ignores_case() {
        let protected = protected();
        for path in [
            "CODECOV.YML",
            "Tarpaulin.toml",
            "DENY.TOML",
            "Flake.Nix",
            "agents.md",
            ".GITHUB/workflows/ci.yml",
            ".github/WORKFLOWS/ci.yml",
            ".github/codeowners",
            ".Github/Rulesets/main.json",
            ".GIT/config",
            ".NANNA/deploy.toml",
            "crates/api/.Nanna/agents/x.toml",
            ".GitHooks/pre-commit",
            ".Cargo/Config.toml",
            ".DevContainer/devcontainer.json",
            "Evals/Cases/happy-path-001/case.toml",
            "Harness/Src/Protected.rs",
            "Harness/SRC/Auditor/mod.rs",
            ".GitAttributes",
            ".GITMODULES",
            "harness/src/SCOPE.rs",
            "codeowners",
            "Docs/CodeOwners",
            "Windows.TOML",
        ] {
            assert!(protected.is_protected(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn ordinary_repository_paths_are_not_protected() {
        let protected = protected();
        for path in [
            "src/main.rs",
            "README.md",
            "nanna/agents/x.toml",
            "codecov.yml.bak",
            "docs/windows.toml",
            ".nannax/agents/x.toml",
            "evals/datasets/x.json",
            "docs/AGENTS.md",
            "harness/src/protected_extra.rs",
            "auditors/x.rs",
            "auditor/src/lib.rs",
            "harness/src/auditors/x.rs",
            "harness/src/scope_extra.rs",
            "harness/src/identity.rs",
            "harness/src/workspace_extra.rs",
            "src/CLAUDE.md",
            "harness/src/action_auditors/x.rs",
            "harness/src/scheduler/policy.rs",
            "harness/src/rollout/health.rs",
            "harness/src/rollout/executor_extra.rs",
            "harness/src/task.rs",
            "harness/src/lib.rs",
            "harness/src/deploy/validate.rs",
            "harness/src/windows_extra.rs",
            "script/install.sh",
            "tests/integration/run.sh",
            "tests/cachix_integration_test.sh",
            "tests/security_notes.md",
            "docs/scripts/x.md",
            ".gitignore",
            "src/.envrc",
            ".configs/x.toml",
            "nix.md",
            ".claudex/x.json",
            ".githubx/x.yml",
            "src/CODEOWNERS",
            "docs/ci/CODEOWNERS",
            ".gitattributes.bak",
        ] {
            assert!(!protected.is_protected(Path::new(path)), "{path}");
            assert_eq!(protected.check(Path::new(path)), Ok(()));
        }
    }

    mod unicode_folding {
        use super::*;

        const NFD: &str = "cafe\u{301}";
        const PRECOMPOSED: &str = "caf\u{e9}";

        fn with_config(config: &str) -> ProtectedPaths {
            let root = Path::new("/repo");
            ProtectedPaths::with_config_dir(root, Some(&root.join(config)))
        }

        #[test]
        fn a_decomposed_spelling_matches_a_precomposed_configuration_directory() {
            let protected = with_config(&format!("cfg/{PRECOMPOSED}"));
            let candidate = format!("cfg/{NFD}/agents/x.toml");
            let violation = protected.check(Path::new(&candidate)).unwrap_err();
            assert_eq!(violation.path, candidate);
            assert_eq!(violation.rule, format!("cfg/{PRECOMPOSED}/**"));
        }

        #[test]
        fn a_precomposed_spelling_matches_a_decomposed_configuration_directory() {
            let config = format!("cfg/{NFD}");
            let protected = with_config(&config);
            let candidate = format!("cfg/{PRECOMPOSED}/agents/x.toml");
            let violation = protected.check(Path::new(&candidate)).unwrap_err();
            assert_eq!(violation.rule, format!("{config}/**"));
        }

        #[test]
        fn the_directory_itself_and_case_variants_match_in_either_spelling() {
            let protected = with_config(&format!("cfg/{PRECOMPOSED}"));
            assert!(protected.is_protected(Path::new(&format!("cfg/{NFD}"))));
            assert!(protected.is_protected(Path::new(&format!("CFG/{NFD}/x"))));
        }

        #[test]
        fn combining_marks_are_reordered_canonically() {
            let protected = with_config("cfg/a\u{323}\u{301}");
            assert!(protected.is_protected(Path::new("cfg/a\u{301}\u{323}/x")));
            assert!(protected.is_protected(Path::new("cfg/\u{1ea1}\u{301}/x")));
        }

        #[test]
        fn a_decomposed_configuration_directory_under_a_precomposed_root_is_a_rule() {
            let root = PathBuf::from(format!("/home/{PRECOMPOSED}/repo"));
            let config = PathBuf::from(format!("/home/{NFD}/repo/cfg"));
            let protected = ProtectedPaths::with_config_dir(&root, Some(&config));
            assert!(protected.is_protected(Path::new("cfg/agents/x.toml")));
            assert_eq!(protected.config_dir(), Some(config.as_path()));
        }

        #[test]
        fn a_precomposed_configuration_directory_under_a_decomposed_root_is_a_rule() {
            let root = PathBuf::from(format!("/home/{NFD}/repo"));
            let config = PathBuf::from(format!("/home/{PRECOMPOSED}/repo/cfg"));
            let protected = ProtectedPaths::with_config_dir(&root, Some(&config));
            assert!(protected.is_protected(Path::new("cfg/agents/x.toml")));
        }

        #[test]
        fn rules_and_mount_roots_keep_the_configuration_directory_spelling() {
            let config = format!("cfg/{NFD}");
            let protected = with_config(&config);
            assert!(protected.rules().contains(&format!("{config}/**").as_str()));
            assert!(protected.mount_roots().contains(&PathBuf::from(&config)));
        }

        #[test]
        fn compatibility_singletons_fold_onto_ascii_protected_names() {
            let protected = protected();
            for (path, rule) in [
                ("fla\u{212A}e.nix", "flake.nix"),
                ("flake.loc\u{212A}", "flake.lock"),
                (".cargo-hus\u{212A}y/x", ".cargo-husky/**"),
                ("crates/.nanna/\u{212B}", "**/.nanna/**"),
            ] {
                let violation = protected.check(Path::new(path)).unwrap_err();
                assert_eq!(violation.rule, rule, "{path}");
                assert_eq!(violation.path, path);
            }
        }

        #[test]
        fn unrelated_non_ascii_names_stay_unprotected() {
            let protected = with_config(&format!("cfg/{PRECOMPOSED}"));
            for path in [
                format!("cfg/{NFD}x/y"),
                format!("other/{NFD}/y"),
                format!("{NFD}/cfg/y"),
                "docs/caf\u{e8}/y".to_string(),
                "docs/\u{212A}elvin.md".to_string(),
            ] {
                assert!(!protected.is_protected(Path::new(&path)), "{path}");
            }
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

    #[cfg(unix)]
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
    #[should_panic(expected = "protected glob is valid")]
    fn an_uncompilable_pattern_panics_instead_of_being_skipped() {
        Rule::new("[".to_string());
    }

    #[test]
    fn a_descendant_listed_after_its_ancestor_adds_no_mount_root() {
        let protected =
            ProtectedPaths::with_config_dir(Path::new("/repo"), Some(Path::new("/repo/nix/inner")));
        let roots = protected.mount_roots();
        assert!(roots.contains(&PathBuf::from("nix")));
        assert!(!roots.contains(&PathBuf::from("nix/inner")));
        assert!(protected.is_protected(Path::new("nix/inner/x")));
    }

    #[test]
    fn a_later_ancestor_replaces_the_nested_roots_mounted_before_it() {
        let targets = protected().mount_targets();
        let paths: Vec<String> = targets
            .iter()
            .map(|t| t.path.display().to_string())
            .collect();
        for nested in [
            ".github/workflows",
            ".github/CODEOWNERS",
            ".github/rulesets",
        ] {
            assert!(!paths.iter().any(|p| p == nested), "{nested} still mounted");
        }
        assert_eq!(paths.iter().filter(|p| p.as_str() == ".github").count(), 1);
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
                PathBuf::from("codecov.yml"),
                PathBuf::from("windows.toml"),
                PathBuf::from("tarpaulin.toml"),
                PathBuf::from("deny.toml"),
                PathBuf::from("flake.nix"),
                PathBuf::from("flake.lock"),
                PathBuf::from(".githooks"),
                PathBuf::from(".cargo"),
                PathBuf::from(".devcontainer"),
                PathBuf::from("AGENTS.md"),
                PathBuf::from("evals/cases"),
                PathBuf::from("harness/src/protected.rs"),
                PathBuf::from(".gitattributes"),
                PathBuf::from(".gitmodules"),
                PathBuf::from("harness/src/scope.rs"),
                PathBuf::from("CODEOWNERS"),
                PathBuf::from("docs/CODEOWNERS"),
                PathBuf::from("harness/src/auditor"),
                PathBuf::from(".github"),
                PathBuf::from(".cargo-husky"),
                PathBuf::from(".envrc"),
                PathBuf::from(".config"),
                PathBuf::from(".claude"),
                PathBuf::from("nix"),
                PathBuf::from("CLAUDE.md"),
                PathBuf::from("docs/ci/protected-paths-guard.yml"),
                PathBuf::from("harness/src/workspace.rs"),
                PathBuf::from("harness/src/tools.rs"),
                PathBuf::from("harness/src/marker.rs"),
                PathBuf::from("harness/src/identity"),
                PathBuf::from("harness/src/action_auditor"),
                PathBuf::from("harness/src/windows.rs"),
                PathBuf::from("harness/src/capabilities.rs"),
                PathBuf::from("harness/src/container.rs"),
                PathBuf::from("harness/src/harness_adapter/isolation.rs"),
                PathBuf::from("harness/src/harness_adapter/checked.rs"),
                PathBuf::from("scripts"),
                PathBuf::from("tests/security"),
                PathBuf::from("tests/lib"),
                PathBuf::from("tests/run-all-tests.sh"),
                PathBuf::from("harness/dev-container.toml"),
                PathBuf::from("harness/src/rollout/executor.rs"),
                PathBuf::from("harness/src/rollout/hooks.rs"),
                PathBuf::from("harness/src/rollout/serverless.rs"),
                PathBuf::from("harness/src/rollout/state.rs"),
                PathBuf::from("harness/src/effects.rs"),
                PathBuf::from("harness/src/leases"),
                PathBuf::from("auto-issue-pr.sh"),
                PathBuf::from("test-agentic-security.sh"),
                PathBuf::from("cfg"),
            ]
        );
    }

    #[test]
    fn mount_targets_say_which_roots_are_directories() {
        let targets = protected().mount_targets();
        let kinds: Vec<(String, bool)> = targets
            .iter()
            .map(|t| (t.path.display().to_string(), t.directory))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (".nanna".to_string(), true),
                (".git".to_string(), true),
                ("codecov.yml".to_string(), false),
                ("windows.toml".to_string(), false),
                ("tarpaulin.toml".to_string(), false),
                ("deny.toml".to_string(), false),
                ("flake.nix".to_string(), false),
                ("flake.lock".to_string(), false),
                (".githooks".to_string(), true),
                (".cargo".to_string(), true),
                (".devcontainer".to_string(), true),
                ("AGENTS.md".to_string(), false),
                ("evals/cases".to_string(), true),
                ("harness/src/protected.rs".to_string(), false),
                (".gitattributes".to_string(), false),
                (".gitmodules".to_string(), false),
                ("harness/src/scope.rs".to_string(), false),
                ("CODEOWNERS".to_string(), false),
                ("docs/CODEOWNERS".to_string(), false),
                ("harness/src/auditor".to_string(), true),
                (".github".to_string(), true),
                (".cargo-husky".to_string(), true),
                (".envrc".to_string(), false),
                (".config".to_string(), true),
                (".claude".to_string(), true),
                ("nix".to_string(), true),
                ("CLAUDE.md".to_string(), false),
                ("docs/ci/protected-paths-guard.yml".to_string(), false),
                ("harness/src/workspace.rs".to_string(), false),
                ("harness/src/tools.rs".to_string(), false),
                ("harness/src/marker.rs".to_string(), false),
                ("harness/src/identity".to_string(), true),
                ("harness/src/action_auditor".to_string(), true),
                ("harness/src/windows.rs".to_string(), false),
                ("harness/src/capabilities.rs".to_string(), false),
                ("harness/src/container.rs".to_string(), false),
                (
                    "harness/src/harness_adapter/isolation.rs".to_string(),
                    false
                ),
                ("harness/src/harness_adapter/checked.rs".to_string(), false),
                ("scripts".to_string(), true),
                ("tests/security".to_string(), true),
                ("tests/lib".to_string(), true),
                ("tests/run-all-tests.sh".to_string(), false),
                ("harness/dev-container.toml".to_string(), false),
                ("harness/src/rollout/executor.rs".to_string(), false),
                ("harness/src/rollout/hooks.rs".to_string(), false),
                ("harness/src/rollout/serverless.rs".to_string(), false),
                ("harness/src/rollout/state.rs".to_string(), false),
                ("harness/src/effects.rs".to_string(), false),
                ("harness/src/leases".to_string(), true),
                ("auto-issue-pr.sh".to_string(), false),
                ("test-agentic-security.sh".to_string(), false),
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
        let guard = include_str!("../../docs/ci/protected-paths-guard.yml").replace("\r\n", "\n");
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

    #[test]
    fn the_ci_guard_fails_closed_and_matches_case_insensitively() {
        let guard = include_str!("../../docs/ci/protected-paths-guard.yml");
        assert!(guard.contains("shopt -s nocasematch"));
        assert!(guard.contains("PROTECTED_PATTERNS_HUMAN_AUTHORS"));
        assert!(!guard.contains("not an agent PR, protected paths are not enforced here"));
        assert!(!guard.contains("if: steps.identity.outputs.name != ''"));
        assert!(!guard.contains("Human PRs\n# (no marker) are not affected"));
    }

    #[cfg(target_os = "linux")]
    mod guard {
        use super::EXAMPLES;
        use std::path::{Path, PathBuf};
        use std::process::{Command, Output};

        const GUARD: &str = include_str!("../../docs/ci/protected-paths-guard.yml");

        fn step_script(step_name: &str) -> String {
            let marker = format!("- name: {step_name}");
            let start = GUARD.find(&marker).expect("guard step exists");
            let after = &GUARD[start..];
            let run = after.find("run: |\n").expect("step has a run block") + "run: |\n".len();
            after[run..]
                .lines()
                .take_while(|line| line.is_empty() || line.starts_with("          "))
                .map(|line| line.strip_prefix("          ").unwrap_or(line))
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn git(dir: &Path, args: &[&str]) -> String {
            let output = Command::new("git")
                .current_dir(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}: {output:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        struct Repo {
            dir: tempfile::TempDir,
            base: String,
            head: String,
        }

        fn repo_changing(paths: &[&str], message: &str) -> Repo {
            let dir = tempfile::tempdir().unwrap();
            git(dir.path(), &["init", "-q"]);
            std::fs::write(dir.path().join("README.md"), "base").unwrap();
            git(dir.path(), &["add", "-A"]);
            git(dir.path(), &["commit", "-q", "-m", "base"]);
            let base = git(dir.path(), &["rev-parse", "HEAD"]);
            for path in paths {
                let file = dir.path().join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(&file, "changed").unwrap();
            }
            git(dir.path(), &["add", "-A"]);
            git(dir.path(), &["commit", "-q", "-m", message]);
            let head = git(dir.path(), &["rev-parse", "HEAD"]);
            Repo { dir, base, head }
        }

        fn run(repo: &Path, script: &str, env: &[(&str, &str)]) -> Output {
            let mut command = Command::new("bash");
            command.current_dir(repo).arg("-c").arg(script);
            for (key, value) in env {
                command.env(key, value);
            }
            command.output().unwrap()
        }

        fn enforce(repo: &Repo, identity: &str, author: &str, humans: &str) -> Output {
            run(
                repo.dir.path(),
                &step_script("Refuse protected paths unless a listed human authored the PR"),
                &[
                    ("BASE_SHA", &repo.base),
                    ("HEAD_SHA", &repo.head),
                    ("IDENTITY", identity),
                    ("PR_AUTHOR", author),
                    ("PROTECTED_PATTERNS_HUMAN_AUTHORS", humans),
                ],
            )
        }

        fn detect(repo: &Repo, base: &str, head: &str, body: &str) -> (Output, String) {
            let outputs: PathBuf = repo.dir.path().join("github-output");
            let _ = std::fs::remove_file(&outputs);
            let output = run(
                repo.dir.path(),
                &step_script("Detect the Nanna-Identity marker"),
                &[
                    ("BASE_SHA", base),
                    ("HEAD_SHA", head),
                    ("PR_BODY", body),
                    ("GITHUB_OUTPUT", outputs.to_str().unwrap()),
                ],
            );
            let written = std::fs::read_to_string(&outputs).unwrap_or_default();
            (output, written)
        }

        fn stderr(output: &Output) -> String {
            String::from_utf8_lossy(&output.stderr).into_owned()
        }

        #[test]
        fn an_agent_pr_touching_every_protected_example_fails_naming_the_rust_rule() {
            let files: Vec<&(&str, &str)> = EXAMPLES
                .iter()
                .filter(|(_, path)| {
                    !path.starts_with(".git/")
                        && !EXAMPLES
                            .iter()
                            .any(|(_, other)| other.starts_with(&format!("{path}/")))
                })
                .collect();
            let paths: Vec<&str> = files.iter().map(|(_, path)| *path).collect();
            let repo = repo_changing(&paths, "agent work");
            let output = enforce(&repo, "", "agent-bot", "owner");
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            let stderr = stderr(&output);
            for (rule, path) in files {
                assert!(
                    stderr.contains(&format!("  - {path} (rule: {rule})")),
                    "{path} / {rule} missing from:\n{stderr}"
                );
            }
        }

        #[test]
        fn a_listed_human_author_may_touch_protected_paths_case_insensitively() {
            let repo = repo_changing(&["flake.nix"], "human work");
            let output = enforce(&repo, "", "OwNeR", " other , owner ");
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("listed human author"));
        }

        #[test]
        fn an_unlisted_author_or_an_empty_allow_list_fails_closed() {
            let repo = repo_changing(&["flake.nix"], "work");
            assert_eq!(
                enforce(&repo, "", "stranger", "owner").status.code(),
                Some(1)
            );
            assert_eq!(enforce(&repo, "", "owner", "").status.code(), Some(1));
            assert_eq!(enforce(&repo, "", "owner", " , ").status.code(), Some(1));
        }

        #[test]
        fn an_identity_marker_blocks_even_a_listed_human() {
            let repo = repo_changing(&["flake.nix"], "work");
            let output = enforce(&repo, "rust-implementer", "owner", "owner");
            assert_eq!(output.status.code(), Some(1), "{output:?}");
            assert!(stderr(&output).contains("identity marker: 'rust-implementer'"));
        }

        #[test]
        fn a_pr_touching_only_ordinary_paths_passes_for_anyone() {
            let repo = repo_changing(&["src/main.rs", "docs/notes.md"], "work");
            let output = enforce(&repo, "rust-implementer", "stranger", "");
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("no protected path touched"));
        }

        #[test]
        fn matching_is_case_insensitive_and_nested_nanna_directories_are_caught() {
            let repo = repo_changing(&["FLAKE.NIX", "crates/api/.Nanna/agents/x.toml"], "work");
            let stderr = stderr(&enforce(&repo, "", "stranger", "owner"));
            assert!(
                stderr.contains("  - FLAKE.NIX (rule: flake.nix)"),
                "{stderr}"
            );
            assert!(
                stderr.contains("  - crates/api/.Nanna/agents/x.toml (rule: **/.nanna/**)"),
                "{stderr}"
            );
        }

        #[test]
        fn unresolvable_commits_fail_closed_in_marker_detection() {
            let repo = repo_changing(&["src/a.rs"], "work");
            let zeros = "0000000000000000000000000000000000000000";
            for (base, head) in [(zeros, repo.head.as_str()), (repo.base.as_str(), zeros)] {
                let (output, written) = detect(&repo, base, head, "");
                assert_eq!(output.status.code(), Some(1), "{output:?}");
                assert!(String::from_utf8_lossy(&output.stdout).contains("fail-closed"));
                assert!(written.is_empty());
            }
        }

        #[test]
        fn the_marker_is_read_from_the_body_or_a_commit_trailer_and_absent_otherwise() {
            let repo = repo_changing(&["src/a.rs"], "work\n\nNanna-Identity: from-commit");
            let (output, written) = detect(&repo, &repo.base, &repo.head, "");
            assert_eq!(output.status.code(), Some(0), "{output:?}");
            assert_eq!(written, "name=from-commit\n");
            let plain = repo_changing(&["src/a.rs"], "work");
            let (_, written) = detect(
                &plain,
                &plain.base,
                &plain.head,
                "text\n<!-- Nanna-Identity: from-body -->\n",
            );
            assert_eq!(written, "name=from-body\n");
            let (_, written) = detect(&plain, &plain.base, &plain.head, "no marker here");
            assert_eq!(written, "name=\n");
        }
    }
}
