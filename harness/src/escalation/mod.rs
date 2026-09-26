//! Escalation paths to humans.
//!
//! When an agent, the auditor, the rollout executor or the scheduler hits
//! something it cannot manage, it hands off to a human with enough context
//! to act and stops. An [`Escalation`] names the producer, the repository,
//! a summary, evidence and a suggested action; `needs-card` escalations
//! carry a proposed identity skeleton rendered from a [`CardRequest`].
//! Titles and dedupe keys are deterministic so repeats of the same problem
//! land on the same issue: the [`GithubIssueSink`] comments on the open
//! issue instead of filing a duplicate. A [`WebhookSink`] posts the JSON
//! form, and a [`FanoutSink`] delivers to several sinks at once. The
//! [`Escalator`] is the single entry point: it records every occurrence in
//! the [`EscalationLog`] (JSON Lines next to the queue and lease logs),
//! collapses repeats inside a window into a counter, and for `incident`
//! severity sets an [`IncidentHold`] that parks production-class work for
//! the repository until a human runs `nanna escalation resolve <id>`.
//! Everything leaving the process is passed through [`redact`] first.

mod card;
mod escalator;
mod github;
mod log;
mod model;
mod redact;
mod sink;
mod webhook;

pub use card::{CardRequest, MODEL_PLACEHOLDER};
pub use escalator::{default_window, EscalationOutcome, Escalator};
pub use github::{GithubIssueSink, ESCALATION_LABEL};
pub use log::{
    default_escalation_path, escalation_path_from, EscalationLog, EscalationSnapshot, IncidentHold,
    KeyState, Occurrence, ESCALATION_PATH_ENV,
};
pub use model::{Escalation, EscalationSource, Severity, UnknownName, HEADLINE_CHARS};
pub use redact::{marker, redact, redact_value};
pub use sink::{
    DeliveryOutcome, DeliveryReceipt, EscalationError, EscalationSink, FanoutSink, SinkFailure,
};
pub use webhook::WebhookSink;
