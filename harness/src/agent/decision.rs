//! Entity Modification Decision logic for the agent (ARCHITECTURE.md)
//!
//! This module is a documentation anchor for the "Entity Modification
//! Decision" node from the Harness Control Flow diagram: given the current
//! entity state, decide whether to **Query Entities (RAG)** for more context
//! or proceed to **Plan Entity Modification**.
//!
//! The actual implementation lives on [`crate::agent::AgentLoop`] as the
//! `entity_modification_decision` method, which runs the LLM-driven
//! QUERY/PROCEED prompt against the conversation history.
//!
//! Previously this module exported a free-function stub that called
//! `unimplemented!()` plus a `#[should_panic]` test that asserted the stub
//! panicked. Per AGENTS.md / TESTING.md ("Untested code is brittle. Harden
//! it, don't bend it. Never merge ... fallback parameters that shadow bad
//! or dead code, or any other graceful-degradation pattern.") that pattern
//! was deleted: a stub whose only purpose is to be tested-as-a-stub is dead
//! code that misleads readers about the architecture.
