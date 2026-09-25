//! Incident evidence, remediation and postmortems.
//!
//! When a step's `[rollback].on_breach = "halt-and-escalate"` (the default
//! any non-automatic breach also falls back to), an optional
//! [`IncidentResponder`] gets a chance to remediate before the executor
//! hands off to a human: it turns the [`HealthBreach`] into an [`Incident`]
//! with fresh evidence, proposes a [`ProposedAction`], and the executor
//! passes that through the existing [`AuditHook::review_action`] seam
//! before acting. With no responder configured, the executor's behaviour
//! is unchanged: it halts and escalates directly.
//!
//! The identity this loop runs under is authored as a TOML fixture at
//! `harness/tests/fixtures/identities/global/incident-responder.toml`,
//! loaded here by [`IncidentIdentity::from_fixture`]; see that file's
//! header for why it is not yet loaded through a catalog.

use super::adapter::Slot;
use super::health::{
    worst_endpoints, EvidenceSample, HealthBreach, HealthError, HealthSource, EVIDENCE_CAP,
};
use super::log::RolloutTransition;
use chrono::Duration;
use serde::Deserialize;
use std::fmt;

/// A health breach turned into a case an [`IncidentResponder`] can act on.
#[derive(Debug, Clone, PartialEq)]
pub struct Incident {
    /// The breach that triggered this incident.
    pub breach: HealthBreach,
    /// Evidence collected while building the incident; may differ from
    /// [`HealthBreach::evidence`] if it was gathered later, over a wider
    /// window.
    pub evidence: Vec<EvidenceSample>,
    /// Plan step the breach happened at.
    pub step: usize,
    /// Rollout this incident belongs to.
    pub deploy_id: String,
}

/// What an [`IncidentResponder`] proposes doing about an [`Incident`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposedAction {
    /// Restore the previous image.
    Rollback,
    /// A fix already has an open pull request; roll forward to it once a
    /// human supplies the built image.
    RollForwardPr(String),
    /// Neither is safe to decide without a human.
    Escalate,
}

impl fmt::Display for ProposedAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProposedAction::Rollback => write!(f, "rollback"),
            ProposedAction::RollForwardPr(pr) => write!(f, "roll forward to the fix in {pr}"),
            ProposedAction::Escalate => write!(f, "escalate"),
        }
    }
}

/// Collect evidence for an incident on `slot`: the same
/// [`HealthSource`] the executor already polls, over `window`, reduced to
/// the worst-offending endpoints.
pub async fn collect_evidence(
    health: &dyn HealthSource,
    slot: &Slot,
    window: Duration,
) -> Result<Vec<EvidenceSample>, HealthError> {
    let sample = health.sample(slot, window).await?;
    Ok(worst_endpoints(&sample, EVIDENCE_CAP))
}

/// The incident-responder identity's `[scope]` table, parsed straight from
/// the fixture TOML: the tool names it is allowed to call. Other tables in
/// the file (`[identity]`, `[limits]`) are not needed here and are not
/// validated; a real catalog loader does that once the identity lane
/// merges.
#[derive(Debug, Clone, Deserialize)]
struct FixtureIdentity {
    scope: FixtureScope,
}

#[derive(Debug, Clone, Deserialize)]
struct FixtureScope {
    tools: Vec<String>,
}

/// The incident responder's allowed tool set, loaded from the fixture
/// identity TOML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncidentIdentity {
    tools: Vec<String>,
}

/// The identity fixture's TOML source, embedded at compile time.
pub const INCIDENT_RESPONDER_FIXTURE_TOML: &str =
    include_str!("../../tests/fixtures/identities/global/incident-responder.toml");

/// Tool name the identity must carry to propose [`ProposedAction::Rollback`].
pub const ROLLBACK_TOOL: &str = "rollout_rollback";
/// Tool name the identity must carry to propose
/// [`ProposedAction::RollForwardPr`].
pub const ROLL_FORWARD_PR_TOOL: &str = "rollout_roll_forward_pr";
/// Tool name the identity must carry to read evidence.
pub const READ_LOGS_TOOL: &str = "read_logs";

impl IncidentIdentity {
    /// Parse `toml`'s `[scope].tools` into an identity.
    ///
    /// ```
    /// use harness::rollout::IncidentIdentity;
    ///
    /// let identity = IncidentIdentity::from_toml_str(
    ///     "[identity]\nname = \"x\"\n[scope]\nrepos = []\npaths = []\nmax_effect = \"production\"\ntools = [\"rollout_rollback\"]\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n",
    /// )
    /// .unwrap();
    /// assert_eq!(identity.tools(), &["rollout_rollback".to_string()]);
    /// ```
    pub fn from_toml_str(toml: &str) -> Result<Self, toml::de::Error> {
        let raw: FixtureIdentity = toml::from_str(toml)?;
        Ok(Self {
            tools: raw.scope.tools,
        })
    }

    /// The bundled `incident-responder.toml` fixture.
    pub fn from_fixture() -> Self {
        Self::from_toml_str(INCIDENT_RESPONDER_FIXTURE_TOML)
            .expect("bundled incident-responder.toml is valid")
    }

    /// The identity's allowed tool names, in file order.
    pub fn tools(&self) -> &[String] {
        &self.tools
    }

    /// Whether the identity's tools permit taking `action`. `Escalate`
    /// needs no tool: handing off to a human is always allowed.
    pub fn permits(&self, action: &ProposedAction) -> bool {
        match action {
            ProposedAction::Rollback => self.tools.iter().any(|t| t == ROLLBACK_TOOL),
            ProposedAction::RollForwardPr(_) => {
                self.tools.iter().any(|t| t == ROLL_FORWARD_PR_TOOL)
            }
            ProposedAction::Escalate => true,
        }
    }
}

/// Proposes a remediation for an [`Incident`] within [`IncidentIdentity`]'s
/// scope. It does not act on its own: the executor persists the resulting
/// transition (or halts and escalates) after the proposal clears
/// [`AuditHook::review_action`](super::AuditHook::review_action).
#[derive(Debug, Clone)]
pub struct IncidentResponder {
    identity: IncidentIdentity,
}

impl IncidentResponder {
    /// A responder scoped to `identity`.
    pub fn new(identity: IncidentIdentity) -> Self {
        Self { identity }
    }

    /// The identity this responder acts under.
    pub fn identity(&self) -> &IncidentIdentity {
        &self.identity
    }

    /// Propose a remediation for `breach`. A shadow-divergence breach has
    /// not (yet) reached live traffic, so it is always escalated for a
    /// human read rather than rolled back. Otherwise, with a fix already
    /// known (`known_fix_pr`), the responder proposes rolling forward to
    /// it; with none, it proposes the always-safe rollback to the known
    /// good image.
    ///
    /// ```
    /// use harness::deploy::ShadowCompare;
    /// use harness::rollout::{
    ///     HealthBreach, HealthObservation, HealthThreshold, IncidentIdentity, IncidentResponder,
    ///     ProposedAction,
    /// };
    ///
    /// let responder = IncidentResponder::new(IncidentIdentity::from_fixture());
    /// let breach = HealthBreach {
    ///     threshold: HealthThreshold::ErrorRateMax(0.01),
    ///     observed: HealthObservation::ErrorRate(0.5),
    ///     step: 0,
    ///     evidence: vec![],
    /// };
    /// assert_eq!(responder.propose(&breach, None), ProposedAction::Rollback);
    /// assert_eq!(
    ///     responder.propose(&breach, Some("https://example.invalid/pr/9")),
    ///     ProposedAction::RollForwardPr("https://example.invalid/pr/9".to_string())
    /// );
    /// let shadow = HealthBreach {
    ///     threshold: HealthThreshold::ShadowDivergenceMax { compare: ShadowCompare::Status, max: 0.1 },
    ///     observed: HealthObservation::ShadowDivergence(0.3),
    ///     step: 0,
    ///     evidence: vec![],
    /// };
    /// assert_eq!(responder.propose(&shadow, None), ProposedAction::Escalate);
    /// ```
    pub fn propose(&self, breach: &HealthBreach, known_fix_pr: Option<&str>) -> ProposedAction {
        use super::health::HealthThreshold;
        if matches!(
            breach.threshold,
            HealthThreshold::ShadowDivergenceMax { .. }
        ) {
            return ProposedAction::Escalate;
        }
        match known_fix_pr {
            Some(pr) => ProposedAction::RollForwardPr(pr.to_string()),
            None => ProposedAction::Rollback,
        }
    }

    /// Whether this responder's identity permits `action`.
    pub fn permits(&self, action: &ProposedAction) -> bool {
        self.identity.permits(action)
    }
}

/// A rendered postmortem, ready to file as a GitHub issue body (filing it
/// is escalation's job, not this crate's).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Postmortem {
    /// Issue title.
    pub title: String,
    /// Markdown issue body.
    pub body: String,
}

impl Incident {
    /// Render a postmortem from this incident, the `action` taken (or
    /// proposed, if a human overrode it), and `history`, the rollout's
    /// transitions from [`RolloutLog::history`](super::RolloutLog::history)
    /// (oldest first), summarising the remediation as `verdict`.
    ///
    /// ```
    /// use harness::rollout::{
    ///     HealthBreach, HealthObservation, HealthThreshold, Incident, ProposedAction,
    /// };
    ///
    /// let incident = Incident {
    ///     breach: HealthBreach {
    ///         threshold: HealthThreshold::ErrorRateMax(0.01),
    ///         observed: HealthObservation::ErrorRate(0.5),
    ///         step: 1,
    ///         evidence: vec![],
    ///     },
    ///     evidence: vec![],
    ///     step: 1,
    ///     deploy_id: "rollout-1".to_string(),
    /// };
    /// let postmortem = incident.postmortem(&ProposedAction::Rollback, &[], "rolled back automatically");
    /// assert_eq!(postmortem.title, "Incident postmortem: rollout-1 step 1: error_rate_max 0.01 breached by error rate 0.5");
    /// assert!(postmortem.body.contains("## Action\n\nrollback"));
    /// assert!(postmortem.body.contains("## Verdict\n\nrolled back automatically"));
    /// ```
    pub fn postmortem(
        &self,
        action: &ProposedAction,
        history: &[RolloutTransition],
        verdict: &str,
    ) -> Postmortem {
        let title = format!("Incident postmortem: {} {}", self.deploy_id, self.breach);
        let mut body = format!("# Incident postmortem: {}\n\n", self.deploy_id);
        body.push_str(&format!("**Breach:** {}\n\n", self.breach));
        body.push_str("## Evidence\n\n");
        if self.evidence.is_empty() {
            body.push_str("No endpoint evidence was attached to this breach.\n\n");
        } else {
            for sample in &self.evidence {
                body.push_str(&format!(
                    "- `{}` responded `{}`\n",
                    sample.endpoint, sample.status
                ));
            }
            body.push('\n');
        }
        body.push_str("## Timeline\n\n");
        if history.is_empty() {
            body.push_str("No rollout transitions were recorded.\n\n");
        } else {
            for transition in history {
                body.push_str(&format!("- {}\n", transition.summary()));
            }
            body.push('\n');
        }
        body.push_str(&format!("## Action\n\n{action}\n\n"));
        body.push_str(&format!("## Verdict\n\n{verdict}\n"));
        Postmortem { title, body }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rollout::health::{HealthObservation, HealthThreshold};
    use crate::rollout::log::RolloutLog;
    use crate::rollout::state::tests::{plan, t0};
    use crate::rollout::state::{RolloutRecord, RolloutState};

    fn breach() -> HealthBreach {
        HealthBreach {
            threshold: HealthThreshold::ErrorRateMax(0.01),
            observed: HealthObservation::ErrorRate(0.5),
            step: 0,
            evidence: vec![EvidenceSample {
                endpoint: "/health/v1".into(),
                status: 503,
            }],
        }
    }

    #[test]
    fn responder_exposes_its_identity() {
        let identity = IncidentIdentity::from_fixture();
        let responder = IncidentResponder::new(identity.clone());
        assert_eq!(responder.identity(), &identity);
    }

    #[test]
    fn propose_prefers_a_known_fix_pr_and_always_escalates_shadow_divergence() {
        let responder = IncidentResponder::new(IncidentIdentity::from_fixture());
        assert_eq!(responder.propose(&breach(), None), ProposedAction::Rollback);
        assert_eq!(
            responder.propose(&breach(), Some("https://example.invalid/pr/9")),
            ProposedAction::RollForwardPr("https://example.invalid/pr/9".to_string())
        );
        let shadow = HealthBreach {
            threshold: HealthThreshold::ShadowDivergenceMax {
                compare: crate::deploy::ShadowCompare::Status,
                max: 0.1,
            },
            observed: HealthObservation::ShadowDivergence(0.3),
            step: 0,
            evidence: vec![],
        };
        assert_eq!(responder.propose(&shadow, None), ProposedAction::Escalate);
        assert_eq!(
            responder.propose(&shadow, Some("https://example.invalid/pr/9")),
            ProposedAction::Escalate
        );
    }

    #[test]
    fn proposed_action_display() {
        assert_eq!(ProposedAction::Rollback.to_string(), "rollback");
        assert_eq!(
            ProposedAction::RollForwardPr("https://example.invalid/pr/9".into()).to_string(),
            "roll forward to the fix in https://example.invalid/pr/9"
        );
        assert_eq!(ProposedAction::Escalate.to_string(), "escalate");
    }

    #[test]
    fn identity_permits_exactly_its_declared_tools() {
        let identity = IncidentIdentity::from_fixture();
        assert!(identity.permits(&ProposedAction::Rollback));
        assert!(identity.permits(&ProposedAction::RollForwardPr("pr".into())));
        assert!(identity.permits(&ProposedAction::Escalate));
        let narrow = IncidentIdentity::from_toml_str(
            "[identity]\nname = \"x\"\n[scope]\nrepos = []\npaths = []\nmax_effect = \"production\"\ntools = [\"read_logs\"]\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n",
        )
        .unwrap();
        assert!(!narrow.permits(&ProposedAction::Rollback));
        assert!(!narrow.permits(&ProposedAction::RollForwardPr("pr".into())));
        assert!(narrow.permits(&ProposedAction::Escalate));
        assert!(IncidentIdentity::from_toml_str("not valid toml =").is_err());
    }

    #[test]
    fn fixture_identity_registry_contains_exactly_the_allowed_tools() {
        let identity = IncidentIdentity::from_fixture();
        let mut tools = identity.tools().to_vec();
        tools.sort();
        let mut expected = vec![
            READ_LOGS_TOOL.to_string(),
            ROLLBACK_TOOL.to_string(),
            ROLL_FORWARD_PR_TOOL.to_string(),
        ];
        expected.sort();
        assert_eq!(tools, expected);
    }

    #[tokio::test]
    async fn collect_evidence_reduces_the_sample_to_worst_offenders() {
        use crate::rollout::health::FakeHealthSource;
        let source = FakeHealthSource::healthy(&["/health/v1".to_string(), "/ready".to_string()]);
        source.push(crate::rollout::health::HealthSample {
            error_rate: 0.5,
            p99_latency_ms: 10,
            endpoint_statuses: [("/health/v1".to_string(), 503u16)].into_iter().collect(),
        });
        let evidence = collect_evidence(&source, &Slot::new("slot-0"), Duration::minutes(1))
            .await
            .unwrap();
        assert_eq!(
            evidence,
            vec![EvidenceSample {
                endpoint: "/health/v1".into(),
                status: 503
            }]
        );
    }

    #[test]
    fn postmortem_renders_evidence_and_timeline_deterministically() {
        let incident = Incident {
            breach: breach(),
            evidence: breach().evidence,
            step: 0,
            deploy_id: "rollout-1".to_string(),
        };
        let dir = tempfile::tempdir().unwrap();
        let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
        let mut record = RolloutRecord::new(
            "rollout-1",
            plan("production"),
            "registry.example.invalid/ns/app:v2",
            "registry.example.invalid/ns/app:v1",
            t0(),
        );
        record.state = RolloutState::Step(0);
        log.append(None, &record).unwrap();
        let later = t0() + Duration::minutes(5);
        record
            .transition(
                RolloutState::Baking {
                    step: 0,
                    since: later,
                },
                later,
            )
            .unwrap();
        log.append(Some(&RolloutState::Step(0)), &record).unwrap();
        let history = log.history("rollout-1").unwrap();
        let postmortem = incident.postmortem(
            &ProposedAction::Rollback,
            &history,
            "rolled back automatically",
        );
        assert_eq!(
            postmortem.title,
            "Incident postmortem: rollout-1 step 0: error_rate_max 0.01 breached by error rate 0.5"
        );
        let expected = format!(
            "# Incident postmortem: rollout-1\n\n\
**Breach:** step 0: error_rate_max 0.01 breached by error rate 0.5\n\n\
## Evidence\n\n\
- `/health/v1` responded `503`\n\n\
## Timeline\n\n\
- {}  created -> step 0  traffic 0%\n\
- {}  step 0 -> baking step 0 since {later}  traffic 0%\n\n\
## Action\n\n\
rollback\n\n\
## Verdict\n\n\
rolled back automatically\n",
            t0(),
            later,
        );
        assert_eq!(postmortem.body, expected);
    }

    #[test]
    fn postmortem_with_no_evidence_and_no_history_says_so() {
        let incident = Incident {
            breach: HealthBreach {
                threshold: HealthThreshold::ErrorRateMax(0.01),
                observed: HealthObservation::ErrorRate(0.02),
                step: 0,
                evidence: vec![],
            },
            evidence: vec![],
            step: 0,
            deploy_id: "rollout-2".to_string(),
        };
        let postmortem = incident.postmortem(&ProposedAction::Escalate, &[], "escalated");
        assert!(postmortem.body.contains("## Action\n\nescalate\n\n"));
        assert!(postmortem
            .body
            .contains("No endpoint evidence was attached to this breach.\n\n"));
        assert!(postmortem
            .body
            .contains("No rollout transitions were recorded.\n\n"));
    }
}
