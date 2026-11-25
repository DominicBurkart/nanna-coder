//! The paths a task run by Nanna may not modify: Nanna's own configuration.
//!
//! [`ProtectedPaths`] is a fixed set of repository-relative globs (Nanna's
//! configuration under `.nanna/` and the repository's own `.git` directory)
//! plus Nanna's own configuration directory when it happens to lie inside
//! the repository. It sits above
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
use crate::policy::{RepoPolicy, MAX_POLICY_BYTES};
use glob::{MatchOptions, Pattern};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

/// The fixed, repository-relative globs every task run by Nanna is refused,
/// matched with `*` never crossing `/` and `**` spanning directories. A glob
/// ending in `/**` also matches the directory itself.
///
/// ```
/// use harness::protected::ProtectedPaths;
/// use std::path::Path;
///
/// let protected = ProtectedPaths::with_config_dir(Path::new("."), None);
/// assert!(protected.is_protected(Path::new(".nanna/agents/reviewer.toml")));
/// assert!(protected.is_protected(Path::new(".git/config")));
/// assert!(!protected.is_protected(Path::new(".github/workflows/ci.yml")));
/// assert!(!protected.is_protected(Path::new("AGENTS.md")));
/// ```
pub const PROTECTED_PATTERNS: &[&str] = &[".nanna/**", "**/.nanna/**", ".git/**"];

const MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: false,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

const RECURSIVE_SUFFIX: &str = "/**";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtectedConfigError {
    #[error(transparent)]
    Policy(#[from] crate::policy::PolicyError),
}

fn read_bounded(reader: impl std::io::Read) -> std::io::Result<String> {
    let mut text = String::new();
    let mut bounded = std::io::Read::take(reader, MAX_POLICY_BYTES as u64 + 1);
    std::io::Read::read_to_string(&mut bounded, &mut text)?;
    Ok(text)
}

fn absent(base: &Path) -> Result<(), &'static str> {
    if !base.is_dir() {
        return Err("the base checkout is not a directory");
    }
    match std::fs::symlink_metadata(base.join(".nanna")) {
        Ok(meta) if !meta.is_dir() => Err("`.nanna` is not a directory"),
        _ => Ok(()),
    }
}

pub(crate) fn declaration_text(base: &Path, file: &str) -> Result<Option<String>, String> {
    let path = base.join(file);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return absent(base).map(|_| None).map_err(String::from);
        }
        Err(e) => return Err(e.to_string()),
        Ok(meta) if !meta.is_file() => return Err("not a regular file".to_string()),
        Ok(_) => {}
    }
    std::fs::File::open(&path)
        .and_then(read_bounded)
        .map(Some)
        .map_err(|e| e.to_string())
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

/// The rule reported for a write refused because the target has other hard
/// links.
pub const HARDLINK_RULE: &str = "hard link (link count above one)";

/// Whether `metadata` describes a regular file or symlink that other names
/// also reach. Directories and other file types are never reported: a
/// directory's link count counts its subdirectories.
#[cfg(unix)]
pub fn is_hardlinked(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    let file_type = metadata.file_type();
    judged_link_count(
        file_type.is_file() || file_type.is_symlink(),
        metadata.nlink(),
    )
}

#[cfg(unix)]
fn judged_link_count(regular_or_symlink: bool, links: u64) -> bool {
    regular_or_symlink && links > 1
}

/// Whether `metadata` describes a file that other names also reach. Link
/// counts are not available on this platform, so no file is reported.
#[cfg(not(unix))]
pub fn is_hardlinked(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// The violation for a write to `path`, which has other hard links.
pub fn hardlink_violation(path: &Path) -> ProtectedPathViolation {
    ProtectedPathViolation {
        path: crate::scope::denial_path(path),
        rule: HARDLINK_RULE.to_string(),
    }
}

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

/// The set of paths a task run by Nanna may not write, regardless of identity.
///
/// ```
/// use harness::protected::ProtectedPaths;
/// use std::path::Path;
///
/// let protected = ProtectedPaths::for_repo(Path::new("."));
/// assert!(protected.is_protected(Path::new(".nanna/agents/rust-implementer.toml")));
/// assert!(protected.is_protected(Path::new(".git/config")));
/// assert!(!protected.is_protected(Path::new(".github/workflows/ci.yml")));
/// assert!(!protected.is_protected(Path::new("src/main.rs")));
///
/// let violation = protected.check(Path::new(".nanna/deploy.toml")).unwrap_err();
/// assert_eq!(violation.rule, ".nanna/**");
/// ```
#[derive(Debug, Clone)]
pub struct ProtectedPaths {
    rules: Vec<Rule>,
    config_dir: Option<PathBuf>,
    policy: RepoPolicy,
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
        let mut literals: Vec<String> = literals.iter().map(|literal| nfc(literal)).collect();
        literals.sort_by_key(|literal| literal.to_lowercase());
        literals.dedup_by_key(|literal| literal.to_lowercase());
        Self {
            rules,
            config_dir: config_dir.map(Path::to_path_buf),
            policy: RepoPolicy::default(),
            literals,
        }
    }

    pub fn with_policy(mut self, policy: RepoPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> RepoPolicy {
        self.policy
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
    ];

    #[test]
    fn the_shipped_set_is_nannas_configuration_and_git_internals_only() {
        assert_eq!(PROTECTED_PATTERNS, [".nanna/**", "**/.nanna/**", ".git/**"]);
    }

    #[test]
    fn repository_ci_docs_build_files_and_source_are_writable() {
        let protected = protected();
        for path in [
            "AGENTS.md",
            "CLAUDE.md",
            ".github/workflows/x.yml",
            ".github/CODEOWNERS",
            "codecov.yml",
            "flake.nix",
            "flake.lock",
            "deny.toml",
            "nix/containers.nix",
            ".claude/settings.json",
            "inchworm.toml",
            "harness/src/tools.rs",
            "harness/src/protected.rs",
            "scripts/install.sh",
        ] {
            assert_eq!(protected.check(Path::new(path)), Ok(()), "{path}");
        }
    }

    #[test]
    fn agents_md_never_restricts_developers() {
        let agents = include_str!("../../AGENTS.md");
        assert!(agents.contains("does not restrict developers or coding assistants"));
        for forbidden in [
            "Expect protected files to need a human",
            "human-authored PR",
            "agents cannot write either file",
        ] {
            assert!(!agents.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn a_reader_that_fails_midway_is_an_error_not_an_empty_file() {
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("device failure"))
            }
        }
        let err = read_bounded(Broken).unwrap_err();
        assert_eq!(err.to_string(), "device failure");
    }

    #[test]
    fn the_reader_stops_one_byte_past_the_cap_however_much_more_is_available() {
        struct Endless(std::rc::Rc<std::cell::Cell<usize>>);
        impl std::io::Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                buf.fill(b' ');
                self.0.set(self.0.get() + buf.len());
                Ok(buf.len())
            }
        }
        let served = std::rc::Rc::new(std::cell::Cell::new(0));
        let text = read_bounded(Endless(served.clone())).unwrap();
        assert_eq!(text.len(), MAX_POLICY_BYTES + 1);
        assert_eq!(served.get(), MAX_POLICY_BYTES + 1);
    }

    #[test]
    fn an_absent_file_under_a_base_that_is_not_a_directory_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain");
        std::fs::write(&file, "x").unwrap();
        assert!(absent(&file).is_err());
        assert!(absent(dir.path()).is_ok());
    }

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
            ".GIT/config",
            ".Git/Hooks/pre-commit",
            ".NANNA/deploy.toml",
            ".Nanna/Policy.toml",
            "crates/api/.Nanna/agents/x.toml",
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
                (".nanna.", ".nanna/**"),
                (".nanna ", ".nanna/**"),
                (".nanna. .", ".nanna/**"),
                (".NANNA...", ".nanna/**"),
                (".git  ", ".git/**"),
                (".git./hooks/pre-commit", ".git/**"),
                (".nanna./agents/x.toml", ".nanna/**"),
                (".nanna /agents/x.toml", ".nanna/**"),
                ("crates/api/.nanna./x.toml", "**/.nanna/**"),
                (".git./config", ".git/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
        }

        #[test]
        fn a_folded_violation_reports_the_spelling_that_was_written() {
            let violation = protected().check(Path::new(".nanna. ")).unwrap_err();
            assert_eq!(violation.path, ".nanna. ");
            assert_eq!(violation.rule, ".nanna/**");
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
            for path in [".nanna.bak", ".nannax.", "x.nanna.", ".git.bak "] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn a_stream_suffix_addresses_the_protected_file() {
            let protected = protected();
            for (path, rule) in [
                (".nanna:stream", ".nanna/**"),
                (".nanna::$DATA", ".nanna/**"),
                (".git.:x", ".git/**"),
                (".nanna/agents/x.toml:x", ".nanna/**"),
                (".nanna:x/agents/x.toml", ".nanna/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
            for path in ["notes:.git", ":.git", "src/a:b.rs"] {
                assert!(!protected.is_protected(Path::new(path)), "{path}");
            }
        }

        #[test]
        fn short_names_of_protected_files_and_directories_are_refused() {
            let protected = protected();
            for (path, rule) in [
                ("NANNA~1/agents/x.toml", ".nanna/**"),
                ("crates/NANNA~1/x.toml", "**/.nanna/**"),
                ("GIT~1/config", ".git/**"),
            ] {
                assert_eq!(rule_of(&protected, path).as_deref(), Some(rule), "{path}");
            }
            for (config, path) in [
                ("codecov.yml", "CODECO~1.YML"),
                ("codecov.yml", "codeco~1.yml"),
                ("tarpaulin.toml", "TARPAU~1.TOM"),
                ("flake.lock", "FLAKEL~1.LOC"),
                ("flake.nix", "FLAKEN~1.NIX"),
                ("flake.nix", "FLAKE~12.NIX"),
                ("flake.nix", "FL1A2B~1.NIX"),
                ("codecov.yml", "CO3F9A~2.YML"),
                (".devcontainer", "DEVCON~1/devcontainer.json"),
                ("docs/CODEOWNERS", "docs/CODEOW~1"),
            ] {
                let protected = with_config_dir_at(config);
                let rule = format!("{config}/**");
                assert_eq!(rule_of(&protected, path), Some(rule), "{path}");
            }
        }

        #[test]
        fn short_names_that_cannot_alias_a_protected_name_are_allowed() {
            let protected = with_config_dir_at("flake.nix");
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
            let many = format!("src/{}/x.rs", ["GIT~1"; 11].join("/"));
            let violation = protected.check(Path::new(&many)).unwrap_err();
            assert_eq!(violation.rule, ALIAS_OVERFLOW_RULE);
            assert_eq!(violation.path, many);
            let few = "src/GIT~1/GIT~1/x.rs";
            assert!(!protected.is_protected(Path::new(few)));
        }

        #[test]
        fn the_variant_cap_is_exactly_1024_spellings() {
            let protected = with_config_dir_at("deep/codecov.yml");
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
            let deep = format!("{}/.nanna.", ["a"; 3_000].join("/"));
            let variants = protected.variants(Path::new(&deep)).unwrap();
            assert_eq!(variants.len(), 2);
            assert!(variants[1].ends_with(".nanna"));
        }

        #[test]
        fn a_deep_path_with_an_aliasable_component_fails_closed_past_the_component_cap() {
            let protected = protected();
            let within = format!("{}/NANNA~1", ["a"; 1_000].join("/"));
            assert!(protected.variants(Path::new(&within)).is_some());
            let beyond = format!("{}/NANNA~1", ["a"; 1_100].join("/"));
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
            "harness/tests/fixtures/sample_app/catalog/auditor.toml.bak",
            "other/harness/tests/fixtures/sample_app/catalog/auditor.toml",
            "harness/tests/fixtures/sample_app/catalog/prompts/auditor.md.bak",
            "other/harness/tests/fixtures/sample_app/catalog/prompts/auditor.md",
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
            for (config, path, rule) in [
                ("flake.nix", "fla\u{212A}e.nix", "flake.nix/**"),
                ("flake.lock", "flake.loc\u{212A}", "flake.lock/**"),
                (".cargo-husky", ".cargo-hus\u{212A}y/x", ".cargo-husky/**"),
                ("cfg", "crates/.nanna/\u{212B}", "**/.nanna/**"),
            ] {
                let protected = with_config(config);
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
    fn windows_aliases_of_shipped_paths_combine_with_unicode_folds() {
        let protected = protected();
        for (path, rule) in [
            ("cafe\u{301}/.nanna ./agents/x.toml", "**/.nanna/**"),
            ("CAF\u{c9}/NANNA~1/x::$DATA", "**/.nanna/**"),
            (".git./cafe\u{301}/config", ".git/**"),
            (".nanna./cafe\u{301}.toml::$DATA", ".nanna/**"),
        ] {
            let found = protected.check(Path::new(path)).err().map(|v| v.rule);
            assert_eq!(found.as_deref(), Some(rule), "{path}");
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

    #[test]
    #[serial_test::serial(nanna_config_dir_env)]
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
        let protected = ProtectedPaths::with_config_dir(
            Path::new("/repo"),
            Some(Path::new("/repo/.git/inner")),
        );
        let roots = protected.mount_roots();
        assert!(roots.contains(&PathBuf::from(".git")));
        assert!(!roots.contains(&PathBuf::from(".git/inner")));
        assert!(protected.is_protected(Path::new(".git/inner/x")));
    }

    #[test]
    fn a_later_ancestor_replaces_the_nested_roots_mounted_before_it() {
        let mut nested = protected();
        nested.rules = ["top/a/**", "top/b", "top/**"]
            .iter()
            .map(|glob| Rule::new(glob.to_string()))
            .collect();
        assert_eq!(nested.mount_roots(), vec![PathBuf::from("top")]);
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
                PathBuf::from("cfg"),
            ]
        );
    }

    #[test]
    fn mount_targets_say_which_roots_are_directories() {
        let mut protected = protected();
        protected.rules.push(Rule::new("single.toml".to_string()));
        let kinds: Vec<(String, bool)> = protected
            .mount_targets()
            .iter()
            .map(|t| (t.path.display().to_string(), t.directory))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (".nanna".to_string(), true),
                (".git".to_string(), true),
                ("single.toml".to_string(), false),
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
            vec![PathBuf::from(".nanna")]
        );
    }

    #[cfg(unix)]
    mod hardlinks {
        use super::*;

        fn link_count_fixture(links: usize) -> (tempfile::TempDir, std::fs::Metadata) {
            let dir = tempfile::tempdir().unwrap();
            let original = dir.path().join("original");
            std::fs::write(&original, "x").unwrap();
            for index in 1..links {
                std::fs::hard_link(&original, dir.path().join(format!("link{index}"))).unwrap();
            }
            let metadata = std::fs::symlink_metadata(&original).unwrap();
            (dir, metadata)
        }

        #[test]
        fn a_single_link_is_not_hardlinked() {
            let (_dir, metadata) = link_count_fixture(1);
            assert!(!is_hardlinked(&metadata));
        }

        #[test]
        fn exactly_two_links_is_hardlinked() {
            let (_dir, metadata) = link_count_fixture(2);
            assert!(is_hardlinked(&metadata));
        }

        #[test]
        fn many_links_is_hardlinked() {
            let (_dir, metadata) = link_count_fixture(5);
            assert!(is_hardlinked(&metadata));
        }

        #[test]
        fn only_regular_files_and_symlinks_are_judged_by_their_link_count() {
            for (judged, links, expected) in [
                (true, 1, false),
                (true, 2, true),
                (true, 9, true),
                (false, 1, false),
                (false, 2, false),
                (false, 9, false),
            ] {
                assert_eq!(
                    judged_link_count(judged, links),
                    expected,
                    "{judged} {links}"
                );
            }
        }

        #[test]
        fn a_directory_with_subdirectories_is_not_hardlinked() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
            for path in [dir.path().to_path_buf(), dir.path().join("a")] {
                let metadata = std::fs::symlink_metadata(path).unwrap();
                assert!(!is_hardlinked(&metadata));
            }
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn a_symlink_with_other_names_is_hardlinked() {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("link");
            std::os::unix::fs::symlink("target", &link).unwrap();
            assert!(!is_hardlinked(&std::fs::symlink_metadata(&link).unwrap()));
            std::fs::hard_link(&link, dir.path().join("second")).unwrap();
            assert!(is_hardlinked(&std::fs::symlink_metadata(&link).unwrap()));
        }

        #[test]
        fn the_hardlink_violation_names_the_path_and_the_fixed_rule() {
            let violation = hardlink_violation(Path::new("docs/alias.md"));
            assert_eq!(violation.path, "docs/alias.md");
            assert_eq!(violation.rule, HARDLINK_RULE);
            assert!(violation.to_string().contains("docs/alias.md"));
        }
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

    mod policy_carriage {
        use super::*;
        use crate::policy::RepoPolicy;

        #[test]
        fn a_protected_set_defaults_to_github_and_carries_the_policy_it_is_given() {
            let plain = ProtectedPaths::with_config_dir(Path::new("/repo"), None);
            assert!(!plain.policy().is_solo());
            let solo = plain.clone().with_policy(RepoPolicy::solo());
            assert!(solo.policy().is_solo());
            assert_eq!(solo.rules(), plain.rules());
            assert!(!solo.with_policy(RepoPolicy::default()).policy().is_solo());
        }

        const SHIPPED_SPELLINGS: [&str; 9] = [
            ".nanna./F",
            ".nanna /F",
            ".nanna/F.",
            ".nanna/F ",
            ".nanna/F::$DATA",
            "NANNA~1/F",
            ".NANNA/F",
            "NANNA~1/F:stream",
            "NANNA~1./F",
        ];

        const NESTED_SPELLINGS: [&str; 3] = [
            "crates/e\u{301}/.nanna/F",
            "crates/e\u{301}/NANNA~1/F",
            "crates/\u{e9}/.nanna./F",
        ];

        fn spelled(template: &str, file: &str) -> String {
            template.replace('F', file)
        }

        #[test]
        fn every_alias_and_normalisation_spelling_of_the_declaration_files_is_protected() {
            let protected = ProtectedPaths::with_config_dir(Path::new("/repo"), None);
            for file in [
                "policy.toml",
                "protected.toml",
                "POLICY~1.TOML",
                "PROTEC~1.TOM",
            ] {
                for template in SHIPPED_SPELLINGS {
                    let path = spelled(template, file);
                    let rule = protected.check(Path::new(&path)).unwrap_err().rule;
                    assert_eq!(rule, ".nanna/**", "{path}");
                }
                for template in NESTED_SPELLINGS {
                    let path = spelled(template, file);
                    let rule = protected.check(Path::new(&path)).unwrap_err().rule;
                    assert_eq!(rule, "**/.nanna/**", "{path}");
                }
            }
        }

        #[test]
        fn the_solo_policy_changes_no_decision_for_any_spelling() {
            let plain = ProtectedPaths::with_config_dir(Path::new("/repo"), None);
            let solo = plain.clone().with_policy(RepoPolicy::solo());
            assert!(solo.policy().is_solo());
            assert_eq!(solo.rules(), plain.rules());
            let mut counted = 0;
            for template in SHIPPED_SPELLINGS.iter().chain(NESTED_SPELLINGS.iter()) {
                let path = spelled(template, "policy.toml");
                assert_eq!(
                    solo.check(Path::new(&path)),
                    plain.check(Path::new(&path)),
                    "{path}"
                );
                counted += 1;
            }
            assert_eq!(counted, 12);
            let deep = format!("{}/NANNA~1/policy.toml", ["a"; 1_100].join("/"));
            let overflow = solo.check(Path::new(&deep)).unwrap_err();
            assert_eq!(overflow.rule, ALIAS_OVERFLOW_RULE);
            assert_eq!(plain.check(Path::new(&deep)), Err(overflow));
        }

        #[test]
        fn the_policy_file_is_protected_like_the_protected_declaration() {
            let protected = ProtectedPaths::with_config_dir(Path::new("/repo"), None);
            for path in [
                ".nanna/policy.toml",
                ".nanna/protected.toml",
                "crates/a/.nanna/policy.toml",
            ] {
                let violation = protected.check(Path::new(path)).unwrap_err();
                assert!(
                    violation.rule.ends_with(".nanna/**"),
                    "{path}: {violation:?}"
                );
            }
        }
    }
}
