//! Git version control entities
//!
//! Tracks repository state, branches, commits, and file changes. Converts to TOML for model consumption.
//!
//! # Reading git state
//!
//! ```no_run
//! use harness::entities::git::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let repo = read_repository(".")?;
//! let branch = read_current_branch(".")?;
//! let commit = read_head_commit(".")?;
//! let working_dir = read_working_directory(".")?;
//!
//! println!("On branch: {}", branch.name);
//! println!("HEAD: {}", commit.title);
//! println!("Clean: {}", working_dir.is_clean());
//! # Ok(())
//! # }
//! ```
//!
//! # Converting to TOML for model
//!
//! ```no_run
//! use harness::entities::git::*;
//! use std::collections::HashMap;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let repo = read_repository(".")?;
//! let branch = read_current_branch(".")?;
//! let commit = read_head_commit(".")?;
//! let working_dir = read_working_directory(".")?;
//!
//! let toml_state = to_toml_presentation(
//!     &repo, &branch, &commit, &working_dir,
//!     &HashMap::new(), &AdditionalEntities::new()
//! );
//! let toml = to_minified_toml(&toml_state)?;
//! // TOML output includes local state, remotes, and available entities
//! # Ok(())
//! # }
//! ```
//!
//! # Branch status tracking
//!
//! ```no_run
//! use harness::entities::git::*;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let branches = read_local_branches(".")?;
//! for branch in branches {
//!     if let Some(status) = branch.tracking_status() {
//!         println!("{}: {}", branch.name, status);
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! All operations are **read-only** (no git state modification).

pub mod operations;
pub mod presentation;
pub mod types;

pub use operations::*;
pub use presentation::*;
pub use types::*;
