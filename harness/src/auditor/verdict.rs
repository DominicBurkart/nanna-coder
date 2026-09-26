//! The auditor's answer: allow, block, or escalate to a human.

use crate::effects::EffectClass;
use crate::identity::{AgentIdentity, DevLoop};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Why a spawn was refused or escalated.
///
/// ```
/// use harness::auditor::ReasonCode;
///
/// assert_eq!(ReasonCode::LoopMismatch.as_str(), "loop_mismatch");
/// assert_eq!("prompt_injection".parse::<ReasonCode>(), Ok(ReasonCode::PromptInjection));
/// assert!("shrug".parse::<ReasonCode>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    /// The subtask needs effects beyond what the identity may reach.
    ScopeCreep,
    /// The request places the identity in a loop it does not act in.
    LoopMismatch,
    /// The subtask text tries to steer the agent or the auditor.
    PromptInjection,
    /// The identity is not in the catalog.
    UnknownIdentity,
    /// The requested effect exceeds the identity's `scope.max_effect`.
    EffectAboveCeiling,
    /// Anything else, described in the reason detail.
    Other,
}

impl ReasonCode {
    /// Every code, in declaration order.
    pub const ALL: [ReasonCode; 6] = [
        ReasonCode::ScopeCreep,
        ReasonCode::LoopMismatch,
        ReasonCode::PromptInjection,
        ReasonCode::UnknownIdentity,
        ReasonCode::EffectAboveCeiling,
        ReasonCode::Other,
    ];

    /// Stable snake_case name, identical to the serde representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReasonCode::ScopeCreep => "scope_creep",
            ReasonCode::LoopMismatch => "loop_mismatch",
            ReasonCode::PromptInjection => "prompt_injection",
            ReasonCode::UnknownIdentity => "unknown_identity",
            ReasonCode::EffectAboveCeiling => "effect_above_ceiling",
            ReasonCode::Other => "other",
        }
    }
}

impl fmt::Display for ReasonCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a string that names no [`ReasonCode`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown reason code `{0}`")]
pub struct UnknownReasonCode(pub String);

impl FromStr for ReasonCode {
    type Err = UnknownReasonCode;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ReasonCode::ALL
            .into_iter()
            .find(|code| code.as_str() == s)
            .ok_or_else(|| UnknownReasonCode(s.to_string()))
    }
}

/// One finding: a code plus the evidence behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    /// Category of the finding.
    pub code: ReasonCode,
    /// Human-readable evidence.
    pub detail: String,
}

impl Reason {
    /// A finding with `code` and `detail`.
    pub fn new(code: ReasonCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

/// What a new identity card would need to look like for the spawn to fit.
///
/// A plain description; rendering it as identity TOML is the escalation
/// path's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardSuggestion {
    /// Proposed card name.
    pub name: String,
    /// Loop the card would act in.
    pub dev_loop: DevLoop,
    /// Ceiling the card would need.
    pub max_effect: EffectClass,
    /// Tool patterns the card would need.
    pub tools: Vec<String>,
    /// Why the existing catalog does not fit.
    pub rationale: String,
}

impl CardSuggestion {
    /// Derive a suggestion from the card the planner tried to play, widened
    /// to `max_effect` and placed in `dev_loop`.
    pub fn widen(
        base: &AgentIdentity,
        dev_loop: DevLoop,
        max_effect: EffectClass,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            name: format!("{}-{}", base.name(), max_effect),
            dev_loop,
            max_effect,
            tools: base.scope.tools.iter().map(ToString::to_string).collect(),
            rationale: rationale.into(),
        }
    }
}

/// The three possible outcomes, without their payloads.
///
/// ```
/// use harness::auditor::VerdictKind;
///
/// assert_eq!("escalate".parse::<VerdictKind>(), Ok(VerdictKind::Escalate));
/// assert_eq!(VerdictKind::Block.to_string(), "block");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    /// The spawn may proceed.
    Allow,
    /// The spawn must not proceed.
    Block,
    /// No existing card fits; a human must decide.
    Escalate,
}

impl VerdictKind {
    /// Every kind, in declaration order.
    pub const ALL: [VerdictKind; 3] = [
        VerdictKind::Allow,
        VerdictKind::Block,
        VerdictKind::Escalate,
    ];

    /// Stable snake_case name, identical to the serde representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            VerdictKind::Allow => "allow",
            VerdictKind::Block => "block",
            VerdictKind::Escalate => "escalate",
        }
    }
}

impl fmt::Display for VerdictKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a string that names no [`VerdictKind`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown verdict `{0}`; expected one of allow, block, escalate")]
pub struct UnknownVerdictKind(pub String);

impl FromStr for VerdictKind {
    type Err = UnknownVerdictKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        VerdictKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| UnknownVerdictKind(s.to_string()))
    }
}

/// The auditor's decision on a [`SpawnRequest`](super::SpawnRequest).
///
/// Serialised with a `verdict` tag, which is also the JSON shape a model
/// auditor is asked to produce:
///
/// ```
/// use harness::auditor::{Reason, ReasonCode, SpawnVerdict, VerdictKind};
///
/// let block = SpawnVerdict::block(vec![Reason::new(ReasonCode::LoopMismatch, "inner card asked to deploy")]);
/// let json = serde_json::to_value(&block).unwrap();
/// assert_eq!(json["verdict"], "block");
/// assert_eq!(json["reasons"][0]["code"], "loop_mismatch");
/// assert_eq!(block.kind(), VerdictKind::Block);
/// assert!(!block.is_allow());
///
/// let allow: SpawnVerdict = serde_json::from_str(r#"{"verdict":"allow"}"#).unwrap();
/// assert!(allow.is_allow());
/// assert!(allow.reasons().is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum SpawnVerdict {
    /// The spawn fits its card.
    Allow,
    /// The spawn must not happen.
    Block {
        /// Why.
        reasons: Vec<Reason>,
    },
    /// No card in the catalog fits; the human should author one.
    Escalate {
        /// Why.
        reasons: Vec<Reason>,
        /// What the missing card would look like.
        suggested_identity_change: CardSuggestion,
    },
}

impl SpawnVerdict {
    /// A block with `reasons`.
    pub fn block(reasons: Vec<Reason>) -> Self {
        SpawnVerdict::Block { reasons }
    }

    /// An escalation with `reasons` and a suggested card.
    pub fn escalate(reasons: Vec<Reason>, suggested_identity_change: CardSuggestion) -> Self {
        SpawnVerdict::Escalate {
            reasons,
            suggested_identity_change,
        }
    }

    /// Which of the three outcomes this is.
    pub fn kind(&self) -> VerdictKind {
        match self {
            SpawnVerdict::Allow => VerdictKind::Allow,
            SpawnVerdict::Block { .. } => VerdictKind::Block,
            SpawnVerdict::Escalate { .. } => VerdictKind::Escalate,
        }
    }

    /// Whether the spawn may proceed.
    pub fn is_allow(&self) -> bool {
        matches!(self, SpawnVerdict::Allow)
    }

    /// The findings behind a refusal; empty for `Allow`.
    pub fn reasons(&self) -> &[Reason] {
        match self {
            SpawnVerdict::Allow => &[],
            SpawnVerdict::Block { reasons } | SpawnVerdict::Escalate { reasons, .. } => reasons,
        }
    }

    /// The suggested card, for an escalation.
    pub fn suggestion(&self) -> Option<&CardSuggestion> {
        match self {
            SpawnVerdict::Escalate {
                suggested_identity_change,
                ..
            } => Some(suggested_identity_change),
            _ => None,
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

impl fmt::Display for SpawnVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind(), self.rationale())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn suggestion() -> CardSuggestion {
        CardSuggestion::widen(
            &crate::identity::example(),
            DevLoop::Outer,
            EffectClass::Sandbox,
            "needs a deploy card",
        )
    }

    #[test]
    fn reason_codes_round_trip_through_strings_and_serde() {
        for code in ReasonCode::ALL {
            assert_eq!(code.as_str().parse::<ReasonCode>(), Ok(code));
            assert_eq!(code.to_string(), code.as_str());
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
            assert_eq!(serde_json::from_str::<ReasonCode>(&json).unwrap(), code);
        }
        assert_eq!(
            "nope".parse::<ReasonCode>(),
            Err(UnknownReasonCode("nope".to_string()))
        );
        assert_eq!(
            UnknownReasonCode("nope".to_string()).to_string(),
            "unknown reason code `nope`"
        );
    }

    #[test]
    fn verdict_kinds_round_trip_through_strings_and_serde() {
        for kind in VerdictKind::ALL {
            assert_eq!(kind.as_str().parse::<VerdictKind>(), Ok(kind));
            assert_eq!(kind.to_string(), kind.as_str());
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.as_str());
        }
        let err = "maybe".parse::<VerdictKind>().unwrap_err();
        assert_eq!(err, UnknownVerdictKind("maybe".to_string()));
        assert!(err.to_string().contains("allow, block, escalate"));
    }

    #[test]
    fn reason_displays_code_and_detail() {
        let reason = Reason::new(ReasonCode::Other, "because");
        assert_eq!(reason.to_string(), "other: because");
    }

    #[test]
    fn widen_derives_a_card_from_the_base_identity() {
        let card = suggestion();
        assert_eq!(card.name, "rust-implementer-sandbox");
        assert_eq!(card.dev_loop, DevLoop::Outer);
        assert_eq!(card.max_effect, EffectClass::Sandbox);
        assert_eq!(
            card.tools,
            vec!["read_file", "write_file", "search", "cargo_*", "git_*"]
        );
        assert_eq!(card.rationale, "needs a deploy card");
    }

    #[test]
    fn allow_has_no_reasons_or_suggestion() {
        let allow = SpawnVerdict::Allow;
        assert_eq!(allow.kind(), VerdictKind::Allow);
        assert!(allow.reasons().is_empty());
        assert!(allow.suggestion().is_none());
        assert_eq!(allow.rationale(), "allowed");
        assert_eq!(allow.to_string(), "allow: allowed");
        assert_eq!(
            serde_json::to_value(&allow).unwrap(),
            serde_json::json!({"verdict": "allow"})
        );
    }

    #[test]
    fn block_carries_its_reasons() {
        let reasons = vec![
            Reason::new(ReasonCode::UnknownIdentity, "no card `ghost`"),
            Reason::new(ReasonCode::Other, "x"),
        ];
        let block = SpawnVerdict::block(reasons.clone());
        assert_eq!(block.kind(), VerdictKind::Block);
        assert_eq!(block.reasons(), &reasons[..]);
        assert!(block.suggestion().is_none());
        assert_eq!(
            block.rationale(),
            "unknown_identity: no card `ghost`; other: x"
        );
        assert!(block.to_string().starts_with("block: unknown_identity"));
    }

    #[test]
    fn escalate_carries_reasons_and_a_suggestion() {
        let reasons = vec![Reason::new(
            ReasonCode::EffectAboveCeiling,
            "sandbox > repository",
        )];
        let escalate = SpawnVerdict::escalate(reasons.clone(), suggestion());
        assert_eq!(escalate.kind(), VerdictKind::Escalate);
        assert_eq!(escalate.reasons(), &reasons[..]);
        assert_eq!(escalate.suggestion(), Some(&suggestion()));
        let json = serde_json::to_value(&escalate).unwrap();
        assert_eq!(json["verdict"], "escalate");
        assert_eq!(json["suggested_identity_change"]["max_effect"], "sandbox");
        assert_eq!(json["suggested_identity_change"]["dev_loop"], "outer");
        let parsed: SpawnVerdict = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, escalate);
    }

    #[test]
    fn verdict_json_requires_a_known_tag() {
        assert!(serde_json::from_str::<SpawnVerdict>(r#"{"verdict":"maybe"}"#).is_err());
        assert!(serde_json::from_str::<SpawnVerdict>(r#"{"verdict":"block"}"#).is_err());
        assert!(serde_json::from_str::<SpawnVerdict>(
            r#"{"verdict":"block","reasons":[{"code":"nope","detail":""}]}"#
        )
        .is_err());
    }

    fn arb_code() -> impl Strategy<Value = ReasonCode> {
        prop::sample::select(ReasonCode::ALL.to_vec())
    }

    fn arb_reason() -> impl Strategy<Value = Reason> {
        (arb_code(), "[a-z ]{0,20}").prop_map(|(code, detail)| Reason::new(code, detail))
    }

    fn arb_verdict() -> impl Strategy<Value = SpawnVerdict> {
        prop_oneof![
            Just(SpawnVerdict::Allow),
            prop::collection::vec(arb_reason(), 0..4).prop_map(SpawnVerdict::block),
            prop::collection::vec(arb_reason(), 0..4)
                .prop_map(|reasons| SpawnVerdict::escalate(reasons, suggestion())),
        ]
    }

    proptest! {
        #[test]
        fn any_verdict_round_trips_through_json(verdict in arb_verdict()) {
            let json = serde_json::to_string(&verdict).unwrap();
            let parsed: SpawnVerdict = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(&parsed, &verdict);
            prop_assert_eq!(parsed.kind(), verdict.kind());
            prop_assert_eq!(verdict.is_allow(), verdict.reasons().is_empty() && verdict.suggestion().is_none() && verdict.kind() == VerdictKind::Allow);
        }
    }
}
