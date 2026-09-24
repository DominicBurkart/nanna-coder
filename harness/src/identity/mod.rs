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
