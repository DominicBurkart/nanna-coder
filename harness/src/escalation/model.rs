use super::card::CardRequest;
use super::redact::{redact, redact_value};
use crate::task::TaskId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

/// How urgently a human must act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Worth knowing; nothing is blocked.
    Info,
    /// No identity in the catalog fits; a human must author a new card.
    NeedsCard,
    /// A task stopped and cannot continue without a decision.
    Blocked,
    /// Production is affected; all production work for the repository is
    /// held until a human resolves the escalation.
    Incident,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::NeedsCard => "needs-card",
            Severity::Blocked => "blocked",
            Severity::Incident => "incident",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A severity or source name that is not recognised.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown escalation {kind} `{value}`")]
pub struct UnknownName {
    pub kind: &'static str,
    pub value: String,
}

impl FromStr for Severity {
    type Err = UnknownName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "info" => Ok(Severity::Info),
            "needs-card" => Ok(Severity::NeedsCard),
            "blocked" => Ok(Severity::Blocked),
            "incident" => Ok(Severity::Incident),
            other => Err(UnknownName {
                kind: "severity",
                value: other.to_string(),
            }),
        }
    }
}

/// Which producer raised the escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EscalationSource {
    /// An auditor `Escalate` verdict on a spawn or an effectful action.
    Auditor,
    /// The rollout executor's `halt-and-escalate` path.
    Rollout,
    /// A task exhausted its iteration, token or wall-clock budget.
    Budget,
    /// Repeated scope denials for one identity.
    ScopeDenials,
    /// An incident postmortem or a remediation the incident agent could not finish.
    Incident,
    /// Raised by a human or an operator script.
    Manual,
}

impl EscalationSource {
    fn label(self) -> &'static str {
        match self {
            EscalationSource::Auditor => "auditor",
            EscalationSource::Rollout => "rollout",
            EscalationSource::Budget => "budget",
            EscalationSource::ScopeDenials => "scope-denials",
            EscalationSource::Incident => "incident",
            EscalationSource::Manual => "manual",
        }
    }
}

impl fmt::Display for EscalationSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for EscalationSource {
    type Err = UnknownName;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "auditor" => Ok(EscalationSource::Auditor),
            "rollout" => Ok(EscalationSource::Rollout),
            "budget" => Ok(EscalationSource::Budget),
            "scope-denials" => Ok(EscalationSource::ScopeDenials),
            "incident" => Ok(EscalationSource::Incident),
            "manual" => Ok(EscalationSource::Manual),
            other => Err(UnknownName {
                kind: "source",
                value: other.to_string(),
            }),
        }
    }
}

/// Longest headline (in characters) taken from the summary for the title.
pub const HEADLINE_CHARS: usize = 72;

/// A hand-off to a human: what happened, where, the evidence, and what the
/// producer suggests doing about it.
///
/// Repeats of the same problem map to the same [`title`](Self::title) and
/// [`dedupe_key`](Self::dedupe_key), which depend only on `source`, `repo`
/// and `summary`; `occurrence` counts how many times this key has been seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    /// Unique id of this occurrence.
    pub id: String,
    pub severity: Severity,
    pub source: EscalationSource,
    /// Task that raised it, if any.
    pub task_id: Option<TaskId>,
    /// Identity the task was running as, if any.
    pub identity: Option<String>,
    /// Target repository in `owner/name` form; the GitHub sink files there.
    pub repo: String,
    /// One-paragraph description of the problem. Its first line becomes the
    /// title headline.
    pub summary: String,
    /// Log lines, verdict rationales, audit records.
    pub evidence: Vec<String>,
    /// What the producer suggests the human does.
    pub suggested_action: String,
    /// Identity card skeleton for `needs-card` escalations.
    pub proposed_identity_toml: Option<String>,
    /// 1 for the first occurrence of this key, 2 for the next, and so on.
    pub occurrence: u64,
    pub created_at: DateTime<Utc>,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(parts: &[&str]) -> u64 {
    let mut hash = FNV_OFFSET;
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hash = (hash ^ u64::from(b'\n')).wrapping_mul(FNV_PRIME);
        }
        for byte in part.bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

impl Escalation {
    /// A fresh escalation with a random id, raised now, occurrence 1, no
    /// evidence and no suggested action.
    pub fn new(
        severity: Severity,
        source: EscalationSource,
        repo: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            severity,
            source,
            task_id: None,
            identity: None,
            repo: repo.into(),
            summary: summary.into(),
            evidence: Vec::new(),
            suggested_action: String::new(),
            proposed_identity_toml: None,
            occurrence: 1,
            created_at: Utc::now(),
        }
    }

    /// A `needs-card` escalation carrying the identity skeleton rendered
    /// from `request`, so the human can copy it into the catalog.
    pub fn needs_card(
        source: EscalationSource,
        repo: impl Into<String>,
        summary: impl Into<String>,
        request: &CardRequest,
    ) -> Self {
        let action = format!("Review the proposed identity `{}` below, adjust its model and limits, and add it to the identity catalog; then re-run the task.", request.name);
        Self::new(Severity::NeedsCard, source, repo, summary)
            .with_suggested_action(action)
            .with_proposed_identity(request.to_toml())
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    pub fn with_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn with_identity(mut self, identity: impl Into<String>) -> Self {
        self.identity = Some(identity.into());
        self
    }

    pub fn with_evidence(mut self, evidence: Vec<String>) -> Self {
        self.evidence = evidence;
        self
    }

    pub fn with_suggested_action(mut self, action: impl Into<String>) -> Self {
        self.suggested_action = action.into();
        self
    }

    pub fn with_proposed_identity(mut self, toml: impl Into<String>) -> Self {
        self.proposed_identity_toml = Some(toml.into());
        self
    }

    pub fn with_occurrence(mut self, occurrence: u64) -> Self {
        self.occurrence = occurrence;
        self
    }

    pub fn at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self
    }

    fn hash(&self) -> String {
        format!(
            "{:016x}",
            fnv1a(&[self.source.label(), &self.repo, &self.summary])
        )
    }

    /// Stable key for rate limiting: `<source>:<repo>:<hash>` where the hash
    /// is FNV-1a over source, repo and summary, so it survives restarts.
    ///
    /// ```
    /// use harness::escalation::{Escalation, EscalationSource, Severity};
    ///
    /// let a = Escalation::new(Severity::Blocked, EscalationSource::Auditor, "example/repo", "no card fits");
    /// let b = Escalation::new(Severity::Info, EscalationSource::Auditor, "example/repo", "no card fits");
    /// assert_eq!(a.dedupe_key(), b.dedupe_key());
    /// assert!(a.dedupe_key().starts_with("auditor:example/repo:"));
    /// ```
    pub fn dedupe_key(&self) -> String {
        format!("{}:{}:{}", self.source, self.repo, self.hash())
    }

    /// First line of the summary, redacted and cut to [`HEADLINE_CHARS`]
    /// characters.
    ///
    /// Redaction runs before truncation: cutting first could split a secret
    /// so neither half is long enough to match a pattern, leaking a
    /// fragment. This is itself an outgoing string (used directly to build
    /// [`crate::escalation::IncidentHold`], and by every caller of
    /// [`Escalation::title`]), so it carries the same "redacted before it
    /// leaves" guarantee `title` and `body` do.
    pub fn headline(&self) -> String {
        let redacted = redact(&self.summary);
        let line = redacted.lines().next().unwrap_or("").trim();
        let mut head: String = line.chars().take(HEADLINE_CHARS).collect();
        if line.chars().count() > HEADLINE_CHARS {
            head.push('…');
        }
        head
    }

    /// Deterministic issue title: `[nanna-escalation] <source> on <repo>:
    /// <headline> (<hash>)`. Two escalations with the same source, repo and
    /// summary get the same title whatever their id, severity or time, so
    /// the GitHub sink can find the issue a repeat belongs to. The title
    /// is redacted like every other outgoing text.
    ///
    /// ```
    /// use harness::escalation::{Escalation, EscalationSource, Severity};
    ///
    /// let first = Escalation::new(Severity::Blocked, EscalationSource::Rollout, "example/repo", "health breach at step 3\ndetails");
    /// let repeat = Escalation::new(Severity::Incident, EscalationSource::Rollout, "example/repo", "health breach at step 3\ndetails");
    /// assert_eq!(first.title(), repeat.title());
    /// assert!(first.title().starts_with("[nanna-escalation] rollout on example/repo: health breach at step 3 ("));
    ///
    /// let other = Escalation::new(Severity::Blocked, EscalationSource::Rollout, "example/repo", "health breach at step 4");
    /// assert_ne!(first.title(), other.title());
    /// ```
    pub fn title(&self) -> String {
        redact(&format!(
            "[nanna-escalation] {} on {}: {} ({})",
            self.source,
            self.repo,
            self.headline(),
            self.hash()
        ))
    }

    /// Markdown body for issues and comments, redacted.
    pub fn body(&self) -> String {
        let task = self
            .task_id
            .as_ref()
            .map_or_else(|| "none".to_string(), ToString::to_string);
        let identity = self.identity.as_deref().unwrap_or("none");
        let mut out = format!(
            "**Severity:** {}\n**Source:** {}\n**Repository:** {}\n**Task:** {task}\n**Identity:** {identity}\n**Occurrence:** {}\n**Raised:** {}\n**Id:** {}\n\n## Summary\n\n{}\n\n## Evidence\n\n",
            self.severity, self.source, self.repo, self.occurrence, self.created_at.to_rfc3339(), self.id, self.summary.trim()
        );
        if self.evidence.is_empty() {
            out.push_str("_none_\n");
        }
        for line in &self.evidence {
            out.push_str(&format!("- {}\n", line.trim()));
        }
        out.push_str(&format!(
            "\n## Suggested action\n\n{}\n",
            if self.suggested_action.is_empty() {
                "_none_"
            } else {
                self.suggested_action.as_str()
            }
        ));
        if let Some(toml) = &self.proposed_identity_toml {
            out.push_str(&format!("\n## Proposed identity\n\nCopy this into the identity catalog after review.\n\n```toml\n{}\n```\n", toml.trim_end()));
        }
        if self.severity == Severity::Incident {
            out.push_str(&format!("\n## Production hold\n\nProduction-class work for `{}` is held until a human runs `nanna escalation resolve {}`.\n", self.repo, self.id));
        }
        redact(&out)
    }

    /// JSON form for webhooks: every field plus `title`, `dedupe_key` and
    /// the rendered `body`, with every string redacted.
    pub fn to_json(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("escalation serialises");
        value["title"] = serde_json::Value::String(self.title());
        value["dedupe_key"] = serde_json::Value::String(self.dedupe_key());
        value["body"] = serde_json::Value::String(self.body());
        redact_value(&mut value);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap()
    }

    fn sample() -> Escalation {
        Escalation::new(
            Severity::Blocked,
            EscalationSource::Auditor,
            "example/repo",
            "scope creep: deploy requested by an inner-loop identity\nsecond line",
        )
        .with_id("esc-1")
        .with_task(TaskId("task-9".to_string()))
        .with_identity("sdlc-dev")
        .with_evidence(vec![
            "verdict: Escalate".to_string(),
            " token=ghp_abcdefghijklmnopqrstuvwxyz0123456789 ".to_string(),
        ])
        .with_suggested_action("Author a middle-loop identity")
        .with_occurrence(3)
        .at(t0())
    }

    #[test]
    fn names_round_trip_through_display_and_from_str() {
        for severity in [
            Severity::Info,
            Severity::NeedsCard,
            Severity::Blocked,
            Severity::Incident,
        ] {
            assert_eq!(severity.to_string().parse::<Severity>().unwrap(), severity);
            assert_eq!(
                serde_json::to_value(severity).unwrap(),
                severity.to_string()
            );
        }
        for source in [
            EscalationSource::Auditor,
            EscalationSource::Rollout,
            EscalationSource::Budget,
            EscalationSource::ScopeDenials,
            EscalationSource::Incident,
            EscalationSource::Manual,
        ] {
            assert_eq!(
                source.to_string().parse::<EscalationSource>().unwrap(),
                source
            );
            assert_eq!(serde_json::to_value(source).unwrap(), source.to_string());
        }
        let err = "urgent".parse::<Severity>().unwrap_err();
        assert_eq!(err.to_string(), "unknown escalation severity `urgent`");
        assert_eq!(
            "cron".parse::<EscalationSource>().unwrap_err().kind,
            "source"
        );
        assert!(Severity::Info < Severity::Incident);
    }

    #[test]
    fn new_fills_defaults() {
        let e = Escalation::new(
            Severity::Info,
            EscalationSource::Manual,
            "example/repo",
            "hi",
        );
        assert_eq!(e.occurrence, 1);
        assert!(e.task_id.is_none() && e.identity.is_none() && e.evidence.is_empty());
        assert!(e.suggested_action.is_empty() && e.proposed_identity_toml.is_none());
        assert_eq!(Uuid::parse_str(&e.id).unwrap().get_version_num(), 4);
        assert_eq!(e.headline(), "hi");
    }

    #[test]
    fn headline_is_first_line_cut_to_limit() {
        let long = "x".repeat(HEADLINE_CHARS + 5);
        let e = Escalation::new(
            Severity::Info,
            EscalationSource::Manual,
            "r",
            format!("  {long}\nmore"),
        );
        assert_eq!(e.headline(), format!("{}…", "x".repeat(HEADLINE_CHARS)));
        let exact = "y".repeat(HEADLINE_CHARS);
        assert_eq!(
            Escalation::new(Severity::Info, EscalationSource::Manual, "r", exact.clone())
                .headline(),
            exact
        );
        assert_eq!(
            Escalation::new(Severity::Info, EscalationSource::Manual, "r", "").headline(),
            ""
        );
    }

    #[test]
    fn headline_redacts_before_truncating_so_a_split_secret_cannot_leak_a_fragment() {
        let prefix = "x".repeat(HEADLINE_CHARS - 11);
        let summary = format!("{prefix} ghp_abcdefghijklmnopqrstuvwxyz0123456789 tail");
        let e = Escalation::new(Severity::Info, EscalationSource::Manual, "r", &summary);
        let headline = e.headline();
        assert!(!headline.contains("ghp_"), "{headline}");
        assert!(headline.contains("<redacted:"), "{headline}");
        let title = e.title();
        assert!(!title.contains("ghp_"), "{title}");
    }

    #[test]
    fn title_and_key_ignore_everything_but_source_repo_and_summary() {
        let a = sample();
        let b = Escalation::new(
            Severity::Info,
            EscalationSource::Auditor,
            "example/repo",
            a.summary.clone(),
        );
        assert_eq!(a.title(), b.title());
        assert_eq!(a.dedupe_key(), b.dedupe_key());
        assert_eq!(a.title(), "[nanna-escalation] auditor on example/repo: scope creep: deploy requested by an inner-loop identity (a66c090e8b084235)");
        assert_eq!(a.dedupe_key(), "auditor:example/repo:a66c090e8b084235");
        let other_repo = Escalation::new(
            Severity::Info,
            EscalationSource::Auditor,
            "example/other",
            a.summary.clone(),
        );
        let other_source = Escalation::new(
            Severity::Info,
            EscalationSource::Budget,
            "example/repo",
            a.summary.clone(),
        );
        assert_ne!(a.dedupe_key(), other_repo.dedupe_key());
        assert_ne!(a.dedupe_key(), other_source.dedupe_key());
        let leaky = Escalation::new(
            Severity::Info,
            EscalationSource::Manual,
            "r",
            "token=ghp_abcdefghijklmnopqrstuvwxyz0123456789 leaked",
        );
        assert!(leaky.title().contains("token=<redacted:credential> leaked"));
    }

    #[test]
    fn body_lists_every_section_and_redacts_evidence() {
        let body = sample().body();
        assert!(body.starts_with("**Severity:** blocked\n**Source:** auditor\n**Repository:** example/repo\n**Task:** task-9\n**Identity:** sdlc-dev\n**Occurrence:** 3\n**Raised:** 2026-09-24T12:00:00+00:00\n**Id:** esc-1\n"));
        assert!(body.contains(
            "## Summary\n\nscope creep: deploy requested by an inner-loop identity\nsecond line\n"
        ));
        assert!(body.contains("- verdict: Escalate\n- token=<redacted:credential>\n"));
        assert!(!body.contains("ghp_"));
        assert!(body.contains("## Suggested action\n\nAuthor a middle-loop identity\n"));
        assert!(!body.contains("## Proposed identity"));
        assert!(!body.contains("## Production hold"));

        let bare = Escalation::new(
            Severity::Incident,
            EscalationSource::Rollout,
            "example/repo",
            "down",
        )
        .with_id("inc-1");
        let body = bare.body();
        assert!(body.contains("**Task:** none\n**Identity:** none\n"));
        assert!(body.contains("## Evidence\n\n_none_\n"));
        assert!(body.contains("## Suggested action\n\n_none_\n"));
        assert!(body.ends_with("Production-class work for `example/repo` is held until a human runs `nanna escalation resolve inc-1`.\n"));
    }

    #[test]
    fn needs_card_body_carries_a_parseable_identity_skeleton() {
        let request = CardRequest {
            name: "sandbox-qa".to_string(),
            dev_loop: "middle".to_string(),
            max_effect: "sandbox".to_string(),
            tools: vec!["cargo_test".to_string(), "deploy_sandbox".to_string()],
            reason: "the task needs a sandbox deploy and no card allows it".to_string(),
        };
        let e = Escalation::needs_card(
            EscalationSource::Auditor,
            "example/repo",
            "no card fits",
            &request,
        );
        assert_eq!(e.severity, Severity::NeedsCard);
        assert!(e.suggested_action.contains("`sandbox-qa`"));
        let body = e.body();
        let block = body
            .split("```toml\n")
            .nth(1)
            .unwrap()
            .split("\n```")
            .next()
            .unwrap();
        let table: toml::Table = toml::from_str(block).unwrap();
        assert_eq!(table["name"].as_str(), Some("sandbox-qa"));
        assert_eq!(table["scope"]["max_effect"].as_str(), Some("sandbox"));
        assert_eq!(table["scope"]["tools"].as_array().unwrap().len(), 2);
        assert_eq!(table["loop"].as_str(), Some("middle"));
    }

    #[test]
    fn json_carries_title_key_body_and_is_redacted() {
        let json = sample().to_json();
        assert_eq!(json["id"], "esc-1");
        assert_eq!(json["severity"], "blocked");
        assert_eq!(json["source"], "auditor");
        assert_eq!(json["task_id"], "task-9");
        assert_eq!(json["occurrence"], 3);
        assert_eq!(json["dedupe_key"], "auditor:example/repo:a66c090e8b084235");
        assert!(json["title"]
            .as_str()
            .unwrap()
            .starts_with("[nanna-escalation]"));
        assert!(json["body"].as_str().unwrap().contains("## Summary"));
        assert_eq!(json["evidence"][1], " token=<redacted:credential> ");
        assert!(!json.to_string().contains("ghp_"));
        let back: Escalation =
            serde_json::from_value(serde_json::to_value(sample()).unwrap()).unwrap();
        assert_eq!(back, sample());
    }

    proptest! {
        #[test]
        fn title_is_deterministic_and_summary_sensitive(summary in "[ -~]{1,120}", other in "[ -~]{1,120}", repo in "[a-z]{1,8}/[a-z]{1,8}") {
            let a = Escalation::new(Severity::Info, EscalationSource::Budget, repo.clone(), summary.clone());
            let b = Escalation::new(Severity::Incident, EscalationSource::Budget, repo.clone(), summary.clone()).with_occurrence(9);
            prop_assert_eq!(a.title(), b.title());
            prop_assert_eq!(a.dedupe_key(), b.dedupe_key());
            let c = Escalation::new(Severity::Info, EscalationSource::Budget, repo, other.clone());
            prop_assert_eq!(a.title() == c.title(), summary == other);
        }
    }
}
