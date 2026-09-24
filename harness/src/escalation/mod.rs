//! Escalation paths to humans.
//!
//! When an agent, the auditor, the rollout executor or the scheduler hits
//! something it cannot manage, it hands off to a human with enough context
//! to act and stops. Everything leaving the process is passed through
//! [`redact`] first.

mod redact;

pub use redact::{marker, redact, redact_value};
