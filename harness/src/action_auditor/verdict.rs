//! The action auditor's answer: allow, block, or escalate.

use crate::auditor::{Reason, VerdictKind};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The [`ActionAuditor`](super::ActionAuditor)'s decision on an
/// [`ActionReview`](super::ActionReview).
///
/// Shares [`VerdictKind`] and [`Reason`] with [`SpawnVerdict`](crate::auditor::SpawnVerdict);
/// there is no [`CardSuggestion`](crate::auditor::CardSuggestion) analogue
/// because an action denial names a tool call, not a missing identity card.
///
/// ```
/// use harness::action_auditor::ActionVerdict;
/// use harness::auditor::{Reason, ReasonCode, VerdictKind};
///
/// let block = ActionVerdict::block(vec![Reason::new(ReasonCode::WindowClosed, "no window open")]);
/// assert_eq!(block.kind(), VerdictKind::Block);
/// assert!(!block.is_allow());
///
/// let json = serde_json::to_value(&block).unwrap();
/// assert_eq!(json["verdict"], "block");
/// let allow: ActionVerdict = serde_json::from_str(r#"{"verdict":"allow"}"#).unwrap();
/// assert!(allow.is_allow());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum ActionVerdict {
    /// The action may run.
    Allow,
    /// The action must not run; the caller may adapt and try something else.
    Block {
        /// Why.
        reasons: Vec<Reason>,
    },
    /// The action must not run and the task should halt for human review.
    Escalate {
        /// Why.
        reasons: Vec<Reason>,
    },
}

impl ActionVerdict {
    /// A block with `reasons`.
    pub fn block(reasons: Vec<Reason>) -> Self {
        ActionVerdict::Block { reasons }
    }

    /// An escalation with `reasons`.
    pub fn escalate(reasons: Vec<Reason>) -> Self {
        ActionVerdict::Escalate { reasons }
    }

    /// Which of the three outcomes this is.
    pub fn kind(&self) -> VerdictKind {
        match self {
            ActionVerdict::Allow => VerdictKind::Allow,
            ActionVerdict::Block { .. } => VerdictKind::Block,
            ActionVerdict::Escalate { .. } => VerdictKind::Escalate,
        }
    }

    /// Whether the action may run.
    pub fn is_allow(&self) -> bool {
        matches!(self, ActionVerdict::Allow)
    }

    /// The findings behind a refusal; empty for `Allow`.
    pub fn reasons(&self) -> &[Reason] {
        match self {
            ActionVerdict::Allow => &[],
            ActionVerdict::Block { reasons } | ActionVerdict::Escalate { reasons } => reasons,
        }
    }

    /// The reasons joined into one line, or `allowed`.
    pub fn rationale(&self) -> String {
        if self.is_allow() {
            return "allowed".to_string();
        }
        self.reasons()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

impl fmt::Display for ActionVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind(), self.rationale())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::ReasonCode;
    use proptest::prelude::*;

    #[test]
    fn allow_has_no_reasons() {
        let allow = ActionVerdict::Allow;
        assert_eq!(allow.kind(), VerdictKind::Allow);
        assert!(allow.reasons().is_empty());
        assert_eq!(allow.rationale(), "allowed");
        assert_eq!(allow.to_string(), "allow: allowed");
        assert_eq!(
            serde_json::to_value(&allow).unwrap(),
            serde_json::json!({"verdict": "allow"})
        );
    }

    #[test]
    fn block_carries_its_reasons() {
        let reasons = vec![Reason::new(ReasonCode::LeaseUnavailable, "held by other")];
        let block = ActionVerdict::block(reasons.clone());
        assert_eq!(block.kind(), VerdictKind::Block);
        assert_eq!(block.reasons(), &reasons[..]);
        assert!(block.to_string().starts_with("block: lease_unavailable"));
    }

    #[test]
    fn escalate_carries_its_reasons() {
        let reasons = vec![Reason::new(ReasonCode::RepeatedDenials, "3rd denial")];
        let escalate = ActionVerdict::escalate(reasons.clone());
        assert_eq!(escalate.kind(), VerdictKind::Escalate);
        assert_eq!(escalate.reasons(), &reasons[..]);
        let json = serde_json::to_value(&escalate).unwrap();
        assert_eq!(json["verdict"], "escalate");
        let parsed: ActionVerdict = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, escalate);
    }

    #[test]
    fn verdict_json_requires_a_known_tag() {
        assert!(serde_json::from_str::<ActionVerdict>(r#"{"verdict":"maybe"}"#).is_err());
        assert!(serde_json::from_str::<ActionVerdict>(r#"{"verdict":"block"}"#).is_err());
    }

    fn arb_code() -> impl Strategy<Value = ReasonCode> {
        prop::sample::select(ReasonCode::ALL.to_vec())
    }

    fn arb_reason() -> impl Strategy<Value = Reason> {
        (arb_code(), "[a-z ]{0,20}").prop_map(|(code, detail)| Reason::new(code, detail))
    }

    fn arb_verdict() -> impl Strategy<Value = ActionVerdict> {
        prop_oneof![
            Just(ActionVerdict::Allow),
            prop::collection::vec(arb_reason(), 0..4).prop_map(ActionVerdict::block),
            prop::collection::vec(arb_reason(), 0..4).prop_map(ActionVerdict::escalate),
        ]
    }

    proptest! {
        #[test]
        fn any_verdict_round_trips_through_json(verdict in arb_verdict()) {
            let json = serde_json::to_string(&verdict).unwrap();
            let parsed: ActionVerdict = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&parsed, &verdict);
            prop_assert_eq!(parsed.kind(), verdict.kind());
            prop_assert_eq!(verdict.is_allow(), verdict.reasons().is_empty() && verdict.kind() == VerdictKind::Allow);
        }
    }
}
