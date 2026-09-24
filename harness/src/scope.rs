//! Runtime scope enforcement for agents running under an identity.
//!
//! [`PathScope`] narrows which paths inside a task worktree the file tools
//! may read and write, and [`ScopeDenial`] is the structured record of every
//! call a scope refuses, so the task result and the auditor can see it.
//!
//! Escape protection (`..`, absolute paths elsewhere, symlinks resolving
//! outside the worktree) is shared with the unscoped tools through
//! [`resolve_path`]: a scope only ever narrows what an unscoped tool would
//! already allow.

use crate::identity::AgentIdentity;
use crate::tools::{ToolError, ToolResult};
use glob::{MatchOptions, Pattern};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

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
}

impl fmt::Display for DenialReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DenialReason::ToolNotInScope => f.write_str("tool is not in scope"),
            DenialReason::PathOutsideScope { access, path } => match access {
                PathAccess::Read => write!(f, "read of `{path}` is outside scope.read_paths"),
                PathAccess::Write => write!(f, "write to `{path}` is outside scope.paths"),
            },
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

/// `path` relative to `workspace_root`, or `path` itself when it is not
/// beneath the root.
pub fn relative_to<'a>(path: &'a Path, workspace_root: &Path) -> &'a Path {
    path.strip_prefix(workspace_root).unwrap_or(path)
}

fn violation(message: String) -> ToolError {
    ToolError::PathSecurityViolation { message }
}

fn cannot_resolve(path: &Path, e: std::io::Error) -> ToolError {
    violation(format!("Cannot resolve path '{}': {}", path.display(), e))
}

fn outside_root(path: &Path) -> ToolError {
    violation(format!(
        "Path '{}' is outside workspace root",
        path.display()
    ))
}

fn canonical_root(workspace_root: &Path) -> ToolResult<PathBuf> {
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
    let canonical = resolved
        .canonicalize()
        .map_err(|e| cannot_resolve(path, e))?;
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
    while !ancestor.exists() {
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
    let canonical_ancestor = ancestor
        .canonicalize()
        .map_err(|e| cannot_resolve(path, e))?;
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
    let (resolved, relative) = match access {
        PathAccess::Read => {
            let (root, canonical) = canonical_within_workspace(path, workspace_root)?;
            let relative = relative_to(&canonical, &root).to_path_buf();
            (canonical, relative)
        }
        PathAccess::Write => resolve_for_write(path, workspace_root)?,
    };
    if let Some(scope) = scope {
        if !scope.permits(access, &relative) {
            let path = relative.to_string_lossy().into_owned();
            let reason = DenialReason::PathOutsideScope { access, path };
            return Err(ToolError::ScopeDenied(scope.deny(tool, reason)));
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::example;
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
}
