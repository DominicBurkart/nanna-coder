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
//! is deterministic: it always allows `None`/`Workspace` calls (the inner
//! loop is container-isolated and needs no gate), checks the identity's
//! effect ceiling for `Repository`/`Ci` calls, and for `Sandbox`/`Production`
//! calls checks the action's availability window and coordination lease
//! before ever considering an allow. [`ModelActionAuditor`] wraps it,
//! consulting the strongest configured model for `Sandbox`/`Production`
//! calls whose window and lease checks already passed; a rule `Block` always
//! short-circuits the model.
//!
//! This module reuses [`crate::protected::AuditHook`] nowhere: that trait
//! exists to notify about protected-path *writes* a workspace already
//! refused, a single narrow callback with no verdict to compute or log. An
//! action review needs a request/response shape (`ActionReview` in,
//! `ActionVerdict` out, every call logged), so it gets its own trait rather
//! than being bent to fit `AuditHook`.

mod context;
mod gate;
mod llm;
mod log;
mod review;
mod rules;
mod verdict;

pub use context::ActionContext;
pub use gate::{ActionDenied, ActionGate};
pub use llm::ModelActionAuditor;
pub use log::{ActionAuditLog, ActionAuditLogEntry};
pub use review::ActionReview;
pub use rules::RuleActionAuditor;
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
