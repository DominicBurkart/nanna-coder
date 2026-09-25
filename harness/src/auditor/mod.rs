//! The internal auditor: an adversarial reviewer of every agent spawn.
//!
//! The planner proposes a [`SpawnRequest`] (an identity card plus the subtask
//! it should run). An [`Auditor`] reads it together with an [`AuditContext`]
//! (the identity catalog, the repository profile and the auditor's own inert
//! identity) and returns a [`SpawnVerdict`]: `Allow`, `Block` or `Escalate`.
//!
//! Two auditors ship here. [`RuleAuditor`] is deterministic and cheap; it is
//! what unit tests and the eval runner score. [`ModelAuditor`] asks a model
//! with an adversarial prompt, after running the rules first so that a rule
//! `Block` can never be talked out of.
//!
//! Verdicts are not advisory. [`Gate::check`] is the only way to obtain an
//! [`Allowed`] proof, and [`TaskManager::submit_spawn`](crate::task::TaskManager::submit_spawn)
//! requires one, so a blocked or escalated spawn never reaches the task
//! manager. Every verdict is appended to an [`AuditLog`] and every
//! escalation is handed to a [`SpawnEscalationHook`].

mod context;
mod record;
mod request;
mod rules;
mod verdict;

pub use context::AuditContext;
pub use record::{content_hash, AuditOutcome, AuditRecord};
pub use request::{SpawnRequest, TaskSummary};
pub use rules::{RuleAuditor, RULE_AUDITOR_NAME};
pub use verdict::{
    CardSuggestion, Reason, ReasonCode, SpawnVerdict, UnknownReasonCode, UnknownVerdictKind,
    VerdictKind,
};

use async_trait::async_trait;
use model::ModelError;
use thiserror::Error;

/// Why an audit could not produce a verdict.
#[derive(Debug, Error)]
pub enum AuditError {
    /// The auditor's own identity may act, which an auditor never should.
    #[error("auditor identity `{name}` is not inert: {reason}")]
    AuditorNotInert {
        /// The offending identity.
        name: String,
        /// Which field grants it effects.
        reason: String,
    },
    /// The model behind a [`ModelAuditor`] failed.
    #[error("auditor model call failed: {0}")]
    Model(#[from] ModelError),
    /// The audit log could not be written.
    #[error("audit log write failed: {0}")]
    Log(#[from] std::io::Error),
}

/// Reviews proposed spawns.
///
/// Implementations must treat `request.subtask` as untrusted data: it is the
/// text a compromised planner or a poisoned issue would use to steer them.
#[async_trait]
pub trait Auditor: Send + Sync {
    /// Name recorded against this auditor's verdicts.
    fn name(&self) -> &str;

    /// Decide whether `request` fits its card.
    async fn review_spawn(
        &self,
        request: &SpawnRequest,
        context: &AuditContext,
    ) -> Result<SpawnVerdict, AuditError>;

    /// [`Auditor::review_spawn`] together with the record of how the verdict
    /// was reached. The default derives the record from the request itself;
    /// model-backed auditors override it to report the real prompt hash.
    async fn audit_spawn(
        &self,
        request: &SpawnRequest,
        context: &AuditContext,
    ) -> Result<AuditOutcome, AuditError> {
        let verdict = self.review_spawn(request, context).await?;
        let record = AuditRecord::deterministic(self.name(), request, &verdict);
        Ok(AuditOutcome { verdict, record })
    }
}
