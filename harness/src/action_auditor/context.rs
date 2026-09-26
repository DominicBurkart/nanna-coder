//! The read-only view an action auditor gets of the world.

use crate::effects::EffectClass;
use crate::leases::LeaseContext;
use chrono::{DateTime, Utc};

/// Everything an [`ActionAuditor`](super::ActionAuditor) may consult beyond
/// the [`ActionReview`](super::ActionReview) itself: the identity's effect
/// ceiling, the availability window the action targets, the coordination
/// lease context it would need, and the instant to evaluate both against.
///
/// ```
/// use harness::action_auditor::ActionContext;
/// use harness::effects::EffectClass;
/// use harness::leases::LeaseContext;
///
/// let ctx = ActionContext {
///     max_effect: EffectClass::Sandbox,
///     window: Some("business-hours"),
///     lease: LeaseContext { repo: "example/repo", pr: Some(7), ..LeaseContext::default() },
///     now: chrono::Utc::now(),
/// };
/// assert_eq!(ctx.max_effect, EffectClass::Sandbox);
/// assert_eq!(ctx.window, Some("business-hours"));
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ActionContext<'a> {
    /// The widest effect class the calling identity may reach.
    pub max_effect: EffectClass,
    /// Name of the availability window the action targets, when one is
    /// configured for it.
    pub window: Option<&'a str>,
    /// What the action would touch, for deriving the coordination leases it
    /// needs.
    pub lease: LeaseContext<'a>,
    /// The instant window and lease checks are evaluated at.
    pub now: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_carries_the_fields_it_was_built_with() {
        let now = Utc::now();
        let lease = LeaseContext {
            repo: "example/repo",
            branch: Some("main"),
            ..LeaseContext::default()
        };
        let ctx = ActionContext {
            max_effect: EffectClass::Repository,
            window: None,
            lease,
            now,
        };
        assert_eq!(ctx.max_effect, EffectClass::Repository);
        assert_eq!(ctx.window, None);
        assert_eq!(ctx.lease.repo, "example/repo");
        assert_eq!(ctx.now, now);
    }
}
