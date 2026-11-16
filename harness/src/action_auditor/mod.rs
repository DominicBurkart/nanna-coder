//! The second auditor gate: adversarial review of every effectful action.
//!
//! [`crate::auditor`] gates *who gets spawned*; this module gates *what a
//! spawned agent then does*. Every tool call whose
//! [`Tool::effect_class`](crate::tools::Tool::effect_class) is at least
//! [`EffectClass::Repository`](crate::effects::EffectClass::Repository) is
//! reduced to an [`ActionReview`] and handed to an [`ActionAuditor`], which
//! returns an [`ActionVerdict`]. [`ActionGate::run_gate`] is the only place
//! that verdict is produced and logged; [`crate::tools::ToolRegistry::execute`]
//! is the only place it is consulted, so the check happens once, in the
//! shared dispatch path, rather than once per tool implementation.
//!
//! Two auditors ship here, mirroring [`crate::auditor`]. [`RuleActionAuditor`]
//! is deterministic: it checks the identity's effect ceiling for every class,
//! allows `None`/`Workspace` calls within it (the inner loop is
//! container-isolated), and for `Sandbox`/`Production` calls checks the action's availability window and coordination lease. It
//! never allows a `Repository`/`Ci`/`Sandbox`/`Production` call by itself.
//! [`ModelActionAuditor`] wraps it, consulting the configured model for
//! those four classes once the rule checks passed; a rule `Block` always
//! short-circuits the model.
//!
//! This module reuses [`crate::protected::AuditHook`] nowhere: that trait
//! exists to notify about protected-path *writes* a workspace already
//! refused, a single narrow callback with no verdict to compute or log. An
//! action review needs a request/response shape (`ActionReview` in,
//! `ActionVerdict` out, every call logged), so it gets its own trait rather
//! than being bent to fit `AuditHook`.

mod commands;
mod context;
mod export;
mod fail_closed;
mod gate;
mod llm;
mod log;
mod message_auditor;
mod message_fail_closed;
mod message_model;
mod message_rules;
mod review;
mod routing;
mod rules;
mod verdict;

pub use context::ActionContext;
pub(crate) use export::plain;
pub use export::{ActionAuditEvent, ActionAuditSink, NullSink, TelemetrySink};
pub use fail_closed::{FailClosedActionAuditor, ACTION_AUDIT_TIMEOUT};
pub use gate::{ActionDenied, ActionGate};
pub use llm::ModelActionAuditor;
pub use log::{redact_for_log, ActionAuditLog, ActionAuditLogEntry};
pub use message_auditor::{
    is_privilege_asymmetric, message_content_hash, BoundVerdict, InboundReview, MessageAuditor,
    OutboundReview, RecipientScope, SharedOracle, ThreadMessage, RULE_MESSAGE_AUDITOR_NAME,
};
pub use message_fail_closed::{FailClosedMessageAuditor, MESSAGE_AUDIT_TIMEOUT};
pub use message_model::{
    ModelMessageAuditor, MESSAGE_AUDITOR_FRAMING, MESSAGE_AUDITOR_RUBRIC, MESSAGE_OUTPUT_CONTRACT,
};
pub use message_rules::{
    EntropyThresholds, IdentifierOracle, MessageVerdict, NullOracle, RuleMessageAuditor,
    RuleSignal, RulesOutcome,
};
pub use review::ActionReview;
pub use routing::{
    is_message_tool, known_arguments, OutboundRouter, RoutingActionAuditor, MESSAGE_TOOLS,
};
pub(crate) use rules::RUN_COMMAND_TOOL;
pub use rules::{RuleActionAuditor, RULE_ACTION_AUDITOR_NAME};
pub use verdict::ActionVerdict;

use async_trait::async_trait;
use model::ModelError;
use thiserror::Error;

/// Why an action review could not produce a verdict.
#[derive(Debug, Error)]
pub enum ActionAuditError {
    /// The model behind a [`ModelActionAuditor`] failed.
    #[error("action auditor model call failed: {0}")]
    Model(#[from] ModelError),
    /// The action audit log could not be written.
    #[error("action audit log write failed: {0}")]
    Log(#[from] std::io::Error),
    /// An action audit log entry could not be (de)serialized.
    #[error("action audit log entry serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
    /// The auditor did not answer within its timeout.
    #[error("action auditor did not answer within {timeout:?}")]
    Timeout {
        /// The timeout that elapsed.
        timeout: std::time::Duration,
    },
}

/// Reviews a proposed effectful tool call.
///
/// Implementations must treat `review.args` as untrusted data: it is
/// whatever the model that requested the call chose to send.
#[async_trait]
pub trait ActionAuditor: Send + Sync {
    /// Name recorded against this auditor's verdicts.
    fn name(&self) -> &str;

    /// Decide whether `review` may proceed.
    async fn review_action(
        &self,
        review: &ActionReview,
        context: &ActionContext<'_>,
    ) -> Result<ActionVerdict, ActionAuditError>;
}
