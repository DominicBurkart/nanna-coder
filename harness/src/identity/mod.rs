//! Agent identities: the human-authored "cards" the orchestrator spawns
//! agents from.
//!
//! An identity is a TOML file with three tables:
//!
//! ```toml
//! [identity]
//! name = "rust-implementer"
//! description = "Implements a scoped issue in a Rust workspace and opens a draft PR."
//! loop = "inner"
//! model = "gemma4:e4b"
//! system_prompt = "prompts/rust-implementer.md"
//!
//! [scope]
//! repos = ["github.com/example/repo"]
//! paths = ["api/**", "shared/**"]
//! max_effect = "repository"
//! tools = ["read_file", "write_file", "search", "cargo_*", "git_*"]
//!
//! [limits]
//! max_iterations = 200
//! max_wall_clock_secs = 3600
//! max_concurrent = 4
//! ```
//!
//! [`AgentIdentity::from_toml_str`] parses and validates a single file;
//! [`IdentityCatalog`] loads a directory of them and applies repo-local
//! overrides, which may only narrow the global identity they shadow.

mod dev_loop;
mod pattern;

pub use dev_loop::{DevLoop, UnknownDevLoop};
pub use pattern::{ToolPattern, ToolPatternError};
mod narrowing;
mod schema;

pub use schema::{AgentIdentity, IdentitySection, LimitsSection, ScopeSection, SystemPrompt};

use std::path::PathBuf;
use thiserror::Error;

/// Errors produced while loading, validating or combining identities.
#[derive(Debug, Error)]
pub enum IdentityError {
    /// A file or directory could not be read.
    #[error("{file}: {source}")]
    Io {
        /// Path that failed.
        file: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The TOML is malformed, has unknown keys or is missing required ones.
    #[error("{file}: {message}")]
    Parse {
        /// Identity file being parsed.
        file: PathBuf,
        /// Parser diagnostic.
        message: String,
    },
    /// A single field is well-formed TOML but semantically invalid.
    #[error("{file}: invalid `{field}`: {reason}")]
    InvalidField {
        /// Identity file being validated.
        file: PathBuf,
        /// Dotted TOML key, with an index for list items (`scope.tools[2]`).
        field: String,
        /// Human-readable explanation.
        reason: String,
    },
    /// A `system_prompt` path canonicalises to a location outside the identity directory.
    #[error("{file}: system prompt `{}` resolves outside the identity directory {}", path.display(), dir.display())]
    PromptOutsideDirectory {
        /// Identity file whose prompt was resolved.
        file: PathBuf,
        /// The `system_prompt` value as written.
        path: PathBuf,
        /// Canonical identity directory.
        dir: PathBuf,
    },
    /// A repo-local identity grants more than the global identity it shadows.
    #[error("repo-local identity `{name}` widens `{field}` of its global base: {reason}")]
    WidensScope {
        /// Identity name shared by base and override.
        name: String,
        /// Dotted TOML key that widened.
        field: String,
        /// What exceeded the base.
        reason: String,
    },
    /// A repo-local identity names no global identity to narrow.
    #[error("{file}: repo-local identity `{name}` has no global identity to narrow")]
    NoBaseIdentity {
        /// The unmatched name.
        name: String,
        /// Repo-local file that declared it.
        file: PathBuf,
    },
    /// Two files in the same catalog directory declare the same name.
    #[error("duplicate identity `{name}`: declared in {} and {}", first.display(), second.display())]
    DuplicateName {
        /// The duplicated name.
        name: String,
        /// First file declaring it.
        first: PathBuf,
        /// Second file declaring it.
        second: PathBuf,
    },
    /// No global catalog directory could be derived from the environment.
    #[error("no configuration directory: set NANNA_CONFIG_DIR, XDG_CONFIG_HOME or HOME")]
    NoConfigDir,
}
