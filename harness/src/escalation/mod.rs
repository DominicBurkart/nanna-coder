//! Escalation paths to humans.
//!
//! When an agent, the auditor, the rollout executor or the scheduler hits
//! something it cannot manage, it hands off to a human with enough context
//! to act and stops. An [`Escalation`] names the producer, the repository,
//! a summary, evidence and a suggested action; `needs-card` escalations
//! carry a proposed identity skeleton rendered from a [`CardRequest`].
//! Titles and dedupe keys are deterministic so repeats of the same problem
//! land on the same issue. Everything leaving the process is passed through
//! [`redact`] first.

mod card;
mod model;
mod redact;

pub use card::{CardRequest, MODEL_PLACEHOLDER};
pub use model::{Escalation, EscalationSource, Severity, UnknownName, HEADLINE_CHARS};
pub use redact::{marker, redact, redact_value};
