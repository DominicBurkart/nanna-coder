//! Runtime scope enforcement for agents running under an identity.
//!
//! [`PathScope`] narrows which paths inside a task worktree the file tools
//! may read and write, and [`ScopeDenial`] is the structured record of every
//! call a scope refuses, so the task result and the auditor can see it.
//!
//! Escape protection (`..`, absolute paths elsewhere, symlinks resolving
//! outside the worktree) is shared with the unscoped tools through
//! [`resolve_path`]: a scope only ever narrows what an unscoped tool would
//! already allow. [`resolve_path_guarded`] additionally refuses writes to
//! [`ProtectedPaths`], which no scope can widen.

use crate::identity::AgentIdentity;
use crate::protected::{ProtectedPathViolation, ProtectedPaths};
use crate::tools::{ToolError, ToolResult};
use glob::{MatchOptions, Pattern};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

/// The identity recorded on a denial raised outside any identity scope.
pub const UNSCOPED_IDENTITY: &str = "unscoped";

/// Which kind of file access a path-scoped tool is attempting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathAccess {
    /// Reading a file or listing a directory.
    Read,
    /// Creating or overwriting a file.
    Write,
}

/// Why a scope refused a call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DenialReason {
    /// The tool is not in `scope.tools` or exceeds `scope.max_effect`, so it
    /// was never registered for this identity.
    ToolNotInScope,
    /// The path is inside the worktree but outside the identity's globs.
    PathOutsideScope {
        /// The access that was attempted.
        access: PathAccess,
        /// The worktree-relative path that was refused.
        path: String,
    },
    /// The path is Nanna's own configuration, which no identity may write.
    ProtectedPath {
        /// The worktree-relative path that was refused.
        path: String,
        /// The protected glob that matched.
        rule: String,
    },
}

impl fmt::Display for DenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenialReason::ToolNotInScope => f.write_str("tool is not in scope"),
            DenialReason::PathOutsideScope { access, path } => match access {
                PathAccess::Read => write!(f, "read of `{path}` is outside scope.read_paths"),
                PathAccess::Write => write!(f, "write to `{path}` is outside scope.paths"),
            },
            DenialReason::ProtectedPath { path, rule } => {
                write!(f, "write to `{path}` is refused by protected rule `{rule}`")
            }
        }
    }
}

/// A refused call, attributed to the identity that made it.
///
/// ```
/// use harness::scope::{DenialReason, PathAccess, ScopeDenial};
///
/// let denial = ScopeDenial {
///     identity: "rust-implementer".to_string(),
///     tool: "write_file".to_string(),
///     reason: DenialReason::PathOutsideScope {
///         access: PathAccess::Write,
///         path: "docs/README.md".to_string(),
///     },
/// };
/// assert_eq!(
///     denial.to_string(),
///     "identity `rust-implementer` may not call `write_file`: write to `docs/README.md` is outside scope.paths"
/// );
/// let json = serde_json::to_value(&denial).unwrap();
/// assert_eq!(json["reason"]["kind"], "path_outside_scope");
/// assert_eq!(json["reason"]["access"], "write");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeDenial {
    /// Name of the identity the call ran under.
    pub identity: String,
    /// Tool the model asked for.
    pub tool: String,
    /// Why the call was refused.
    pub reason: DenialReason,
}

impl ScopeDenial {
    /// The denial recorded when `tool` reached a protected path, attributed
    /// to [`UNSCOPED_IDENTITY`] until a scoped registry names its identity.
    pub fn protected(tool: &str, violation: &ProtectedPathViolation) -> Self {
        ScopeDenial {
            identity: UNSCOPED_IDENTITY.to_string(),
            tool: tool.to_string(),
            reason: DenialReason::ProtectedPath {
                path: violation.path.clone(),
                rule: violation.rule.clone(),
            },
        }
    }
}

impl fmt::Display for ScopeDenial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "identity `{}` may not call `{}`: {}",
            self.identity, self.tool, self.reason
        )
    }
}

/// Error building a scope from an identity whose globs do not compile.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScopeError {
    /// A `scope.paths` or `scope.read_paths` entry is not valid glob syntax.
    #[error("identity `{identity}`: `{pattern}` in {field} is not a valid glob: {reason}")]
    InvalidGlob {
        /// Identity whose scope was being compiled.
        identity: String,
        /// `scope.paths` or `scope.read_paths`.
        field: &'static str,
        /// The offending glob as written.
        pattern: String,
        /// The glob parser's explanation.
        reason: String,
    },
    /// The task's target repository is not listed in `scope.repos`.
    #[error("identity `{identity}` may not run against `{repo}`: not in scope.repos")]
    RepoOutsideScope {
        /// Identity the task was dispatched under.
        identity: String,
        /// The repository the task targets, or the path when it has no
        /// recognisable `origin` remote.
        repo: String,
    },
}

const MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

/// The paths an identity may read and write inside a task worktree.
///
/// `writable` mirrors `scope.paths`; `readable` mirrors `scope.read_paths`,
/// and `None` leaves reads unrestricted within the worktree. Globs are
/// matched against the worktree-relative path with `*` never crossing a
/// `/`, so `api/*` covers `api/lib.rs` but not `api/sub/lib.rs`, while
/// `api/**` covers both.
///
/// ```
/// use harness::identity::AgentIdentity;
/// use harness::scope::{PathAccess, PathScope};
/// use std::path::Path;
///
/// let toml = r#"
/// [identity]
/// name = "rust-implementer"
/// description = "Implements one issue."
/// loop = "inner"
/// model = "gemma4:e4b"
/// system_prompt = { inline = "You implement one issue at a time." }
///
/// [scope]
/// repos = ["github.com/example/repo"]
/// paths = ["api/**", "shared/**"]
/// max_effect = "workspace"
/// tools = ["read_file", "write_file", "search"]
///
/// [limits]
/// max_iterations = 200
/// max_wall_clock_secs = 3600
/// max_concurrent = 4
/// "#;
/// let identity = AgentIdentity::from_toml_str(toml, "rust-implementer.toml").unwrap();
/// let scope = PathScope::from_identity(&identity).unwrap();
///
/// assert_eq!(scope.identity(), "rust-implementer");
/// assert!(scope.permits(PathAccess::Write, Path::new("api/lib.rs")));
/// assert!(!scope.permits(PathAccess::Write, Path::new("docs/README.md")));
/// assert!(scope.permits(PathAccess::Read, Path::new("docs/README.md")));
/// ```
#[derive(Debug, Clone)]
pub struct PathScope {
    identity: String,
    writable: Vec<Pattern>,
    readable: Option<Vec<Pattern>>,
}

fn compile(
    identity: &str,
    field: &'static str,
    globs: &[String],
) -> Result<Vec<Pattern>, ScopeError> {
    globs
        .iter()
        .map(|glob| Pattern::new(glob).map_err(|e| invalid_glob(identity, field, glob, e)))
        .collect()
}

fn invalid_glob(
    identity: &str,
    field: &'static str,
    glob: &str,
    e: glob::PatternError,
) -> ScopeError {
    ScopeError::InvalidGlob {
        identity: identity.to_string(),
        field,
        pattern: glob.to_string(),
        reason: e.msg.to_string(),
    }
}

impl PathScope {
    /// Compile a scope for `identity` from writable globs and optional
    /// readable globs.
    pub fn new(
        identity: &str,
        writable: &[String],
        readable: Option<&[String]>,
    ) -> Result<Self, ScopeError> {
        let writable = compile(identity, "scope.paths", writable)?;
        let readable = match readable {
            Some(globs) => Some(compile(identity, "scope.read_paths", globs)?),
            None => None,
        };
        Ok(Self {
            identity: identity.to_string(),
            writable,
            readable,
        })
    }

    /// The scope declared by `identity`'s `[scope]` table.
    pub fn from_identity(identity: &AgentIdentity) -> Result<Self, ScopeError> {
        let readable = identity.scope.read_paths.as_deref();
        Self::new(identity.name(), &identity.scope.paths, readable)
    }

    /// Name of the identity this scope enforces.
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// Whether `relative` (a worktree-relative path) may be accessed.
    pub fn permits(&self, access: PathAccess, relative: &Path) -> bool {
        let patterns = match access {
            PathAccess::Write => &self.writable,
            PathAccess::Read => match &self.readable {
                Some(readable) => readable,
                None => return true,
            },
        };
        patterns
            .iter()
            .any(|pattern| pattern.matches_path_with(relative, MATCH_OPTIONS))
    }

    /// Whether the directory `relative` holds at least one readable file,
    /// so that listings can show the directories that lead somewhere.
    pub fn contains_readable(&self, dir: &Path, workspace_root: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                if self.contains_readable(&path, workspace_root) {
                    return true;
                }
            } else if self.permits(PathAccess::Read, relative_to(&path, workspace_root)) {
                return true;
            }
        }
        false
    }

    /// A denial of `tool` under this scope's identity.
    pub fn deny(&self, tool: &str, reason: DenialReason) -> ScopeDenial {
        ScopeDenial {
            identity: self.identity.clone(),
            tool: tool.to_string(),
            reason,
        }
    }
}

/// Normalise a git remote URL to `host/owner/name`, the form `scope.repos`
/// entries use. Handles `https://`, `ssh://` and scp-style remotes, drops
/// credentials, ports and a trailing `.git`, and lowercases nothing but the
/// host.
///
/// ```
/// use harness::scope::repo_slug;
///
/// assert_eq!(
///     repo_slug("git@GitHub.com:example/repo.git").as_deref(),
///     Some("github.com/example/repo")
/// );
/// assert_eq!(
///     repo_slug("https://token@github.com/example/repo/").as_deref(),
///     Some("github.com/example/repo")
/// );
/// assert_eq!(repo_slug("/srv/git/repo"), None);
/// ```
pub fn repo_slug(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((_, rest)) => {
            let rest = rest.rsplit_once('@').map_or(rest, |(_, after)| after);
            let (authority, path) = rest.split_once('/')?;
            (authority.split(':').next()?, path)
        }
        None => {
            let rest = url.rsplit_once('@').map_or(url, |(_, after)| after);
            rest.split_once(':')?
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let valid = |part: &str| !part.is_empty() && !part.contains('/');
    if host.is_empty() || !valid(owner) || !valid(name) {
        return None;
    }
    Some(format!("{}/{owner}/{name}", host.to_ascii_lowercase()))
}

fn origin_slug(repo_path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    repo_slug(&String::from_utf8_lossy(&output.stdout))
}

/// Refuse a task whose target repository is not in the identity's
/// `scope.repos`. The repository is named by its `origin` remote; without a
/// recognisable one it cannot be named, so the check fails closed.
///
/// ```
/// use harness::identity::AgentIdentity;
/// use harness::scope::check_repo;
///
/// let toml = r#"
/// [identity]
/// name = "reader"
/// description = "Reads."
/// loop = "inner"
/// model = "m"
/// system_prompt = { inline = "Read." }
///
/// [scope]
/// repos = ["github.com/example/repo"]
/// paths = []
/// max_effect = "none"
/// tools = ["read_file"]
///
/// [limits]
/// max_iterations = 1
/// max_wall_clock_secs = 1
/// max_concurrent = 1
/// "#;
/// let identity = AgentIdentity::from_toml_str(toml, "reader.toml").unwrap();
/// let not_a_repo = tempfile::tempdir().unwrap();
/// assert!(check_repo(&identity, not_a_repo.path()).is_err());
/// ```
pub fn check_repo(identity: &AgentIdentity, repo_path: &Path) -> Result<(), ScopeError> {
    let slug = origin_slug(repo_path);
    let allowed = slug.as_deref().is_some_and(|slug| {
        identity
            .scope
            .repos
            .iter()
            .any(|repo| repo.eq_ignore_ascii_case(slug))
    });
    if allowed {
        return Ok(());
    }
    Err(ScopeError::RepoOutsideScope {
        identity: identity.name().to_string(),
        repo: slug.unwrap_or_else(|| repo_path.display().to_string()),
    })
}

/// `path` relative to `workspace_root`, or `path` itself when it is not
/// beneath the root.
pub fn relative_to<'a>(path: &'a Path, workspace_root: &Path) -> &'a Path {
    path.strip_prefix(workspace_root).unwrap_or(path)
}

pub(crate) fn denial_path(relative: &Path) -> String {
    relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn violation(message: String) -> ToolError {
    ToolError::PathSecurityViolation { message }
}

fn cannot_resolve(path: &Path, e: std::io::Error) -> ToolError {
    violation(format!("Cannot resolve path '{}': {}", path.display(), e))
}

fn canonicalize(path: &Path, resolved: &Path) -> ToolResult<PathBuf> {
    resolved.canonicalize().map_err(|e| cannot_resolve(path, e))
}

fn outside_root(path: &Path) -> ToolError {
    violation(format!(
        "Path '{}' is outside workspace root",
        path.display()
    ))
}

pub(crate) fn canonical_root(workspace_root: &Path) -> ToolResult<PathBuf> {
    workspace_root
        .canonicalize()
        .map_err(|e| violation(format!("Cannot resolve workspace root: {}", e)))
}

fn join_root(path: &Path, workspace_root: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    }
}

/// Canonicalise `path` and require it to lie inside `workspace_root`.
/// Returns the canonical root and the canonical path.
fn canonical_within_workspace(
    path: &Path,
    workspace_root: &Path,
) -> ToolResult<(PathBuf, PathBuf)> {
    let root = canonical_root(workspace_root)?;
    let resolved = join_root(path, workspace_root);
    let canonical = canonicalize(path, &resolved)?;
    if !canonical.starts_with(&root) {
        return Err(outside_root(path));
    }
    Ok((root, canonical))
}

/// Validate a path for reading: it must exist and resolve inside the
/// worktree. Returns the canonical path.
pub fn validate_path_within_workspace(path: &Path, workspace_root: &Path) -> ToolResult<PathBuf> {
    canonical_within_workspace(path, workspace_root).map(|(_, canonical)| canonical)
}

/// The deepest existing ancestor of `path` (possibly `path` itself) and
/// the components below it.
fn existing_ancestor(path: &Path) -> (PathBuf, PathBuf) {
    let mut ancestor = path.to_path_buf();
    let mut remainder = PathBuf::new();
    while ancestor.symlink_metadata().is_err() {
        let name = ancestor.file_name().map(PathBuf::from).unwrap_or_default();
        remainder = name.join(&remainder);
        if !ancestor.pop() {
            break;
        }
    }
    (ancestor, remainder)
}

/// Validate a path for writing: it need not exist yet, but its deepest
/// existing ancestor must resolve inside the worktree and it must contain
/// no `..` component. Returns the (non-canonical) path to write.
pub fn validate_path_for_write(path: &Path, workspace_root: &Path) -> ToolResult<PathBuf> {
    resolve_for_write(path, workspace_root).map(|(resolved, _)| resolved)
}

fn resolve_for_write(path: &Path, workspace_root: &Path) -> ToolResult<(PathBuf, PathBuf)> {
    let root = canonical_root(workspace_root)?;
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(violation("Path contains '..' components".to_string()));
    }
    let resolved = join_root(path, workspace_root);
    let (ancestor, remainder) = existing_ancestor(&resolved);
    let canonical_ancestor = canonicalize(path, &ancestor)?;
    let canonical = canonical_ancestor.join(remainder);
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| outside_root(path))?;
    Ok((resolved, relative.to_path_buf()))
}

/// Resolve `path` for `access` by `tool`, applying escape protection and,
/// when `scope` is present, the identity's globs.
///
/// Reads return the canonical path; writes return the path to write, whose
/// parents may not exist yet. A path outside the scope is
/// [`ToolError::ScopeDenied`]; a path outside the worktree is
/// [`ToolError::PathSecurityViolation`] with or without a scope.
pub fn resolve_path(
    scope: Option<&PathScope>,
    tool: &str,
    access: PathAccess,
    path: &Path,
    workspace_root: &Path,
) -> ToolResult<PathBuf> {
    resolve(scope, None, tool, access, path, workspace_root)
}

/// [`resolve_path`] that also refuses writes to `protected` paths.
///
/// Protection is judged before the scope, so a protected path is
/// [`ToolError::ProtectedPath`] whether or not the identity's globs would
/// have permitted it; reads are never protected.
pub fn resolve_path_guarded(
    scope: Option<&PathScope>,
    protected: &ProtectedPaths,
    tool: &str,
    access: PathAccess,
    path: &Path,
    workspace_root: &Path,
) -> ToolResult<PathBuf> {
    resolve(scope, Some(protected), tool, access, path, workspace_root)
}

fn resolve(
    scope: Option<&PathScope>,
    protected: Option<&ProtectedPaths>,
    tool: &str,
    access: PathAccess,
    path: &Path,
    workspace_root: &Path,
) -> ToolResult<PathBuf> {
    let (resolved, relative) = match access {
        PathAccess::Read => {
            let root = canonical_root(workspace_root)?;
            let joined = join_root(path, workspace_root);
            match joined.canonicalize() {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if let Some(denial) =
                        deny_missing_read_uniformly(scope, tool, path, workspace_root)
                    {
                        return Err(denial);
                    }
                    return Err(cannot_resolve(path, e));
                }
                canonicalized => {
                    let canonical = canonicalized.map_err(|e| cannot_resolve(path, e))?;
                    if !canonical.starts_with(&root) {
                        return Err(outside_root(path));
                    }
                    let relative = relative_to(&canonical, &root).to_path_buf();
                    (canonical, relative)
                }
            }
        }
        PathAccess::Write => resolve_for_write(path, workspace_root)?,
    };
    if let (PathAccess::Write, Some(protected)) = (access, protected) {
        protected
            .check(&relative)
            .map_err(ToolError::ProtectedPath)?;
    }
    if let Some(scope) = scope {
        if !scope.permits(access, &relative) {
            let path = denial_path(&relative);
            let reason = DenialReason::PathOutsideScope { access, path };
            return Err(ToolError::ScopeDenied(scope.deny(tool, reason)));
        }
    }
    Ok(resolved)
}

/// When a read target does not exist, decide whether to report that
/// directly or to deny it as out-of-scope instead.
///
/// Without this, a scoped identity could distinguish "exists but outside
/// `scope.read_paths`" ([`ToolError::ScopeDenied`]) from "does not exist"
/// ([`ToolError::PathSecurityViolation`]), using `read_file` as an oracle
/// for the existence of files it has no business knowing about. A relative,
/// non-traversing path that a scope would deny anyway is denied uniformly
/// instead, so both outcomes look identical to the caller; the raw I/O
/// error `canonical_within_workspace` produced is used everywhere else
/// (traversal attempts, absolute paths, and any read a scope would permit).
fn deny_missing_read_uniformly(
    scope: Option<&PathScope>,
    tool: &str,
    path: &Path,
    workspace_root: &Path,
) -> Option<ToolError> {
    let scope = scope?;
    if path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let relative = relative_to(&join_root(path, workspace_root), workspace_root).to_path_buf();
    if scope.permits(PathAccess::Read, &relative) {
        return None;
    }
    let path = denial_path(&relative);
    let reason = DenialReason::PathOutsideScope {
        access: PathAccess::Read,
        path,
    };
    Some(ToolError::ScopeDenied(scope.deny(tool, reason)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::example;
    use crate::protected::{ProtectedPathViolation, ProtectedPaths};
    use proptest::prelude::*;
    use tempfile::TempDir;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    fn scope(writable: &[&str], readable: Option<&[&str]>) -> PathScope {
        let readable = readable.map(strings);
        PathScope::new("tester", &strings(writable), readable.as_deref()).unwrap()
    }

    fn workspace() -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("api/sub")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("api/lib.rs"), "pub fn f() {}").unwrap();
        std::fs::write(dir.path().join("api/sub/deep.rs"), "").unwrap();
        std::fs::write(dir.path().join("docs/README.md"), "# docs").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        dir
    }

    #[test]
    fn writes_follow_scope_paths_and_reads_are_free_without_read_paths() {
        let scope = scope(&["api/**", "shared/**"], None);
        assert!(scope.permits(PathAccess::Write, Path::new("api/lib.rs")));
        assert!(scope.permits(PathAccess::Write, Path::new("api/sub/deep.rs")));
        assert!(scope.permits(PathAccess::Write, Path::new("shared/x")));
        assert!(!scope.permits(PathAccess::Write, Path::new("docs/README.md")));
        assert!(!scope.permits(PathAccess::Write, Path::new("api")));
        assert!(!scope.permits(PathAccess::Write, Path::new("apix/lib.rs")));
        assert!(scope.permits(PathAccess::Read, Path::new("docs/README.md")));
        assert!(scope.permits(PathAccess::Read, Path::new("")));
    }

    #[test]
    fn read_paths_restrict_reads_independently_of_writes() {
        let scope = scope(&["api/**"], Some(&["docs/**"]));
        assert!(scope.permits(PathAccess::Read, Path::new("docs/README.md")));
        assert!(!scope.permits(PathAccess::Read, Path::new("api/lib.rs")));
        assert!(scope.permits(PathAccess::Write, Path::new("api/lib.rs")));
        assert!(!scope.permits(PathAccess::Write, Path::new("docs/README.md")));
    }

    #[test]
    fn single_star_never_crosses_a_separator() {
        let scope = scope(&["api/*"], None);
        assert!(scope.permits(PathAccess::Write, Path::new("api/lib.rs")));
        assert!(!scope.permits(PathAccess::Write, Path::new("api/sub/deep.rs")));
        let any = self::scope(&["**"], None);
        assert!(any.permits(PathAccess::Write, Path::new("a/b/c.rs")));
    }

    #[test]
    fn from_identity_uses_the_scope_table() {
        let identity = example();
        let scope = PathScope::from_identity(&identity).unwrap();
        assert_eq!(scope.identity(), "rust-implementer");
        assert!(scope.permits(PathAccess::Write, Path::new("shared/types.rs")));
        assert!(!scope.permits(PathAccess::Write, Path::new("Cargo.toml")));
        assert!(scope.permits(PathAccess::Read, Path::new("Cargo.toml")));
    }

    #[test]
    fn invalid_globs_name_the_field() {
        let err = PathScope::new("x", &strings(&["api/["]), None).unwrap_err();
        assert!(
            matches!(&err, ScopeError::InvalidGlob { field: "scope.paths", pattern, .. } if pattern == "api/[")
        );
        assert!(err
            .to_string()
            .starts_with("identity `x`: `api/[` in scope.paths is not a valid glob: "));
        let readable = strings(&["docs/["]);
        let err = PathScope::new("x", &[], Some(&readable)).unwrap_err();
        assert!(matches!(
            err,
            ScopeError::InvalidGlob {
                field: "scope.read_paths",
                ..
            }
        ));
        let mut identity = example();
        identity.scope.paths = strings(&["[["]);
        assert!(PathScope::from_identity(&identity).is_err());
    }

    #[test]
    fn deny_attributes_the_identity() {
        let scope = scope(&[], None);
        let denial = scope.deny("write_file", DenialReason::ToolNotInScope);
        assert_eq!(denial.identity, "tester");
        assert_eq!(denial.tool, "write_file");
        assert_eq!(
            denial.to_string(),
            "identity `tester` may not call `write_file`: tool is not in scope"
        );
        let read = DenialReason::PathOutsideScope {
            access: PathAccess::Read,
            path: "a".to_string(),
        };
        assert_eq!(read.to_string(), "read of `a` is outside scope.read_paths");
        let json = serde_json::to_value(scope.deny("read_file", read.clone())).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"identity": "tester", "tool": "read_file", "reason": {"kind": "path_outside_scope", "access": "read", "path": "a"}})
        );
        let back: ScopeDenial = serde_json::from_value(json).unwrap();
        assert_eq!(back.reason, read);
        let not_in_scope = serde_json::to_value(DenialReason::ToolNotInScope).unwrap();
        assert_eq!(
            not_in_scope,
            serde_json::json!({"kind": "tool_not_in_scope"})
        );
    }

    #[test]
    fn unscoped_reads_return_the_canonical_path_and_reject_escapes() {
        let ws = workspace();
        let root = ws.path();
        let resolved = resolve_path(
            None,
            "read_file",
            PathAccess::Read,
            Path::new("api/lib.rs"),
            root,
        )
        .unwrap();
        assert_eq!(resolved, root.join("api/lib.rs").canonicalize().unwrap());
        let absolute = root.join("docs/README.md");
        assert!(resolve_path(None, "read_file", PathAccess::Read, &absolute, root).is_ok());
        let escape = resolve_path(
            None,
            "read_file",
            PathAccess::Read,
            Path::new("../../etc/passwd"),
            root,
        );
        assert!(matches!(
            escape,
            Err(ToolError::PathSecurityViolation { .. })
        ));
        let missing = resolve_path(
            None,
            "read_file",
            PathAccess::Read,
            Path::new("nope.rs"),
            root,
        );
        assert!(matches!(
            missing,
            Err(ToolError::PathSecurityViolation { .. })
        ));
        let outside = resolve_path(None, "read_file", PathAccess::Read, Path::new("/"), root);
        assert!(
            matches!(outside, Err(ToolError::PathSecurityViolation { message }) if message.contains("outside workspace root"))
        );
        let bad_root = resolve_path(
            None,
            "read_file",
            PathAccess::Read,
            Path::new("x"),
            Path::new("/definitely/missing/root"),
        );
        assert!(
            matches!(bad_root, Err(ToolError::PathSecurityViolation { message }) if message.contains("workspace root"))
        );
    }

    #[test]
    fn unscoped_writes_allow_new_files_and_reject_escapes() {
        let ws = workspace();
        let root = ws.path();
        let new_file = Path::new("api/new/dir/file.rs");
        assert_eq!(
            validate_path_for_write(new_file, root).unwrap(),
            root.join(new_file)
        );
        let dotdot = validate_path_for_write(Path::new("api/../../x"), root);
        assert!(
            matches!(dotdot, Err(ToolError::PathSecurityViolation { message }) if message.contains(".."))
        );
        let outside = validate_path_for_write(Path::new("/definitely/not/here.rs"), root);
        assert!(
            matches!(outside, Err(ToolError::PathSecurityViolation { message }) if message.contains("outside workspace root"))
        );
        assert!(
            validate_path_for_write(Path::new("x"), Path::new("/definitely/missing/root")).is_err()
        );
        assert_eq!(
            validate_path_within_workspace(Path::new("Cargo.toml"), root).unwrap(),
            root.join("Cargo.toml").canonicalize().unwrap()
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_path_within_workspace_reports_a_missing_path() {
        let dir = workspace();
        let err = validate_path_within_workspace(Path::new("missing.rs"), dir.path()).unwrap_err();
        assert!(
            matches!(&err, ToolError::PathSecurityViolation { message } if message.contains("Cannot resolve path 'missing.rs'")),
            "{err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_path_within_workspace_rejects_a_symlink_escape() {
        let ws = workspace();
        let root = ws.path();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();
        let escape = validate_path_within_workspace(Path::new("escape/secret"), root);
        assert!(
            matches!(escape, Err(ToolError::PathSecurityViolation { .. })),
            "{escape:?}"
        );
    }

    #[test]
    fn scoped_writes_outside_paths_are_denied_with_the_relative_path() {
        let ws = workspace();
        let root = ws.path();
        let scope = scope(&["api/**"], None);
        let ok = resolve_path(
            Some(&scope),
            "write_file",
            PathAccess::Write,
            Path::new("api/new.rs"),
            root,
        )
        .unwrap();
        assert_eq!(ok, root.join("api/new.rs"));
        let absolute = root.join("api/sub/other.rs");
        assert_eq!(
            resolve_path(
                Some(&scope),
                "write_file",
                PathAccess::Write,
                &absolute,
                root
            )
            .unwrap(),
            absolute
        );
        let denied = resolve_path(
            Some(&scope),
            "write_file",
            PathAccess::Write,
            Path::new("./docs/new.md"),
            root,
        );
        match denied {
            Err(ToolError::ScopeDenied(denial)) => {
                assert_eq!(denial.identity, "tester");
                assert_eq!(denial.tool, "write_file");
                assert_eq!(
                    denial.reason,
                    DenialReason::PathOutsideScope {
                        access: PathAccess::Write,
                        path: "docs/new.md".to_string()
                    }
                );
            }
            other => panic!("expected ScopeDenied, got {other:?}"),
        }
        let escape = resolve_path(
            Some(&scope),
            "write_file",
            PathAccess::Write,
            Path::new("api/../../x"),
            root,
        );
        assert!(matches!(
            escape,
            Err(ToolError::PathSecurityViolation { .. })
        ));
        let read = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("docs/README.md"),
            root,
        );
        assert!(read.is_ok(), "reads are unrestricted without read_paths");
    }

    #[test]
    fn scoped_reads_outside_read_paths_are_denied() {
        let ws = workspace();
        let root = ws.path();
        let scope = scope(&["api/**"], Some(&["api/**"]));
        assert!(resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("api/lib.rs"),
            root
        )
        .is_ok());
        let denied = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("docs/README.md"),
            root,
        );
        match denied {
            Err(ToolError::ScopeDenied(denial)) => {
                assert_eq!(
                    denial.reason,
                    DenialReason::PathOutsideScope {
                        access: PathAccess::Read,
                        path: "docs/README.md".to_string()
                    }
                );
                assert_eq!(denial.to_string(), "identity `tester` may not call `read_file`: read of `docs/README.md` is outside scope.read_paths");
            }
            other => panic!("expected ScopeDenied, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_out_of_scope_read_does_not_leak_whether_it_exists() {
        let ws = workspace();
        let root = ws.path();
        let scope = scope(&["api/**"], Some(&["api/**"]));
        let existing = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("docs/README.md"),
            root,
        );
        let missing = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("docs/does_not_exist.md"),
            root,
        );
        for (label, result) in [("existing", existing), ("missing", missing)] {
            match result {
                Err(ToolError::ScopeDenied(denial)) => {
                    assert_eq!(
                        denial.reason,
                        DenialReason::PathOutsideScope {
                            access: PathAccess::Read,
                            path: format!(
                                "docs/{}",
                                if label == "existing" {
                                    "README.md"
                                } else {
                                    "does_not_exist.md"
                                }
                            ),
                        },
                        "{label}"
                    );
                }
                other => panic!("{label}: expected ScopeDenied, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_in_scope_read_still_reports_it_is_missing() {
        let ws = workspace();
        let root = ws.path();
        let scope = scope(&["api/**"], Some(&["api/**"]));
        let missing = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("api/does_not_exist.rs"),
            root,
        );
        match &missing {
            Err(ToolError::PathSecurityViolation { message }) => {
                assert!(message.contains("does_not_exist.rs"), "{message}");
            }
            other => panic!("expected PathSecurityViolation, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_scoped_read_with_a_traversal_or_absolute_path_is_not_masked_as_a_scope_denial() {
        let ws = workspace();
        let root = ws.path();
        let scope = scope(&["api/**"], Some(&["api/**"]));
        let traversal = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("api/../does_not_exist.rs"),
            root,
        );
        assert!(
            matches!(traversal, Err(ToolError::PathSecurityViolation { .. })),
            "{traversal:?}"
        );
        let outside_missing = std::env::temp_dir()
            .join("nanna-definitely-not-here")
            .join("here.rs");
        let absolute = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            &outside_missing,
            root,
        );
        assert!(
            matches!(absolute, Err(ToolError::PathSecurityViolation { .. })),
            "{absolute:?}"
        );
    }

    #[test]
    fn a_missing_unscoped_read_still_reports_it_is_missing() {
        let ws = workspace();
        let root = ws.path();
        let missing = resolve_path(
            None,
            "read_file",
            PathAccess::Read,
            Path::new("nope.rs"),
            root,
        );
        assert!(matches!(
            missing,
            Err(ToolError::PathSecurityViolation { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_judged_by_their_target() {
        let ws = workspace();
        let root = ws.path();
        std::os::unix::fs::symlink(root.join("api"), root.join("shared")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).unwrap();

        let scope = scope(&["api/**"], Some(&["api/**"]));
        let via_link = resolve_path(
            Some(&scope),
            "write_file",
            PathAccess::Write,
            Path::new("shared/new.rs"),
            root,
        )
        .unwrap();
        assert_eq!(via_link, root.join("shared/new.rs"));
        assert!(resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("shared/lib.rs"),
            root
        )
        .is_ok());

        let read_escape = resolve_path(
            Some(&scope),
            "read_file",
            PathAccess::Read,
            Path::new("escape/secret"),
            root,
        );
        assert!(matches!(
            read_escape,
            Err(ToolError::PathSecurityViolation { .. })
        ));
        let write_escape = resolve_path(
            Some(&scope),
            "write_file",
            PathAccess::Write,
            Path::new("escape/new"),
            root,
        );
        assert!(matches!(
            write_escape,
            Err(ToolError::PathSecurityViolation { .. })
        ));
        let unscoped_escape = resolve_path(
            None,
            "write_file",
            PathAccess::Write,
            Path::new("escape/deeper/new"),
            root,
        );
        assert!(matches!(
            unscoped_escape,
            Err(ToolError::PathSecurityViolation { .. })
        ));
    }

    #[test]
    fn contains_readable_walks_until_it_finds_a_readable_file() {
        let ws = workspace();
        let root = ws.path();
        let scope = scope(&[], Some(&["api/sub/**"]));
        assert!(scope.contains_readable(&root.join("api"), root));
        assert!(scope.contains_readable(&root.join("api/sub"), root));
        assert!(!scope.contains_readable(&root.join("docs"), root));
        assert!(!scope.contains_readable(&root.join("missing"), root));
        let everything = self::scope(&[], None);
        assert!(everything.contains_readable(root, root));
        assert!(!everything.contains_readable(&root.join("api/lib.rs"), root));
    }

    #[test]
    fn relative_to_strips_the_root_or_returns_the_path() {
        assert_eq!(
            relative_to(Path::new("/r/a/b"), Path::new("/r")),
            Path::new("a/b")
        );
        assert_eq!(
            relative_to(Path::new("/x/a"), Path::new("/r")),
            Path::new("/x/a")
        );
    }

    #[test]
    fn existing_ancestor_splits_at_the_first_missing_component() {
        let ws = workspace();
        let root = ws.path();
        let (ancestor, remainder) = existing_ancestor(&root.join("api/new/deep.rs"));
        assert_eq!(ancestor, root.join("api"));
        assert_eq!(remainder, Path::new("new/deep.rs"));
        let (ancestor, remainder) = existing_ancestor(&root.join("api/lib.rs"));
        assert_eq!(ancestor, root.join("api/lib.rs"));
        assert_eq!(remainder, Path::new(""));
        let (ancestor, _) = existing_ancestor(Path::new("definitely-missing-relative/x"));
        assert_eq!(ancestor, Path::new(""));
    }

    #[cfg(unix)]
    fn assert_write_rejected(scope: Option<&PathScope>, path: &str, root: &Path) {
        let result = resolve_path(
            scope,
            "write_file",
            PathAccess::Write,
            Path::new(path),
            root,
        );
        assert!(
            matches!(result, Err(ToolError::PathSecurityViolation { .. })),
            "{path}: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_leaf_pointing_outside_the_workspace_is_rejected_for_writes() {
        let ws = workspace();
        let root = ws.path();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("planted");
        std::os::unix::fs::symlink(&target, root.join("api/dangling")).unwrap();
        assert_write_rejected(None, "api/dangling", root);
        assert_write_rejected(Some(&scope(&["api/**"], None)), "api/dangling", root);
        assert!(validate_path_for_write(Path::new("api/dangling"), root).is_err());
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_into_a_protected_path_is_rejected_for_writes() {
        let ws = workspace();
        let root = ws.path();
        std::os::unix::fs::symlink(root.join("docs/new.md"), root.join("api/sneaky")).unwrap();
        assert_write_rejected(Some(&scope(&["api/**"], None)), "api/sneaky", root);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_directory_component_is_rejected_for_writes() {
        let ws = workspace();
        let root = ws.path();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("nope"), root.join("api/dir")).unwrap();
        assert_write_rejected(None, "api/dir/file.rs", root);
        assert_write_rejected(Some(&scope(&["api/**"], None)), "api/dir/file.rs", root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_an_existing_outside_file_is_rejected_for_writes() {
        let ws = workspace();
        let root = ws.path();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("existing");
        std::fs::write(&target, "x").unwrap();
        std::os::unix::fs::symlink(&target, root.join("api/link")).unwrap();
        assert_write_rejected(None, "api/link", root);
        assert_write_rejected(Some(&scope(&["api/**"], None)), "api/link", root);
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_chain_is_rejected_for_writes() {
        let ws = workspace();
        let root = ws.path();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("planted"), root.join("api/b")).unwrap();
        std::os::unix::fs::symlink(root.join("api/b"), root.join("api/a")).unwrap();
        assert_write_rejected(None, "api/a", root);
        assert_write_rejected(Some(&scope(&["api/**"], None)), "api/a", root);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_is_rejected_for_writes() {
        let ws = workspace();
        let root = ws.path();
        std::os::unix::fs::symlink(root.join("api/y"), root.join("api/x")).unwrap();
        std::os::unix::fs::symlink(root.join("api/x"), root.join("api/y")).unwrap();
        assert_write_rejected(None, "api/x", root);
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_valid_in_workspace_symlinks_and_new_paths_still_succeed() {
        let ws = workspace();
        let root = ws.path();
        std::os::unix::fs::symlink(root.join("api/lib.rs"), root.join("api/alias")).unwrap();
        let scope = scope(&["api/**"], None);
        for path in ["api/alias", "api/fresh.rs", "api/new/deep/fresh.rs"] {
            let result = resolve_path(
                Some(&scope),
                "write_file",
                PathAccess::Write,
                Path::new(path),
                root,
            );
            assert_eq!(result.unwrap(), root.join(path));
        }
    }

    #[cfg(unix)]
    #[test]
    fn existing_ancestor_treats_a_dangling_symlink_as_existing() {
        let ws = workspace();
        let root = ws.path();
        std::os::unix::fs::symlink(root.join("gone"), root.join("api/dangling")).unwrap();
        let (ancestor, remainder) = existing_ancestor(&root.join("api/dangling"));
        assert_eq!(ancestor, root.join("api/dangling"));
        assert_eq!(remainder, Path::new(""));
    }

    fn segment() -> impl Strategy<Value = String> {
        "[a-z][a-z0-9_]{0,6}(\\.[a-z]{1,3})?"
    }

    fn relative_path() -> impl Strategy<Value = String> {
        prop::collection::vec(segment(), 1..4).prop_map(|parts| parts.join("/"))
    }

    proptest! {
        #[test]
        fn a_catch_all_permits_everything_and_an_empty_scope_permits_no_write(path in relative_path()) {
            let all = scope(&["**"], Some(&["**"]));
            prop_assert!(all.permits(PathAccess::Write, Path::new(&path)));
            prop_assert!(all.permits(PathAccess::Read, Path::new(&path)));
            let none = scope(&[], Some(&[]));
            prop_assert!(!none.permits(PathAccess::Write, Path::new(&path)));
            prop_assert!(!none.permits(PathAccess::Read, Path::new(&path)));
            let free_reads = scope(&[], None);
            prop_assert!(free_reads.permits(PathAccess::Read, Path::new(&path)));
        }

        #[test]
        fn a_directory_glob_permits_exactly_the_paths_beneath_it(dir in segment(), path in relative_path()) {
            let scoped = scope(&[&format!("{dir}/**")], None);
            let inside = format!("{dir}/{path}");
            prop_assert!(scoped.permits(PathAccess::Write, Path::new(&inside)));
            let outside = format!("{dir}x/{path}");
            prop_assert!(!scoped.permits(PathAccess::Write, Path::new(&outside)));
            prop_assert!(!scoped.permits(PathAccess::Write, Path::new(&dir)));
        }
    }

    fn guarded(
        scope: Option<&PathScope>,
        access: PathAccess,
        path: &str,
        root: &Path,
    ) -> ToolResult<PathBuf> {
        let protected = ProtectedPaths::with_config_dir(root, None);
        resolve_path_guarded(
            scope,
            &protected,
            "write_file",
            access,
            Path::new(path),
            root,
        )
    }

    fn protected_error(result: ToolResult<PathBuf>) -> ProtectedPathViolation {
        match result {
            Err(ToolError::ProtectedPath(violation)) => violation,
            other => panic!("expected ProtectedPath, got {other:?}"),
        }
    }

    #[test]
    fn a_protected_write_is_refused_even_under_a_catch_all_scope() {
        let ws = workspace();
        let root = ws.path();
        let all = scope(&["**"], None);
        let violation = protected_error(guarded(
            Some(&all),
            PathAccess::Write,
            ".nanna/agents/x.toml",
            root,
        ));
        assert_eq!(violation.path, ".nanna/agents/x.toml");
        assert_eq!(violation.rule, ".nanna/**");
        let unscoped = protected_error(guarded(None, PathAccess::Write, "codecov.yml", root));
        assert_eq!(unscoped.rule, "codecov.yml");
    }

    #[test]
    fn protection_wins_over_a_scope_denial() {
        let ws = workspace();
        let root = ws.path();
        let narrow = scope(&["api/**"], None);
        let violation = protected_error(guarded(
            Some(&narrow),
            PathAccess::Write,
            ".github/workflows/ci.yml",
            root,
        ));
        assert_eq!(violation.rule, ".github/workflows/**");
        let denied = guarded(Some(&narrow), PathAccess::Write, "docs/new.md", root);
        assert!(matches!(denied, Err(ToolError::ScopeDenied(_))));
        let allowed = guarded(Some(&narrow), PathAccess::Write, "api/new.rs", root).unwrap();
        assert_eq!(allowed, root.join("api/new.rs"));
    }

    #[test]
    fn protected_paths_may_still_be_read() {
        let ws = workspace();
        let root = ws.path();
        std::fs::create_dir_all(root.join(".nanna/agents")).unwrap();
        std::fs::write(root.join(".nanna/agents/x.toml"), "").unwrap();
        let read = guarded(None, PathAccess::Read, ".nanna/agents/x.toml", root).unwrap();
        assert_eq!(
            read,
            root.canonicalize().unwrap().join(".nanna/agents/x.toml")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_into_a_protected_directory_is_judged_by_its_target() {
        let ws = workspace();
        let root = ws.path();
        std::fs::create_dir_all(root.join(".nanna")).unwrap();
        std::os::unix::fs::symlink(root.join(".nanna"), root.join("cfg")).unwrap();
        let violation = protected_error(guarded(None, PathAccess::Write, "cfg/x.toml", root));
        assert_eq!(violation.path, ".nanna/x.toml");
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_into_a_protected_root_is_refused() {
        let ws = workspace();
        let root = ws.path();
        std::os::unix::fs::symlink(root.join(".nanna/agents/x.toml"), root.join("leaf")).unwrap();
        std::os::unix::fs::symlink(root.join(".nanna/missing"), root.join("dir")).unwrap();
        let all = scope(&["**"], None);
        for (scope, path) in [
            (None, "leaf"),
            (None, "dir/x.toml"),
            (Some(&all), "leaf"),
            (Some(&all), "dir/x.toml"),
        ] {
            let result = guarded(scope, PathAccess::Write, path, root);
            assert!(
                matches!(
                    result,
                    Err(ToolError::ProtectedPath(_)) | Err(ToolError::PathSecurityViolation { .. })
                ),
                "{path}: {result:?}"
            );
        }
        assert!(!root.join(".nanna").exists());
    }

    #[test]
    fn an_escape_is_still_a_security_violation_when_guarded() {
        let ws = workspace();
        let root = ws.path();
        let escape = guarded(None, PathAccess::Write, "../.nanna/x", root);
        assert!(matches!(
            escape,
            Err(ToolError::PathSecurityViolation { .. })
        ));
    }

    #[test]
    fn protected_path_denial_reason_displays_and_serialises() {
        let reason = DenialReason::ProtectedPath {
            path: "codecov.yml".to_string(),
            rule: "codecov.yml".to_string(),
        };
        assert_eq!(
            reason.to_string(),
            "write to `codecov.yml` is refused by protected rule `codecov.yml`"
        );
        let json = serde_json::to_value(&reason).unwrap();
        assert_eq!(json["kind"], "protected_path");
        assert_eq!(json["rule"], "codecov.yml");
        let denial = ScopeDenial::protected(
            "write_file",
            &ProtectedPathViolation {
                path: "codecov.yml".to_string(),
                rule: "codecov.yml".to_string(),
            },
        );
        assert_eq!(denial.identity, UNSCOPED_IDENTITY);
        assert_eq!(denial.tool, "write_file");
        assert_eq!(denial.reason, reason);
    }

    fn writable_glob() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("**".to_string()),
            Just("*".to_string()),
            Just("**/*".to_string()),
            Just("**/*.toml".to_string()),
            Just("**/*.yml".to_string()),
            Just(".nanna/**".to_string()),
            Just(".github/**".to_string()),
            Just(".*".to_string()),
            Just(".*/**".to_string()),
            segment().prop_map(|s| format!("{s}/**")),
        ]
    }

    fn protected_path() -> impl Strategy<Value = String> {
        let prefix = prop_oneof![
            Just(".nanna".to_string()),
            Just(".github/workflows".to_string()),
            segment().prop_map(|s| format!("{s}/.nanna")),
            relative_path().prop_map(|p| format!("{p}/.nanna")),
        ];
        prop_oneof![
            (prefix, relative_path()).prop_map(|(p, rest)| format!("{p}/{rest}")),
            Just(".nanna".to_string()),
            Just(".github/CODEOWNERS".to_string()),
            Just("codecov.yml".to_string()),
            Just("windows.toml".to_string()),
        ]
    }

    proptest! {
        #[test]
        fn no_scope_glob_lets_a_write_reach_a_protected_path(globs in prop::collection::vec(writable_glob(), 1..4), path in protected_path()) {
            let ws = workspace();
            let root = ws.path();
            let scoped = scope(&globs.iter().map(String::as_str).collect::<Vec<_>>(), None);
            let violation = protected_error(guarded(Some(&scoped), PathAccess::Write, &path, root));
            prop_assert_eq!(violation.path, path);
        }
    }

    #[test]
    fn repo_slug_normalises_every_remote_shape() {
        let slug = |url: &str| repo_slug(url);
        let expected = Some("github.com/example/repo".to_string());
        assert_eq!(slug("https://github.com/example/repo.git"), expected);
        assert_eq!(slug("https://user:pw@GITHUB.com/example/repo"), expected);
        assert_eq!(slug("ssh://git@github.com:22/example/repo.git"), expected);
        assert_eq!(slug("git@github.com:example/repo.git"), expected);
        assert_eq!(slug("  https://github.com/example/repo/  "), expected);
        assert_eq!(slug("https://github.com/example"), None);
        assert_eq!(slug("https://github.com"), None);
        assert_eq!(slug("https://github.com/a/b/c"), None);
        assert_eq!(slug("https:///example/repo"), None);
        assert_eq!(slug("https://github.com//repo"), None);
        assert_eq!(slug("/srv/git/repo"), None);
        assert_eq!(slug(""), None);
    }

    fn repo_with_origin(url: Option<&str>) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir.path())
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success());
        };
        git(&["init", "-q"]);
        if let Some(url) = url {
            git(&["remote", "add", "origin", url]);
        }
        dir
    }

    #[test]
    fn check_repo_allows_only_listed_repositories() {
        let identity = example();
        let listed = repo_with_origin(Some("git@github.com:Example/Repo.git"));
        assert_eq!(check_repo(&identity, listed.path()), Ok(()));

        let other = repo_with_origin(Some("https://github.com/example/other"));
        assert_eq!(
            check_repo(&identity, other.path()),
            Err(ScopeError::RepoOutsideScope {
                identity: "rust-implementer".to_string(),
                repo: "github.com/example/other".to_string(),
            })
        );
    }

    #[test]
    fn check_repo_fails_closed_without_a_nameable_origin() {
        let identity = example();
        let no_remote = repo_with_origin(None);
        let err = check_repo(&identity, no_remote.path()).unwrap_err();
        assert!(err
            .to_string()
            .contains(&no_remote.path().display().to_string()));

        let local_remote = repo_with_origin(Some("/srv/git/repo"));
        assert!(check_repo(&identity, local_remote.path()).is_err());

        let mut open_identity = example();
        open_identity.scope.repos.clear();
        let listed = repo_with_origin(Some("https://github.com/example/repo"));
        assert!(check_repo(&open_identity, listed.path()).is_err());
    }
}
