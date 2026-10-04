//! What a tool dispatch site asks the action auditor to review.

use crate::effects::EffectClass;
use crate::task::TaskId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One effectful tool call, reduced to what an [`ActionAuditor`](super::ActionAuditor)
/// needs to decide whether it may run.
///
/// `args` is exactly what the model sent to the tool; every implementation
/// must treat it as untrusted data rather than as instructions.
///
/// ```
/// use harness::action_auditor::ActionReview;
/// use harness::effects::EffectClass;
/// use harness::task::TaskId;
///
/// let review = ActionReview {
///     identity: "rust-implementer".to_string(),
///     task_id: TaskId("task-7".to_string()),
///     tool: "github_pr_status".to_string(),
///     args: serde_json::json!({}),
///     effect_class: EffectClass::Repository,
///     prior_actions: vec![EffectClass::Workspace],
/// };
/// let json = serde_json::to_value(&review).unwrap();
/// assert_eq!(json["effect_class"], "repository");
/// assert_eq!(json["prior_actions"][0], "workspace");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionReview {
    /// Name of the identity attempting the call, or
    /// [`UNSCOPED_IDENTITY`](crate::scope::UNSCOPED_IDENTITY) when the
    /// registry carries none.
    pub identity: String,
    /// The task the call belongs to.
    pub task_id: TaskId,
    /// Name of the tool being called.
    pub tool: String,
    /// The arguments the model supplied.
    pub args: Value,
    /// Blast radius the tool declares for this call.
    pub effect_class: EffectClass,
    /// Effect classes of every action already reviewed for this task, in
    /// call order.
    pub prior_actions: Vec<EffectClass>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_round_trips_through_json() {
        let review = ActionReview {
            identity: "deployer".to_string(),
            task_id: TaskId("t1".to_string()),
            tool: "sandbox_deploy".to_string(),
            args: serde_json::json!({"env": "staging"}),
            effect_class: EffectClass::Sandbox,
            prior_actions: vec![EffectClass::Repository, EffectClass::Ci],
        };
        let json = serde_json::to_string(&review).unwrap();
        let parsed: ActionReview = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, review);
    }
}
