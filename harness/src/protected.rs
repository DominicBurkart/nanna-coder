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
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
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
    ".gitattributes",
    ".gitmodules",
    "CODEOWNERS",
    "docs/CODEOWNERS",
    ".github/**",
    ".cargo-husky/**",
    ".envrc",
    ".config/**",
    ".claude/**",
    "nix/**",
    "CLAUDE.md",
];

const MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: false,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

const RECURSIVE_SUFFIX: &str = "/**";

pub const EXTRAS_FILE: &str = ".nanna/protected.toml";

pub const MAX_EXTRAS_BYTES: usize = 64 * 1024;

pub const MAX_EXTRAS_ENTRIES: usize = 256;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtectedConfigError {
    #[error("`{EXTRAS_FILE}` is unreadable: {0}")]
    Unreadable(String),
    #[error("`{EXTRAS_FILE}` is malformed: {0}")]
    Malformed(String),
    #[error("`{EXTRAS_FILE}` declares the invalid pattern `{pattern}`: {reason}")]
    InvalidPattern { pattern: String, reason: String },
    #[error("`{EXTRAS_FILE}` is larger than {MAX_EXTRAS_BYTES} bytes")]
    TooLarge,
    #[error(
        "`{EXTRAS_FILE}` declares {count} patterns, more than the {MAX_EXTRAS_ENTRIES} allowed"
    )]
    TooManyEntries { count: usize },
}

/// Extra protected globs a repository declares in `.nanna/protected.toml`.
/// They only add to [`PROTECTED_PATTERNS`]; a malformed declaration is an
/// error, never an empty set.
///
/// ```
/// use harness::protected::{ProtectedExtras, ProtectedPaths};
/// use std::path::Path;
///
/// let extras = ProtectedExtras::parse(r#"extra = ["src/gate.rs", "ci/**"]"#).unwrap();
/// let protected = ProtectedPaths::for_repo_with_extras(Path::new("."), &extras);
/// assert!(protected.is_protected(Path::new("src/gate.rs")));
/// assert!(protected.is_protected(Path::new("ci/deploy.sh")));
/// assert!(protected.is_protected(Path::new(".git/config")));
/// assert!(ProtectedExtras::parse("remove = [\".git/**\"]").is_err());
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProtectedExtras(Vec<String>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtrasFile {
    #[serde(default)]
    extra: Vec<String>,
}

fn read_bounded(reader: impl std::io::Read) -> std::io::Result<String> {
    let mut text = String::new();
    let mut bounded = std::io::Read::take(reader, MAX_EXTRAS_BYTES as u64 + 1);
    std::io::Read::read_to_string(&mut bounded, &mut text)?;
    Ok(text)
}

fn invalid_pattern(pattern: &str, reason: &str) -> ProtectedConfigError {
    ProtectedConfigError::InvalidPattern {
        pattern: pattern.to_string(),
        reason: reason.to_string(),
    }
}

fn validate_extra(pattern: &str) -> Result<(), ProtectedConfigError> {
    let literal = pattern.strip_suffix(RECURSIVE_SUFFIX).unwrap_or(pattern);
    if literal.is_empty() {
        return Err(invalid_pattern(pattern, "the pattern is empty"));
    }
    if literal.starts_with('/') {
        return Err(invalid_pattern(pattern, "the pattern must be relative"));
    }
    if literal.starts_with('!') {
        return Err(invalid_pattern(pattern, "patterns cannot be negated"));
    }
    if literal.contains(['*', '?', '[', ']', '{', '}', '\\', '\n', '\r', '\0']) {
        return Err(invalid_pattern(
            pattern,
            "use a literal path, optionally ending in `/**`",
        ));
    }
    if literal
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid_pattern(
            pattern,
            "the path must be normalised and stay inside the repository",
        ));
    }
    if literal
        .split('/')
        .any(|part| part.ends_with(['.', ' ']) || part.contains(':'))
    {
        return Err(invalid_pattern(
            pattern,
            "a path component cannot end in a dot or a space or contain a colon, because Windows reads those as another spelling of the same name",
        ));
    }
    Ok(())
}

impl ProtectedExtras {
    pub fn parse(text: &str) -> Result<Self, ProtectedConfigError> {
        if text.len() > MAX_EXTRAS_BYTES {
            return Err(ProtectedConfigError::TooLarge);
        }
        let file: ExtrasFile =
            toml::from_str(text).map_err(|e| ProtectedConfigError::Malformed(e.to_string()))?;
        if file.extra.len() > MAX_EXTRAS_ENTRIES {
            return Err(ProtectedConfigError::TooManyEntries {
                count: file.extra.len(),
            });
        }
        file.extra
            .iter()
            .try_for_each(|pattern| validate_extra(pattern))?;
        Ok(Self(file.extra))
    }

    pub fn load(base: &Path) -> Result<Self, ProtectedConfigError> {
        let unreadable =
            |e: &dyn std::fmt::Display| ProtectedConfigError::Unreadable(e.to_string());
        let path = base.join(EXTRAS_FILE);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Self::absent(base).map_err(|e| unreadable(&e))
            }
            Err(e) => return Err(unreadable(&e)),
            Ok(meta) if !meta.is_file() => return Err(unreadable(&"not a regular file")),
            Ok(_) => {}
        }
        let text = std::fs::File::open(&path)
            .and_then(read_bounded)
            .map_err(|e| unreadable(&e))?;
        Self::parse(&text)
    }

    fn absent(base: &Path) -> Result<Self, &'static str> {
        if !base.is_dir() {
            return Err("the base checkout is not a directory");
        }
        match std::fs::symlink_metadata(base.join(".nanna")) {
            Ok(meta) if !meta.is_dir() => Err("`.nanna` is not a directory"),
            _ => Ok(Self::default()),
        }
    }

    pub fn globs(&self) -> &[String] {
        &self.0
    }
}

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

/// The rule reported for a path with so many short-name components that its
/// spellings cannot all be enumerated.
pub const ALIAS_OVERFLOW_RULE: &str = "ambiguous short-name path";

const MAX_ALIAS_VARIANTS: usize = 1024;
const MAX_ALIAS_COMPONENTS: usize = 1024;
const STREAM_SEPARATOR: char = ':';
const SHORT_NAME_TILDE: char = '~';
const SHORT_NAME_WIDTH: usize = 8;
const SHORT_NAME_MAX_PREFIX: usize = 6;
const SHORT_NAME_MAX_EXTENSION: usize = 3;
const HASHED_SHORT_NAME_KEPT: usize = 2;

fn fold_component(component: &str) -> String {
    let without_stream = component
        .split(STREAM_SEPARATOR)
        .next()
        .unwrap_or(component);
    let trimmed = without_stream.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        component.to_string()
    } else {
        trimmed.to_string()
    }
}

fn short_name_parts(component: &str) -> Option<(&str, &str)> {
    let (base, extension) = component.rsplit_once('.').unwrap_or((component, ""));
    let (prefix, digits) = base.rsplit_once(SHORT_NAME_TILDE)?;
    let well_formed = !prefix.is_empty()
        && !prefix.contains('.')
        && !digits.is_empty()
        && digits.chars().all(|c| c.is_ascii_digit())
        && prefix.chars().count() + 1 + digits.len() <= SHORT_NAME_WIDTH;
    well_formed.then_some((prefix, extension))
}

fn short_name_char(c: char) -> char {
    if matches!(c, '+' | ',' | ';' | '=' | '[' | ']') {
        '_'
    } else {
        c
    }
}

fn long_name_parts(literal: &str) -> (String, String) {
    let stem = literal.trim_start_matches(['.', ' ']);
    let (base, extension) = stem.rsplit_once('.').unwrap_or((stem, ""));
    let base: String = base
        .chars()
        .filter(|c| !matches!(c, '.' | ' '))
        .map(short_name_char)
        .collect();
    let extension: String = extension
        .chars()
        .take(SHORT_NAME_MAX_EXTENSION)
        .map(short_name_char)
        .collect();
    (base.to_lowercase(), extension.to_lowercase())
}

fn is_short_name_of(literal: &str, prefix: &str, extension: &str) -> bool {
    let (base, long_extension) = long_name_parts(literal);
    if extension.to_lowercase() != long_extension {
        return false;
    }
    let prefix = prefix.to_lowercase();
    if !base.is_ascii() {
        return true;
    }
    let overlaps =
        base.starts_with(&prefix) || (!long_extension.is_empty() && prefix.starts_with(&base));
    let hashed = prefix.chars().count() == SHORT_NAME_MAX_PREFIX
        && prefix.is_ascii()
        && prefix[..HASHED_SHORT_NAME_KEPT] == *base.get(..HASHED_SHORT_NAME_KEPT).unwrap_or("")
        && prefix[HASHED_SHORT_NAME_KEPT..]
            .chars()
            .all(|c| c.is_ascii_hexdigit());
    overlaps || hashed
}

fn literal_components(glob: &str) -> impl Iterator<Item = String> + '_ {
    glob.split('/').map(str::to_string)
}

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
    PathBuf::from(nfc(&path.to_string_lossy()))
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
    literals: Vec<String>,
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
        Self::with_extras(repo_root, config_dir, &ProtectedExtras::default())
    }

    pub fn for_repo_with_extras(repo_root: &Path, extras: &ProtectedExtras) -> Self {
        let agents_dir = IdentityCatalog::default_global_dir();
        let config_dir = agents_dir.as_deref().and_then(Path::parent);
        Self::with_extras(repo_root, config_dir, extras)
    }

    pub fn with_extras(
        repo_root: &Path,
        config_dir: Option<&Path>,
        extras: &ProtectedExtras,
    ) -> Self {
        let mut rules: Vec<Rule> = PROTECTED_PATTERNS
            .iter()
            .map(|glob| Rule::new(glob.to_string()))
            .collect();
        let mut literals: Vec<String> = PROTECTED_PATTERNS
            .iter()
            .flat_map(|glob| literal_components(glob))
            .collect();
        if let Some(relative) = config_dir.and_then(|dir| relative_config_dir(repo_root, dir)) {
            let raw = crate::scope::denial_path(&relative);
            let escaped = Pattern::escape(&raw);
            rules.push(Rule::new(format!("{escaped}{RECURSIVE_SUFFIX}")));
            literals.extend(raw.split('/').map(str::to_string));
        }
        rules.extend(extras.globs().iter().cloned().map(Rule::new));
        literals.extend(
            extras
                .globs()
                .iter()
                .flat_map(|glob| literal_components(glob)),
        );
        let mut literals: Vec<String> = literals.iter().map(|literal| nfc(literal)).collect();
        literals.sort_by_key(|literal| literal.to_lowercase());
        literals.dedup_by_key(|literal| literal.to_lowercase());
        Self {
            rules,
            config_dir: config_dir.map(Path::to_path_buf),
            literals,
        }
    }

    pub(crate) fn variants(&self, path: &Path) -> Option<Vec<PathBuf>> {
        let path = fold_path(path);
        let mut variants = vec![path.clone()];
        let options: Vec<Vec<OsString>> = path
            .components()
            .map(|component| match component {
                Component::Normal(name) => self.component_options(name.to_string_lossy().as_ref()),
                other => vec![other.as_os_str().to_os_string()],
            })
            .collect();
        let mut count = 1_usize;
        for choices in &options {
            count = count.saturating_mul(choices.len());
            if count > MAX_ALIAS_VARIANTS {
                return None;
            }
        }
        if count == 1 {
            let only: PathBuf = options.into_iter().flatten().collect();
            if only != path {
                variants.push(only);
            }
            return Some(variants);
        }
        if options.len() > MAX_ALIAS_COMPONENTS {
            return None;
        }
        let mut spellings: Vec<Vec<OsString>> = vec![Vec::new()];
        for choices in &options {
            let mut next = Vec::with_capacity(spellings.len() * choices.len());
            for prefix in &spellings {
                for option in choices {
                    let mut spelling = prefix.clone();
                    spelling.push(option.clone());
                    next.push(spelling);
                }
            }
            spellings = next;
        }
        variants.extend(spellings.into_iter().map(PathBuf::from_iter));
        Some(variants)
    }

    fn component_options(&self, component: &str) -> Vec<OsString> {
        let folded = fold_component(component);
        let mut options = vec![OsString::from(&folded)];
        if let Some((prefix, extension)) = short_name_parts(&folded) {
            options.extend(
                self.literals
                    .iter()
                    .filter(|literal| is_short_name_of(literal, prefix, extension))
                    .map(OsString::from),
            );
        }
        options
    }

    /// Whether the repository-relative `path` may not be written.
    pub fn is_protected(&self, path: &Path) -> bool {
        self.check(path).is_err()
    }

    /// The rule protecting the repository-relative `path`, as an error.
    pub fn check(&self, path: &Path) -> Result<(), ProtectedPathViolation> {
        let Some(variants) = self.variants(path) else {
            return Err(ProtectedPathViolation {
                path: crate::scope::denial_path(path),
                rule: ALIAS_OVERFLOW_RULE.to_string(),
            });
        };
        let hit = self
            .rules
            .iter()
            .find(|rule| variants.iter().any(|variant| rule.matches(variant)));
        match hit {
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
        (".gitattributes", ".gitattributes"),
        (".gitmodules", ".gitmodules"),
        ("CODEOWNERS", "CODEOWNERS"),
        ("docs/CODEOWNERS", "docs/CODEOWNERS"),
        (".github/**", ".github/actions/setup/action.yml"),
        (".github/**", ".github/dependabot.yml"),
        (".github/**", ".github/ISSUE_TEMPLATE/bug.md"),
        (".cargo-husky/**", ".cargo-husky/hooks/pre-commit"),
        (".envrc", ".envrc"),
        (".config/**", ".config/nextest.toml"),
        (".claude/**", ".claude/settings.json"),
        ("nix/**", "nix/containers.nix"),
        ("CLAUDE.md", "CLAUDE.md"),
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

    const MOVED_TO_DECLARATION: [&str; 44] = [
        "evals/cases/**",
        "harness/src/protected.rs",
        "harness/src/scope.rs",
        "harness/src/auditor/**",
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
        "harness/src/pr_tools.rs",
        "docs/pr-lifecycle-tools.md",
        "harness/src/budget.rs",
        "harness/src/ci_tools.rs",
        "harness/src/ci_policy.rs",
        "harness/src/ci_integrity.rs",
        "harness/src/deploy/template.rs",
        "harness/src/sandbox_tools.rs",
        "harness/tests/spawn_gate_structural.rs",
        "harness/tests/protected_audit_hook_structural.rs",
        "harness/tests/escalation_sinks_structural.rs",
        "harness/tests/fixtures/identities/global/auditor.toml",
        "harness/tests/fixtures/identities/global/prompts/auditor.md",
        "harness/tests/fixtures/one_track/catalog/auditor.toml",
        "harness/tests/fixtures/one_track/catalog/prompts/auditor.md",
        "harness/tests/planner_structural.rs",
    ];

    fn declaration_example_path(pattern: &str) -> String {
        match pattern.strip_suffix("/**") {
            Some(prefix) => format!("{prefix}/sub/x.txt"),
            None => pattern.to_string(),
        }
    }

    fn own_declaration() -> ProtectedExtras {
        ProtectedExtras::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap()
    }

    #[test]
    fn nanna_coders_own_declaration_is_exactly_the_patterns_that_left_the_shipped_list() {
        assert_eq!(own_declaration().globs(), MOVED_TO_DECLARATION);
    }

    #[test]
    fn none_of_the_patterns_that_left_the_shipped_list_is_protected_by_default() {
        let protected = protected();
        for pattern in MOVED_TO_DECLARATION {
            assert!(!PROTECTED_PATTERNS.contains(&pattern), "{pattern}");
            let path = declaration_example_path(pattern);
            assert!(!protected.is_protected(Path::new(&path)), "{path}");
        }
    }

    #[test]
    fn with_its_own_declaration_loaded_every_moved_path_is_protected_by_its_declared_rule() {
        let protected = ProtectedPaths::with_extras(Path::new("/repo"), None, &own_declaration());
        for pattern in MOVED_TO_DECLARATION {
            let path = declaration_example_path(pattern);
            let violation = protected.check(Path::new(&path)).unwrap_err();
            assert_eq!(violation.rule, pattern, "{path}");
        }
        let declaration = protected
            .check(Path::new(".nanna/protected.toml"))
            .unwrap_err();
        assert_eq!(declaration.rule, ".nanna/**");
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

    fn files_under(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let skipped = dir == root
                    && [".git", "target", ".claude"]
                        .contains(&entry.file_name().to_str().unwrap_or(""));
                if !skipped {
                    files_under(root, &path, out);
                }
            } else if kind.is_file() {
                out.push(path);
            }
        }
    }

    #[test]
    fn the_auditor_card_walk_skips_build_and_vcs_directories_only_at_the_repository_root() {
        let dir = tempfile::tempdir().unwrap();
        for relative in [
            "target/a.toml",
            ".git/b.toml",
            ".claude/c.toml",
            "x/target/d.toml",
            "x/.git/e.toml",
            "x/.claude/f.toml",
            "g.toml",
        ] {
            let file = dir.path().join(relative);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "").unwrap();
        }
        let mut found = Vec::new();
        files_under(dir.path(), dir.path(), &mut found);
        let mut found: Vec<String> = found
            .iter()
            .map(|file| {
                file.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        found.sort();
        assert_eq!(
            found,
            [
                "g.toml",
                "x/.claude/f.toml",
                "x/.git/e.toml",
                "x/target/d.toml"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_auditor_card_walk_follows_no_symlinks_and_survives_unreadable_directories() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("o.toml"), "").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("o.toml"), dir.path().join("l.toml"))
            .unwrap();
        let mut found = Vec::new();
        files_under(dir.path(), dir.path(), &mut found);
        assert!(found.is_empty(), "{found:?}");
        files_under(dir.path(), &dir.path().join("missing"), &mut found);
        assert!(found.is_empty());
    }

    fn auditor_identity_files() -> Vec<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        files_under(&root, &root, &mut files);
        let mut found = Vec::new();
        for file in files {
            if file.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            let Ok(card) = std::fs::read_to_string(&file) else {
                continue;
            };
            let Ok(value) = card.parse::<toml::Table>() else {
                continue;
            };
            let named_auditor = value
                .get("identity")
                .and_then(|identity| identity.get("name"))
                .and_then(toml::Value::as_str)
                == Some(crate::identity::AUDITOR_IDENTITY);
            if !named_auditor {
                continue;
            }
            let relative = |path: &Path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            };
            found.push(relative(&file));
            let prompt = value
                .get("identity")
                .and_then(|identity| identity.get("system_prompt"))
                .and_then(toml::Value::as_str);
            if let Some(prompt) = prompt {
                found.push(relative(&file.parent().unwrap().join(prompt)));
            }
        }
        found.sort();
        found
    }

    #[test]
    fn every_auditor_identity_card_and_prompt_in_the_repository_is_protected() {
        let protected = ProtectedPaths::with_extras(Path::new("/repo"), None, &own_declaration());
        let found = auditor_identity_files();
        for expected in [
            "evals/cases/auditor_heldout/catalog/auditor.toml",
            "evals/cases/auditor_spawn/catalog/auditor.toml",
            "harness/tests/fixtures/identities/global/auditor.toml",
            "harness/tests/fixtures/identities/global/prompts/auditor.md",
            "harness/tests/fixtures/one_track/catalog/auditor.toml",
            "harness/tests/fixtures/one_track/catalog/prompts/auditor.md",
        ] {
            assert!(found.iter().any(|file| file == expected), "{expected}");
        }
        for file in &found {
            assert!(
                protected.is_protected(Path::new(file)),
                "{file} defines the auditor identity but is not protected"
            );
        }
    }

    #[test]
    fn every_auditor_identity_card_in_the_repository_has_no_effect_ceiling_above_none() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let cards: Vec<String> = auditor_identity_files()
            .into_iter()
            .filter(|file| file.ends_with(".toml"))
            .collect();
        assert!(!cards.is_empty());
        for card in cards {
            let value = std::fs::read_to_string(root.join(&card))
                .unwrap()
                .parse::<toml::Table>()
                .unwrap();
            let max_effect = value
                .get("scope")
                .and_then(|scope| scope.get("max_effect"))
                .and_then(toml::Value::as_str);
            assert_eq!(max_effect, Some("none"), "{card}");
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
            ".GitAttributes",
            ".GITMODULES",
            "codeowners",
            "Docs/CodeOwners",
            "Windows.TOML",
        ] {
            assert!(protected.is_protected(Path::new(path)), "{path}");
        }
    }

    mod windows_aliases {
        use super::*;

        fn with_config_dir_at(config: &str) -> ProtectedPaths {
            let root = Path::new("/repo");
            ProtectedPaths::with_config_dir(root, Some(&root.join(config)))
        }

        fn rule_of(protected: &ProtectedPaths, path: &str) -> Option<String> {
            protected.check(Path::new(path)).err().map(|v| v.rule)
        }

        #[test]
        fn trailing_dots_and_spaces_fold_onto_the_protected_name() {
            let protected = protected();
            for (path, rule) in [
                ("flake.nix.", "flake.nix"),
                ("flake.nix ", "flake.nix"),
                ("flake.nix. .", "flake.nix"),
                ("FLAKE.NIX...", "flake.nix"),
                ("codecov.yml  ", "codecov.yml"),
                (".github./workflows/ci.yml", ".github/workflows/**"),
                (".github/workflows /ci.yml", ".github/workflows/**"),
                (".nanna /agents/x.toml", ".nanna/**"),
                ("crates/api/.nanna./x.toml", "**/.nanna/**"),
                ("AGENTS.md.", "AGENTS.md"),
                (".git./config", ".git/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
        }

        #[test]
        fn a_folded_violation_reports_the_spelling_that_was_written() {
            let violation = protected().check(Path::new("flake.nix. ")).unwrap_err();
            assert_eq!(violation.path, "flake.nix. ");
            assert_eq!(violation.rule, "flake.nix");
        }

        #[test]
        fn folding_never_empties_a_component_or_touches_dot_components() {
            let protected = protected();
            for path in [
                "...",
                "src/.../main.rs",
                "docs/. /x",
                "src/main.rs.",
                "a. b",
                " /flake.nix",
                ". /flake.nix",
                "... /codecov.yml",
            ] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn names_that_only_resemble_a_protected_name_stay_unprotected() {
            let protected = protected();
            for path in [
                "flake.nix.bak",
                "flake.nixx.",
                "xflake.nix.",
                "codecov.yml.bak ",
            ] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn a_stream_suffix_addresses_the_protected_file() {
            let protected = protected();
            for (path, rule) in [
                ("flake.nix:stream", "flake.nix"),
                ("flake.nix::$DATA", "flake.nix"),
                ("flake.nix.:x", "flake.nix"),
                (".github/workflows/ci.yml:x", ".github/workflows/**"),
                (".github:x/workflows/ci.yml", ".github/workflows/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
            for path in ["notes:flake.nix", ":flake.nix", "src/a:b.rs"] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn short_names_of_protected_files_and_directories_are_refused() {
            let protected = protected();
            for (path, rule) in [
                ("CODECO~1.YML", "codecov.yml"),
                ("codeco~1.yml", "codecov.yml"),
                ("TARPAU~1.TOM", "tarpaulin.toml"),
                ("WINDOW~1.TOM", "windows.toml"),
                ("DENY~1.TOM", "deny.toml"),
                ("FLAKEL~1.LOC", "flake.lock"),
                ("FLAKEN~1.NIX", "flake.nix"),
                ("FLAKE~12.NIX", "flake.nix"),
                ("GITHUB~1/WORKFL~1/ci.yml", ".github/workflows/**"),
                ("GITHUB~1/CODEOW~1", ".github/CODEOWNERS"),
                ("NANNA~1/agents/x.toml", ".nanna/**"),
                ("crates/NANNA~1/x.toml", "**/.nanna/**"),
                ("GIT~1/config", ".git/**"),
                ("DEVCON~1/devcontainer.json", ".devcontainer/**"),
                ("docs/CODEOW~1", "docs/CODEOWNERS"),
                ("FL1A2B~1.NIX", "flake.nix"),
                ("CO3F9A~2.YML", "codecov.yml"),
                ("CLAUDE~1.MD", "CLAUDE.md"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
        }

        #[test]
        fn short_names_that_cannot_alias_a_protected_name_are_allowed() {
            let protected = protected();
            for path in [
                "README~1.MD",
                "XLAKEN~1.NIX",
                "FLAKEN~1.TXT",
                "FLAKEN~1.NIXX",
                "src/FLAKEN~1.NIX",
                "flake~.nix",
                "~1.NIX",
                "FLAKEN~.NIX",
                "FLAKEN~1234567.NIX",
                "FLAKENIX~1.NIX",
                "FLAKEN~1.NI.X",
                "FLA.KE~1.NIX",
                "ZZ1A2B~1.NIX",
                "FLZZZZ~1.NIX",
                "docs/GITHUB~1",
                "GITLAB~1/x",
                "FLAKEN~12.NIX",
                "FLAKE.~1.NIX",
                "FLAKE~A.NIX",
                "FA1A2B~1.NIX",
            ] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn a_path_with_too_many_short_name_spellings_fails_closed() {
            let protected = protected();
            let many = "src/G~1/G~1/G~1/G~1/G~1/G~1/G~1/G~1/x.rs";
            let violation = protected.check(Path::new(many)).unwrap_err();
            assert_eq!(violation.rule, ALIAS_OVERFLOW_RULE);
            assert_eq!(violation.path, many);
            let few = "src/G~1/G~1/x.rs";
            assert!(!protected.is_protected(Path::new(few)));
        }

        #[test]
        fn the_variant_cap_is_exactly_1024_spellings() {
            let protected = protected();
            let at_cap = ["CODECO~1.YML"; 10].join("/");
            assert!(!protected.is_protected(Path::new(&at_cap)));
            let over_cap = ["CODECO~1.YML"; 11].join("/");
            let violation = protected.check(Path::new(&over_cap)).unwrap_err();
            assert_eq!(violation.rule, ALIAS_OVERFLOW_RULE);
        }

        #[test]
        fn a_very_deep_path_with_nothing_aliasable_is_checked_in_linear_time() {
            let protected = protected();
            let deep = ["a"; 50_000].join("/");
            let started = std::time::Instant::now();
            assert!(protected.check(Path::new(&deep)).is_ok());
            assert!(started.elapsed() < std::time::Duration::from_secs(2));
            let variants = protected.variants(Path::new(&deep)).unwrap();
            assert_eq!(variants.len(), 1);
        }

        #[test]
        fn a_deep_path_still_reports_its_folded_spelling_as_a_second_variant() {
            let protected = protected();
            let deep = format!("{}/flake.nix.", ["a"; 3_000].join("/"));
            let variants = protected.variants(Path::new(&deep)).unwrap();
            assert_eq!(variants.len(), 2);
            assert!(variants[1].ends_with("flake.nix"));
        }

        #[test]
        fn a_deep_path_with_an_aliasable_component_fails_closed_past_the_component_cap() {
            let protected = protected();
            let within = format!("{}/CODECO~1.YML", ["a"; 1_000].join("/"));
            assert!(protected.variants(Path::new(&within)).is_some());
            let beyond = format!("{}/CODECO~1.YML", ["a"; 1_100].join("/"));
            let violation = protected.check(Path::new(&beyond)).unwrap_err();
            assert_eq!(violation.rule, ALIAS_OVERFLOW_RULE);
        }

        #[test]
        fn a_configuration_directory_with_a_trailing_dot_matches_as_written() {
            let protected = with_config_dir_at("dot./state");
            assert!(protected.is_protected(Path::new("dot./state/x")));
            assert!(!protected.is_protected(Path::new("dot./other/x")));
        }

        #[test]
        fn a_non_ascii_configuration_directory_matches_any_short_name_prefix() {
            let protected = with_config_dir_at("cfg/\u{e9}tat-courant");
            assert!(protected.is_protected(Path::new("cfg/_TAT-C~1/x")));
            assert!(protected.is_protected(Path::new("cfg/XYZ~1/x")));
            assert!(!protected.is_protected(Path::new("cfg/XYZ~1.TXT/x")));
            assert!(!protected.is_protected(Path::new("other/XYZ~1/x")));
        }

        #[test]
        fn a_configuration_directory_has_short_name_aliases_too() {
            let protected = with_config_dir_at("cfg/nanna-state");
            assert!(protected.is_protected(Path::new("cfg/NANNA-~1/agents/x.toml")));
            assert!(protected.is_protected(Path::new("CFG/nanna-state./x")));
            assert!(!protected.is_protected(Path::new("cfg/OTHER~1/x")));
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
            "harness/src/pr_tool.rs",
            "harness/src/pr_tools.rs.bak",
            "harness/src/pr_tools",
            "other/harness/src/pr_tools.rs",
            "harness/pr_tools.rs",
            "docs/pr-lifecycle-tools.md.bak",
            "docs/other/pr-lifecycle-tools.md",
            "docs/pr-lifecycle-tool.md",
            "harness/src/budget.rs.bak",
            "harness/src/budget",
            "other/harness/src/budget.rs",
            "harness/budget.rs",
            "harness/src/budgets.rs",
            "harness/src/ci_tools.rs.bak",
            "other/harness/src/ci_tools.rs",
            "harness/src/ci_toolsx.rs",
            "harness/src/ci_policy.rs.bak",
            "other/harness/src/ci_policy.rs",
            "harness/src/ci_policyx.rs",
            "harness/src/ci_integrity.rs.bak",
            "other/harness/src/ci_integrity.rs",
            "harness/src/ci_integrityx.rs",
            "harness/src/deploy/template.rs.bak",
            "other/harness/src/deploy/template.rs",
            "harness/src/deploy/templatex.rs",
            "harness/src/sandbox_tools.rs.bak",
            "other/harness/src/sandbox_tools.rs",
            "harness/src/sandbox_toolsx.rs",
            "harness/tests/spawn_gate_structural.rs.bak",
            "other/harness/tests/spawn_gate_structural.rs",
            "harness/tests/protected_audit_hook_structural.rs.bak",
            "other/harness/tests/protected_audit_hook_structural.rs",
            "harness/tests/escalation_sinks_structural.rs.bak",
            "other/harness/tests/escalation_sinks_structural.rs",
            "harness/tests/fixtures/identities/global/auditor.toml.bak",
            "other/harness/tests/fixtures/identities/global/auditor.toml",
            "harness/tests/fixtures/identities/global/prompts/auditor.md.bak",
            "other/harness/tests/fixtures/identities/global/prompts/auditor.md",
            "harness/tests/fixtures/one_track/catalog/auditor.toml.bak",
            "other/harness/tests/fixtures/one_track/catalog/auditor.toml",
            "harness/tests/fixtures/one_track/catalog/prompts/auditor.md.bak",
            "other/harness/tests/fixtures/one_track/catalog/prompts/auditor.md",
            "harness/tests/planner_structural.rs.bak",
            "other/harness/tests/planner_structural.rs",
            "harness/src/pr_tool.rs",
            "harness/src/pr_tools.rs.bak",
            "harness/src/pr_tools",
            "other/harness/src/pr_tools.rs",
            "harness/pr_tools.rs",
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
        fn the_directory_itself_matches_when_the_configuration_directory_is_decomposed() {
            let protected = with_config(&format!("cfg/{NFD}"));
            assert!(protected.is_protected(Path::new(&format!("cfg/{PRECOMPOSED}"))));
        }

        #[test]
        fn a_configuration_directory_equal_to_the_repository_root_adds_no_rule() {
            let root = Path::new("/repo");
            let protected = ProtectedPaths::with_config_dir(root, Some(root));
            assert_eq!(protected.rules().len(), PROTECTED_PATTERNS.len());
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

        #[cfg(unix)]
        #[test]
        fn a_path_that_is_not_valid_utf8_is_still_matched() {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let protected = protected();
            let inside = Path::new(OsStr::from_bytes(b".nanna/agents/\xff.toml"));
            assert!(protected.is_protected(inside));
            let outside = Path::new(OsStr::from_bytes(b"docs/\xff.md"));
            assert!(!protected.is_protected(outside));
        }

        #[test]
        fn compatibility_forms_that_no_file_system_folds_stay_unprotected() {
            let protected = protected();
            for path in ["\u{fb02}ake.nix", "\u{ff46}lake.nix", "codecov\u{ff0e}yml"] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
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
                PathBuf::from(".gitattributes"),
                PathBuf::from(".gitmodules"),
                PathBuf::from("CODEOWNERS"),
                PathBuf::from("docs/CODEOWNERS"),
                PathBuf::from(".github"),
                PathBuf::from(".cargo-husky"),
                PathBuf::from(".envrc"),
                PathBuf::from(".config"),
                PathBuf::from(".claude"),
                PathBuf::from("nix"),
                PathBuf::from("CLAUDE.md"),
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
                (".gitattributes".to_string(), false),
                (".gitmodules".to_string(), false),
                ("CODEOWNERS".to_string(), false),
                ("docs/CODEOWNERS".to_string(), false),
                (".github".to_string(), true),
                (".cargo-husky".to_string(), true),
                (".envrc".to_string(), false),
                (".config".to_string(), true),
                (".claude".to_string(), true),
                ("nix".to_string(), true),
                ("CLAUDE.md".to_string(), false),
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
        fn ordinary_names_with_dots_spaces_or_colons_still_pass_the_guard() {
            let repo = repo_changing(&["src/main.rs.", "docs/a b.md", "notes:flake.nix"], "work");
            let output = enforce(&repo, "rust-implementer", "stranger", "");
            assert_eq!(output.status.code(), Some(0), "{output:?}");
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

    mod extras {
        use super::*;

        fn declared(globs: &[&str]) -> ProtectedExtras {
            let quoted: Vec<String> = globs.iter().map(|g| format!("{g:?}")).collect();
            ProtectedExtras::parse(&format!("extra = [{}]", quoted.join(", "))).unwrap()
        }

        fn with(globs: &[&str]) -> ProtectedPaths {
            ProtectedPaths::with_extras(Path::new("/repo"), None, &declared(globs))
        }

        fn write_declaration(base: &Path, body: &[u8]) {
            std::fs::create_dir_all(base.join(".nanna")).unwrap();
            std::fs::write(base.join(EXTRAS_FILE), body).unwrap();
        }

        #[test]
        fn a_declared_list_is_read_in_order() {
            let extras = declared(&["src/gate.rs", "scripts/**"]);
            assert_eq!(extras.globs(), ["src/gate.rs", "scripts/**"]);
        }

        #[test]
        fn an_empty_file_or_an_empty_list_declares_nothing() {
            assert!(ProtectedExtras::parse("").unwrap().globs().is_empty());
            assert!(ProtectedExtras::parse("extra = []")
                .unwrap()
                .globs()
                .is_empty());
        }

        #[test]
        fn an_absent_declaration_declares_nothing() {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(
                ProtectedExtras::load(dir.path()).unwrap(),
                ProtectedExtras::default()
            );
        }

        #[test]
        fn a_present_declaration_is_loaded_from_the_base_directory() {
            let dir = tempfile::tempdir().unwrap();
            write_declaration(dir.path(), b"extra = [\"src/gate.rs\"]\n");
            assert_eq!(
                ProtectedExtras::load(dir.path()).unwrap().globs(),
                ["src/gate.rs"]
            );
        }

        #[test]
        fn unknown_keys_are_rejected_at_every_level() {
            for text in [
                "extras = [\"a\"]",
                "extra = [\"a\"]\nremove = [\".git/**\"]",
                "[extra]\nsrc = \"a\"",
                "[other]\nextra = [\"a\"]",
            ] {
                let err = ProtectedExtras::parse(text).unwrap_err();
                assert!(matches!(err, ProtectedConfigError::Malformed(_)), "{text}");
            }
        }

        #[test]
        fn malformed_toml_and_wrong_types_are_rejected() {
            for text in [
                "extra = [",
                "extra = \"src/gate.rs\"",
                "extra = [1]",
                "extra = [[\"a\"]]",
                "= 3",
            ] {
                let err = ProtectedExtras::parse(text).unwrap_err();
                assert!(matches!(err, ProtectedConfigError::Malformed(_)), "{text}");
            }
        }

        #[test]
        fn a_malformed_file_on_disk_fails_to_load() {
            let dir = tempfile::tempdir().unwrap();
            write_declaration(dir.path(), b"extra = [");
            assert!(matches!(
                ProtectedExtras::load(dir.path()),
                Err(ProtectedConfigError::Malformed(_))
            ));
        }

        #[test]
        fn a_non_utf8_file_is_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            write_declaration(dir.path(), &[0xff, 0xfe, 0x00]);
            assert!(matches!(
                ProtectedExtras::load(dir.path()),
                Err(ProtectedConfigError::Unreadable(_))
            ));
        }

        #[test]
        fn a_directory_in_place_of_the_file_is_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join(EXTRAS_FILE)).unwrap();
            assert!(matches!(
                ProtectedExtras::load(dir.path()),
                Err(ProtectedConfigError::Unreadable(_))
            ));
        }

        #[cfg(unix)]
        #[test]
        fn a_symlink_in_place_of_the_file_is_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("elsewhere.toml");
            std::fs::write(&target, "extra = []").unwrap();
            std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
            std::os::unix::fs::symlink(&target, dir.path().join(EXTRAS_FILE)).unwrap();
            assert!(matches!(
                ProtectedExtras::load(dir.path()),
                Err(ProtectedConfigError::Unreadable(_))
            ));
        }

        #[test]
        fn a_base_that_is_not_a_directory_is_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("plain");
            std::fs::write(&file, "x").unwrap();
            assert!(matches!(
                ProtectedExtras::load(&file),
                Err(ProtectedConfigError::Unreadable(_))
            ));
        }

        #[test]
        fn an_absent_declaration_under_a_base_that_is_not_a_directory_is_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("plain");
            std::fs::write(&file, "x").unwrap();
            assert!(ProtectedExtras::absent(&file).is_err());
            assert!(ProtectedExtras::absent(dir.path()).is_ok());
        }

        fn padded_to(len: usize) -> String {
            let head = "extra = [\"src/gate.rs\"]\n";
            format!("{head}{}", "\n".repeat(len - head.len()))
        }

        fn with_entries(count: usize) -> String {
            let entries: Vec<String> = (0..count).map(|i| format!("\"p{i}\"")).collect();
            format!("extra = [{}]", entries.join(", "))
        }

        #[test]
        fn a_file_of_exactly_the_size_cap_loads_and_one_byte_more_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            write_declaration(dir.path(), padded_to(MAX_EXTRAS_BYTES).as_bytes());
            assert_eq!(
                ProtectedExtras::load(dir.path()).unwrap().globs(),
                ["src/gate.rs"]
            );
            write_declaration(dir.path(), padded_to(MAX_EXTRAS_BYTES + 1).as_bytes());
            assert_eq!(
                ProtectedExtras::load(dir.path()),
                Err(ProtectedConfigError::TooLarge)
            );
            assert!(ProtectedConfigError::TooLarge
                .to_string()
                .contains("65536 bytes"));
        }

        #[test]
        fn text_over_the_size_cap_is_refused_by_parse_too() {
            assert!(ProtectedExtras::parse(&padded_to(MAX_EXTRAS_BYTES)).is_ok());
            assert_eq!(
                ProtectedExtras::parse(&padded_to(MAX_EXTRAS_BYTES + 1)),
                Err(ProtectedConfigError::TooLarge)
            );
        }

        struct Endless(std::rc::Rc<std::cell::Cell<usize>>);

        impl std::io::Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf.fill(b' ');
                self.0.set(self.0.get() + buf.len());
                Ok(buf.len())
            }
        }

        struct Broken;

        impl std::io::Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("device failure"))
            }
        }

        #[test]
        fn a_reader_that_fails_midway_is_an_error_not_an_empty_declaration() {
            let err = read_bounded(Broken).unwrap_err();
            assert_eq!(err.to_string(), "device failure");
        }

        #[test]
        fn the_reader_stops_one_byte_past_the_cap_however_much_more_is_available() {
            let served = std::rc::Rc::new(std::cell::Cell::new(0));
            let text = read_bounded(Endless(served.clone())).unwrap();
            assert_eq!(text.len(), MAX_EXTRAS_BYTES + 1);
            assert_eq!(served.get(), MAX_EXTRAS_BYTES + 1);
        }

        #[test]
        fn a_file_far_over_the_size_cap_is_refused() {
            let dir = tempfile::tempdir().unwrap();
            write_declaration(dir.path(), &vec![b'\n'; MAX_EXTRAS_BYTES * 4]);
            assert_eq!(
                ProtectedExtras::load(dir.path()),
                Err(ProtectedConfigError::TooLarge)
            );
        }

        #[test]
        fn exactly_the_entry_cap_is_accepted_and_one_more_is_refused() {
            let at_cap = ProtectedExtras::parse(&with_entries(MAX_EXTRAS_ENTRIES)).unwrap();
            assert_eq!(at_cap.globs().len(), MAX_EXTRAS_ENTRIES);
            let err = ProtectedExtras::parse(&with_entries(MAX_EXTRAS_ENTRIES + 1)).unwrap_err();
            assert_eq!(
                err,
                ProtectedConfigError::TooManyEntries {
                    count: MAX_EXTRAS_ENTRIES + 1
                }
            );
            assert!(err.to_string().contains("257 patterns"));
            assert!(err.to_string().contains("256 allowed"));
        }

        #[test]
        fn a_file_in_place_of_the_config_directory_is_unreadable() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join(".nanna"), "x").unwrap();
            assert!(matches!(
                ProtectedExtras::load(dir.path()),
                Err(ProtectedConfigError::Unreadable(_))
            ));
        }

        #[test]
        fn patterns_that_cannot_be_mounted_or_matched_literally_are_rejected() {
            for pattern in [
                "",
                "/etc/passwd",
                "../outside",
                "a/../b",
                "./a",
                "a/./b",
                "a//b",
                "a\\b",
                "!a",
                "a/",
                "*",
                "**",
                "**/a",
                "src/*.rs",
                "src/?.rs",
                "src/[ab].rs",
                "src/{a,b}.rs",
                "a/**/b",
                "/**",
                "src/**/",
                "a\nb",
            ] {
                let text = format!("extra = [{pattern:?}]");
                let err = ProtectedExtras::parse(&text).unwrap_err();
                assert!(
                    matches!(&err, ProtectedConfigError::InvalidPattern { pattern: p, .. } if p == pattern),
                    "{pattern:?}: {err:?}"
                );
                assert!(err.to_string().contains(".nanna/protected.toml"));
            }
        }

        #[test]
        fn literal_files_and_recursive_directories_are_accepted() {
            let extras = declared(&["a", "a/b.rs", "dir/**", "dir/sub/**", "weird name.txt"]);
            assert_eq!(extras.globs().len(), 5);
        }

        #[test]
        fn declared_patterns_protect_files_and_directories_case_insensitively() {
            let protected = with(&["src/gate.rs", "tools/**"]);
            let violation = protected.check(Path::new("src/gate.rs")).unwrap_err();
            assert_eq!(violation.rule, "src/gate.rs");
            assert!(protected.is_protected(Path::new("SRC/Gate.rs")));
            assert!(protected.is_protected(Path::new("tools")));
            assert!(protected.is_protected(Path::new("tools/deep/file.sh")));
            assert!(!protected.is_protected(Path::new("src/other.rs")));
            assert!(!protected.is_protected(Path::new("toolsmith/file.sh")));
        }

        #[test]
        fn without_a_declaration_the_same_paths_are_unprotected() {
            let bare = ProtectedPaths::with_config_dir(Path::new("/repo"), None);
            assert!(!bare.is_protected(Path::new("src/gate.rs")));
            assert!(!bare.is_protected(Path::new("tools/deep/file.sh")));
        }

        #[test]
        fn declarations_only_add_never_remove_a_shipped_rule() {
            let config = Path::new("/repo/home/cfg");
            let hostile = declared(&["src/gate.rs", ".nanna/protected.toml", "home/cfg/x"]);
            let protected = ProtectedPaths::with_extras(Path::new("/repo"), Some(config), &hostile);
            for (rule, path) in EXAMPLES {
                let violation = protected.check(Path::new(path)).unwrap_err();
                assert_eq!(violation.rule, *rule, "{path}");
            }
            assert!(protected.is_protected(Path::new("home/cfg/agents/x.toml")));
            let rules = protected.rules();
            for pattern in PROTECTED_PATTERNS {
                assert!(rules.contains(pattern), "{pattern}");
            }
            assert!(rules.contains(&"home/cfg/**"));
            assert_eq!(&rules[..PROTECTED_PATTERNS.len()], PROTECTED_PATTERNS);
        }

        #[test]
        fn declared_rules_follow_the_shipped_and_config_dir_rules() {
            let extras = declared(&["src/gate.rs"]);
            let protected = ProtectedPaths::with_extras(
                Path::new("/repo"),
                Some(Path::new("/repo/cfg")),
                &extras,
            );
            let rules = protected.rules();
            assert_eq!(rules.len(), PROTECTED_PATTERNS.len() + 2);
            assert_eq!(rules[PROTECTED_PATTERNS.len()], "cfg/**");
            assert_eq!(rules[PROTECTED_PATTERNS.len() + 1], "src/gate.rs");
        }

        #[test]
        fn declared_patterns_become_mount_roots() {
            let protected = with(&["src/gate.rs", "tools/**", "tools/inner/**"]);
            let targets = protected.mount_targets();
            let find = |path: &str| targets.iter().find(|t| t.path == Path::new(path));
            assert!(!find("src/gate.rs").unwrap().directory);
            assert!(find("tools").unwrap().directory);
            assert!(find("tools/inner").is_none());
        }

        #[test]
        fn for_repo_with_extras_keeps_the_catalog_config_dir() {
            let extras = declared(&["src/gate.rs"]);
            let protected = ProtectedPaths::for_repo_with_extras(Path::new("/repo"), &extras);
            let expected = IdentityCatalog::default_global_dir()
                .and_then(|d| d.parent().map(Path::to_path_buf));
            assert_eq!(protected.config_dir(), expected.as_deref());
            assert!(protected.is_protected(Path::new("src/gate.rs")));
            assert!(protected.is_protected(Path::new(".nanna/agents/x.toml")));
        }

        fn alias_declaration() -> ProtectedPaths {
            with(&["tools/release-helper.sh", "gate/**", "src/lib.rs"])
        }

        fn rule_of(protected: &ProtectedPaths, path: &str) -> Option<String> {
            protected.check(Path::new(path)).err().map(|v| v.rule)
        }

        #[test]
        fn trailing_dots_spaces_and_streams_fold_onto_a_declared_path() {
            let protected = alias_declaration();
            for (path, rule) in [
                ("tools/release-helper.sh.", "tools/release-helper.sh"),
                ("tools/release-helper.sh ", "tools/release-helper.sh"),
                ("TOOLS./Release-Helper.SH. .", "tools/release-helper.sh"),
                ("src/lib.rs::$DATA", "src/lib.rs"),
                ("src/lib.rs:stream", "src/lib.rs"),
                ("gate./x.rs", "gate/**"),
                ("gate /sub/x.rs", "gate/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
        }

        #[test]
        fn short_names_of_declared_paths_are_refused() {
            let protected = alias_declaration();
            for (path, rule) in [
                ("tools/RELEAS~1.SH", "tools/release-helper.sh"),
                ("tools/RE3F9A~1.SH", "tools/release-helper.sh"),
                ("TOOLS/relea~12.sh", "tools/release-helper.sh"),
                ("gate~1/x.rs", "gate/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
        }

        #[test]
        fn short_names_that_cannot_alias_a_declared_path_are_allowed() {
            let protected = alias_declaration();
            for path in [
                "tools/OTHER~1.SH",
                "tools/RELEAS~1.TXT",
                "src/RELEAS~1.SH",
                "tools/release-helper.sh.bak",
                "tools/release-helper.shx",
            ] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn without_a_declaration_the_same_aliases_are_unprotected() {
            let protected = with(&[]);
            for path in [
                "tools/release-helper.sh.",
                "tools/RELEAS~1.SH",
                "src/lib.rs::$DATA",
                "gate~1/x.rs",
            ] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn a_declared_alias_violation_reports_the_written_spelling_and_the_declared_glob() {
            let protected = alias_declaration();
            let violation = protected.check(Path::new("tools/RELEAS~1.SH")).unwrap_err();
            assert_eq!(violation.path, "tools/RELEAS~1.SH");
            assert_eq!(violation.rule, "tools/release-helper.sh");
        }

        #[test]
        fn declared_paths_that_end_in_a_dot_or_space_or_hold_a_colon_are_rejected() {
            for pattern in [
                "docs/notes.",
                "docs/notes ",
                "docs./x",
                "docs /x",
                "docs/**.",
                "docs/notes.txt:stream",
                "a:b/c",
            ] {
                let text = format!("extra = [\"{pattern}\"]");
                assert!(
                    matches!(
                        ProtectedExtras::parse(&text),
                        Err(ProtectedConfigError::InvalidPattern { .. })
                    ),
                    "{pattern}"
                );
            }
            assert!(ProtectedExtras::parse("extra = [\"docs/notes.txt\", \"dir.d/**\"]").is_ok());
        }

        #[test]
        fn a_declared_name_with_characters_windows_replaces_in_short_names_is_matched() {
            let protected = with_raw(&["tools/a+b+c+d+e.txt", "tools/x;y=z.sh"]);
            for (path, rule) in [
                ("tools/A_B_C_~1.TXT", "tools/a+b+c+d+e.txt"),
                ("tools/X_Y_Z~1.SH", "tools/x;y=z.sh"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
            assert!(!protected.is_protected(Path::new("tools/A_B_X_~1.TXT")));
        }

        #[test]
        fn too_many_short_name_spellings_of_a_declared_path_fail_closed() {
            let protected = with(&["tools/release-helper.sh"]);
            let many = ["RELEAS~1.SH"; 11].join("/");
            let violation = protected.check(Path::new(&many)).unwrap_err();
            assert_eq!(violation.rule, ALIAS_OVERFLOW_RULE);
        }

        fn with_raw(globs: &[&str]) -> ProtectedPaths {
            let quoted: Vec<String> = globs.iter().map(|g| format!("\"{g}\"")).collect();
            let extras =
                ProtectedExtras::parse(&format!("extra = [{}]", quoted.join(", "))).unwrap();
            ProtectedPaths::with_extras(Path::new("/repo"), None, &extras)
        }

        const NFD_FILE: &str = "src/cafe\u{301}.rs";
        const NFC_FILE: &str = "src/caf\u{e9}.rs";
        const NFD_DIR: &str = "docs/cafe\u{301}";
        const NFC_DIR: &str = "docs/caf\u{e9}";

        #[test]
        fn a_declared_decomposed_file_matches_its_precomposed_spelling() {
            let protected = with_raw(&[NFD_FILE]);
            let violation = protected.check(Path::new(NFC_FILE)).unwrap_err();
            assert_eq!(violation.rule, NFD_FILE);
            assert_eq!(violation.path, NFC_FILE);
            assert!(protected.is_protected(Path::new(NFD_FILE)));
        }

        #[test]
        fn a_declared_precomposed_file_matches_its_decomposed_spelling() {
            let protected = with_raw(&[NFC_FILE]);
            let violation = protected.check(Path::new(NFD_FILE)).unwrap_err();
            assert_eq!(violation.rule, NFC_FILE);
            assert_eq!(violation.path, NFD_FILE);
        }

        #[test]
        fn a_declared_directory_matches_either_spelling_of_itself_and_its_contents() {
            for (declared, candidates) in [
                (
                    NFD_DIR,
                    [NFC_DIR, "docs/caf\u{e9}/x/y.md", "DOCS/caf\u{e9}/x"],
                ),
                (
                    NFC_DIR,
                    [NFD_DIR, "docs/cafe\u{301}/x/y.md", "DOCS/cafe\u{301}/x"],
                ),
            ] {
                let protected = with_raw(&[&format!("{declared}/**")]);
                for candidate in candidates {
                    assert!(
                        protected.is_protected(Path::new(candidate)),
                        "{declared} {candidate}"
                    );
                }
            }
        }

        #[test]
        fn a_declared_ascii_name_matches_its_compatibility_singleton() {
            let protected = with_raw(&["src/key.rs", "ci/**"]);
            assert!(protected.is_protected(Path::new("src/\u{212A}ey.rs")));
            assert!(protected.is_protected(Path::new("ci/\u{212A}/x")));
        }

        #[cfg(unix)]
        #[test]
        fn a_declared_directory_still_matches_a_path_that_is_not_valid_utf8() {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let protected = with_raw(&["ci/**"]);
            assert!(protected.is_protected(Path::new(OsStr::from_bytes(b"ci/\xff.sh"))));
            assert!(!protected.is_protected(Path::new(OsStr::from_bytes(b"cj/\xff.sh"))));
        }

        #[test]
        fn declared_rules_and_mount_roots_keep_the_declared_spelling() {
            let protected = with_raw(&[NFD_FILE, &format!("{NFC_DIR}/**")]);
            assert!(protected.rules().contains(&NFD_FILE));
            assert!(protected
                .rules()
                .contains(&format!("{NFC_DIR}/**").as_str()));
            let roots = protected.mount_roots();
            assert!(roots.contains(&PathBuf::from(NFD_FILE)));
            assert!(roots.contains(&PathBuf::from(NFC_DIR)));
        }

        #[test]
        fn unrelated_non_ascii_names_stay_unprotected_under_declared_patterns() {
            let protected = with_raw(&[NFD_FILE, &format!("{NFC_DIR}/**")]);
            for path in [
                "src/caf\u{e8}.rs",
                "docs/cafe\u{301}x/y",
                "other/cafe\u{301}/y",
            ] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        const COMBINED: [&str; 3] = [
            "docs/caf\u{e9}/**",
            "src/cafe\u{301}.rs",
            "docs/r\u{e9}sum\u{e9}-final.txt",
        ];

        #[test]
        fn nfd_spellings_with_a_trailing_dot_stream_or_space_match_declared_paths() {
            let protected = with_raw(&COMBINED);
            for (path, rule) in [
                ("docs/cafe\u{301}./x.md", COMBINED[0]),
                ("docs/cafe\u{301} /x.md", COMBINED[0]),
                ("src/caf\u{e9}.rs.", COMBINED[1]),
                ("src/caf\u{e9}.rs ", COMBINED[1]),
                ("src/caf\u{e9}.rs::$DATA", COMBINED[1]),
                ("SRC/cafe\u{301}.RS.", COMBINED[1]),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
        }

        #[test]
        fn nfd_short_names_match_declared_non_ascii_paths() {
            for declared in [
                "docs/r\u{e9}sum\u{e9}-final.txt",
                "docs/re\u{301}sume\u{301}-final.txt",
            ] {
                let protected = with_raw(&[declared]);
                for path in [
                    "docs/re\u{301}sume\u{301}~1.txt",
                    "docs/r\u{e9}sum\u{e9}~1.txt",
                    "docs/re\u{301}sume\u{301}~1.txt.",
                ] {
                    assert_eq!(
                        rule_of(&protected, path).as_deref(),
                        Some(declared),
                        "{path}"
                    );
                }
            }
        }

        #[test]
        fn combined_spellings_that_resemble_but_differ_stay_unprotected() {
            let protected = with_raw(&COMBINED);
            for path in [
                "docs/cafe\u{301}x./y.md",
                "src/caf\u{e8}.rs.",
                "docs/re\u{301}sume\u{301}~1.md",
                "other/re\u{301}sume\u{301}~1.txt",
            ] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn windows_aliases_of_shipped_paths_combine_with_unicode_folds() {
            let protected = protected();
            for (path, rule) in [
                ("fla\u{212A}e.nix.", "flake.nix"),
                ("fla\u{212A}e.nix ", "flake.nix"),
                ("FLA\u{212A}EN~1.NIX", "flake.nix"),
                ("cafe\u{301}/.nanna ./agents/x.toml", "**/.nanna/**"),
                (".git./cafe\u{301}/config", ".git/**"),
                (".github./workflows/ci.yml::$DATA", ".github/workflows/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
        }

        #[test]
        fn a_configuration_directory_combines_both_folds() {
            let root = Path::new("/repo");
            let config = root.join("cfg/cafe\u{301}");
            let protected = ProtectedPaths::with_config_dir(root, Some(&config));
            for path in [
                "cfg/caf\u{e9}./agents/x.toml",
                "cfg/CAF\u{e9}/x",
                "cfg/cafe\u{301} /x",
            ] {
                assert!(protected.is_protected(Path::new(path)), "{path}");
            }
        }
    }
}
