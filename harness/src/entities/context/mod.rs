//! Project Context Entities
//!
//! Conversation and project-context entities for tracking user prompts,
//! agent decisions, and per-run history (`ContextEntity`, `ToolCallRecord`).
//! These power the "Project context entity" leg of the entity-store API
//! described in ARCHITECTURE.md.
//!
//! Tracked in issue #26.

pub mod types;

pub use types::*;
