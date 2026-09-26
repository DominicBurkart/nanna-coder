//! Provenance for a verdict: which auditor produced it from what prompt.

use super::{SpawnRequest, SpawnVerdict};
use serde::{Deserialize, Serialize};

/// Who decided, from what input, and why.
///
/// Attached to every logged verdict so a decision can be traced back to the
/// exact auditor model and prompt that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Model name, or the rule auditor's name for deterministic verdicts.
    pub model: String,
    /// [`content_hash`] of the prompt (or request) the verdict was derived from.
    pub prompt_hash: String,
    /// The auditor's explanation.
    pub rationale: String,
}

impl AuditRecord {
    /// The record for a verdict derived deterministically from `request`.
    pub fn deterministic(auditor: &str, request: &SpawnRequest, verdict: &SpawnVerdict) -> Self {
        let serialized = serde_json::to_string(request).unwrap_or_default();
        Self {
            model: auditor.to_string(),
            prompt_hash: content_hash(&serialized),
            rationale: verdict.rationale(),
        }
    }
}

/// A verdict together with its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditOutcome {
    /// The decision.
    pub verdict: SpawnVerdict,
    /// How it was reached.
    pub record: AuditRecord,
}

/// A stable 64-bit FNV-1a digest of `text`, as 16 lowercase hex digits.
///
/// Used to identify prompts in audit logs; it is not a cryptographic hash.
///
/// ```
/// use harness::auditor::content_hash;
///
/// assert_eq!(content_hash(""), "cbf29ce484222325");
/// assert_eq!(content_hash("a"), "af63dc4c8601ec8c");
/// assert_ne!(content_hash("prompt v1"), content_hash("prompt v2"));
/// ```
pub fn content_hash(text: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let digest = text.bytes().fold(OFFSET, |acc, byte| {
        (acc ^ u64::from(byte)).wrapping_mul(PRIME)
    });
    format!("{digest:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::{Reason, ReasonCode};
    use crate::effects::EffectClass;
    use crate::identity::DevLoop;
    use proptest::prelude::*;

    #[test]
    fn deterministic_record_hashes_the_request_and_names_the_auditor() {
        let request = crate::auditor::request::tests::request(
            "rust-implementer",
            "Add a test",
            DevLoop::Inner,
            EffectClass::Workspace,
        );
        let verdict = SpawnVerdict::block(vec![Reason::new(ReasonCode::Other, "why")]);
        let record = AuditRecord::deterministic("rule-auditor", &request, &verdict);
        assert_eq!(record.model, "rule-auditor");
        assert_eq!(record.rationale, "other: why");
        assert_eq!(
            record.prompt_hash,
            content_hash(&serde_json::to_string(&request).unwrap())
        );
        let again = AuditRecord::deterministic("rule-auditor", &request, &verdict);
        assert_eq!(record, again);
    }

    #[test]
    fn outcome_round_trips_through_json() {
        let outcome = AuditOutcome {
            verdict: SpawnVerdict::Allow,
            record: AuditRecord {
                model: "m".to_string(),
                prompt_hash: content_hash("p"),
                rationale: "allowed".to_string(),
            },
        };
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(
            serde_json::from_str::<AuditOutcome>(&json).unwrap(),
            outcome
        );
    }

    proptest! {
        #[test]
        fn hash_is_sixteen_hex_digits_and_deterministic(text in ".{0,64}") {
            let hash = content_hash(&text);
            prop_assert_eq!(hash.len(), 16);
            prop_assert!(hash.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            prop_assert_eq!(hash, content_hash(&text));
        }
    }
}
