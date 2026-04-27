//! Testing & Analysis Entities
//!
//! Test-result and static-analysis entities for tracking code quality
//! metrics and test outcomes. The current `TestEntity` schema is a
//! minimal `EntityMetadata` wrapper; richer fields will be added as
//! the test/analysis pipeline lands.
//!
//! Tracked in issue #24.

pub mod types;

pub use types::*;
