use super::message::MessageReach;
use super::store::StoreError;
use crate::auditor::ReasonCode;
use std::fmt::Write;
use std::time::Duration;

/// Why a hub call failed. No variant carries agent-authored text.
///
/// ```
/// use harness::hub::error::HubError;
///
/// assert_eq!(HubError::NotVisible.to_string(), "session not visible");
/// ```
#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("session not visible")]
    NotVisible,
    #[error("no active session matches that recipient")]
    NoRecipient,
    #[error("recipient is not active")]
    RecipientInactive,
    #[error("message reach {0} does not allow this recipient")]
    ReachDenied(MessageReach),
    #[error("rate limited, retry after {0:?}")]
    RateLimited(Duration),
    #[error("recipient inbox full")]
    InboxFull,
    #[error("{field} exceeds {max}")]
    TooLarge { field: &'static str, max: usize },
    #[error("thread is too deep")]
    ThreadTooDeep,
    #[error("{}", blocked_text(.0))]
    Blocked(Vec<ReasonCode>),
    #[error("message held for human review, nothing was delivered; stop and wait")]
    Quarantined { recipients: usize },
    #[error("message review unavailable, nothing was delivered; try again later")]
    AuditUnavailable,
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn advice(code: ReasonCode) -> &'static str {
    match code {
        ReasonCode::SecretLeak => "remove long random-looking tokens; refer to a pull request by its number or to a commit or branch by a shortened name",
        ReasonCode::PromptInjection => "remove instruction-like wording and unusual characters",
        ReasonCode::CrossScopeSteering => "describe the result and ask for it; do not tell the recipient which tools to run",
        _ => "state the facts plainly and shorten the message",
    }
}

fn blocked_text(codes: &[ReasonCode]) -> String {
    let mut text = String::from("message not delivered; you may rephrase and send again.");
    for code in codes {
        let _ = write!(text, " [{}] {}.", code.as_str(), advice(*code));
    }
    text
}
