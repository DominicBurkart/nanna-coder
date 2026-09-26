use super::adapter::Slot;
use crate::deploy::{Health, ShadowCompare};
use async_trait::async_trait;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::Mutex;
use thiserror::Error;

/// What a slot looked like over one observation window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthSample {
    /// Share of requests that failed, `0.0..=1.0`.
    pub error_rate: f64,
    /// 99th-percentile latency in milliseconds.
    pub p99_latency_ms: u32,
    /// Last HTTP status observed per probed endpoint path.
    pub endpoint_statuses: BTreeMap<String, u16>,
}

impl HealthSample {
    /// A sample with no errors, negligible latency and `200` on every
    /// endpoint in `endpoints`.
    pub fn healthy(endpoints: &[String]) -> Self {
        Self {
            error_rate: 0.0,
            p99_latency_ms: 1,
            endpoint_statuses: endpoints.iter().map(|e| (e.clone(), 200)).collect(),
        }
    }
}

/// The health source could not observe the slot.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("health source failed: {0}")]
pub struct HealthError(pub String);

/// Where the executor reads a slot's health from during a bake.
///
/// Real sources (endpoint probes, log-derived rates) are provided by the
/// monitoring work; [`FakeHealthSource`] scripts samples for tests.
#[async_trait]
pub trait HealthSource: Send + Sync {
    /// Observe `slot` over the last `window`.
    async fn sample(&self, slot: &Slot, window: Duration) -> Result<HealthSample, HealthError>;
}

/// The `[health]` gate that was breached.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthThreshold {
    /// `error_rate_max`.
    ErrorRateMax(f64),
    /// `latency_p99_max_ms`.
    LatencyP99MaxMs(u32),
    /// An endpoint in `endpoints` must answer with a `2xx`.
    EndpointOk(String),
    /// `[shadow].max_divergence` for one compared attribute.
    ShadowDivergenceMax {
        /// Attribute whose divergence rate is bounded.
        compare: ShadowCompare,
        /// The bound.
        max: f64,
    },
}

/// The value that breached a [`HealthThreshold`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthObservation {
    /// Observed error rate.
    ErrorRate(f64),
    /// Observed p99 latency.
    LatencyP99Ms(u32),
    /// Observed status code.
    EndpointStatus(u16),
    /// The sample had no data for the endpoint.
    EndpointMissing,
    /// Observed share of mirrored pairs that diverged.
    ShadowDivergence(f64),
}

/// One endpoint's status at the moment of a [`HealthBreach`], kept as
/// evidence for whoever investigates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSample {
    /// Endpoint path probed.
    pub endpoint: String,
    /// Status it answered with.
    pub status: u16,
}

/// How many [`EvidenceSample`]s a breach carries at most.
pub const EVIDENCE_CAP: usize = 3;

/// The worst-offending endpoints in `sample`, at most `cap`: every
/// non-`2xx` status, worst status first, ties broken by endpoint name for
/// a deterministic order (`endpoint_statuses` is a `BTreeMap`, so this is
/// stable across calls).
pub(crate) fn worst_endpoints(sample: &HealthSample, cap: usize) -> Vec<EvidenceSample> {
    let mut offenders: Vec<EvidenceSample> = sample
        .endpoint_statuses
        .iter()
        .filter(|(_, status)| !(200..300).contains(*status))
        .map(|(endpoint, status)| EvidenceSample {
            endpoint: endpoint.clone(),
            status: *status,
        })
        .collect();
    offenders.sort_by(|a, b| {
        b.status
            .cmp(&a.status)
            .then_with(|| a.endpoint.cmp(&b.endpoint))
    });
    offenders.truncate(cap);
    offenders
}

/// A health gate breached during step `step`; what `[rollback].on_breach`
/// acts on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthBreach {
    /// The gate.
    pub threshold: HealthThreshold,
    /// The observation that breached it.
    pub observed: HealthObservation,
    /// Plan step during which it happened.
    pub step: usize,
    /// A few of the worst-offending endpoint statuses observed alongside
    /// the breach, for an incident responder to inspect without a second
    /// round trip. Empty for a shadow-divergence breach, which has no
    /// endpoint identity to attach. `#[serde(default)]` so a rollout log
    /// line written before this field existed still deserialises.
    #[serde(default)]
    pub evidence: Vec<EvidenceSample>,
}

impl fmt::Display for HealthBreach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let threshold = match &self.threshold {
            HealthThreshold::ErrorRateMax(max) => format!("error_rate_max {max}"),
            HealthThreshold::LatencyP99MaxMs(max) => format!("latency_p99_max_ms {max}"),
            HealthThreshold::EndpointOk(path) => format!("endpoint {path} ok"),
            HealthThreshold::ShadowDivergenceMax { compare, max } => {
                format!("shadow {} divergence max {max}", compare.name())
            }
        };
        let observed = match &self.observed {
            HealthObservation::ErrorRate(rate) => format!("error rate {rate}"),
            HealthObservation::LatencyP99Ms(ms) => format!("p99 {ms} ms"),
            HealthObservation::EndpointStatus(status) => format!("status {status}"),
            HealthObservation::EndpointMissing => "no sample".to_string(),
            HealthObservation::ShadowDivergence(rate) => format!("divergence rate {rate}"),
        };
        write!(f, "step {}: {threshold} breached by {observed}", self.step)
    }
}

/// Compare `sample` with the template's `[health]` gates for `step`.
///
/// Gates are checked in order: error rate, p99 latency, then each listed
/// endpoint, which must be present with a `2xx` status. The first breach
/// is returned.
///
/// ```
/// use harness::deploy::DeployTemplate;
/// use harness::rollout::{check_health, HealthObservation, HealthSample, HealthThreshold};
///
/// let template = DeployTemplate::parse(
///     "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"instant\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.01\nlatency_p99_max_ms = 800\nbake_time = \"30m\"\n",
/// )
/// .unwrap();
/// let health = template.health.unwrap();
/// let mut sample = HealthSample::healthy(&health.endpoints);
/// assert!(check_health(&health, &sample, 2).is_none());
///
/// sample.p99_latency_ms = 900;
/// let breach = check_health(&health, &sample, 2).unwrap();
/// assert_eq!(breach.threshold, HealthThreshold::LatencyP99MaxMs(800));
/// assert_eq!(breach.observed, HealthObservation::LatencyP99Ms(900));
/// assert_eq!(breach.step, 2);
/// ```
pub fn check_health(health: &Health, sample: &HealthSample, step: usize) -> Option<HealthBreach> {
    let evidence = worst_endpoints(sample, EVIDENCE_CAP);
    if sample.error_rate > health.error_rate_max {
        return Some(HealthBreach {
            threshold: HealthThreshold::ErrorRateMax(health.error_rate_max),
            observed: HealthObservation::ErrorRate(sample.error_rate),
            step,
            evidence,
        });
    }
    if sample.p99_latency_ms > health.latency_p99_max_ms {
        return Some(HealthBreach {
            threshold: HealthThreshold::LatencyP99MaxMs(health.latency_p99_max_ms),
            observed: HealthObservation::LatencyP99Ms(sample.p99_latency_ms),
            step,
            evidence,
        });
    }
    for endpoint in &health.endpoints {
        let observed = match sample.endpoint_statuses.get(endpoint) {
            Some(status) if (200..300).contains(status) => continue,
            Some(status) => HealthObservation::EndpointStatus(*status),
            None => HealthObservation::EndpointMissing,
        };
        return Some(HealthBreach {
            threshold: HealthThreshold::EndpointOk(endpoint.clone()),
            observed,
            step,
            evidence,
        });
    }
    None
}

/// Health source scripted from a queue of samples.
///
/// Each [`sample`](HealthSource::sample) call pops the next scripted sample
/// and falls back to the default once the queue is empty, so a test scripts
/// one sample per bake poll. Every call is recorded.
#[derive(Debug)]
pub struct FakeHealthSource {
    default: HealthSample,
    scripted: Mutex<VecDeque<HealthSample>>,
    calls: Mutex<Vec<(Slot, Duration)>>,
    failing: Mutex<bool>,
}

impl FakeHealthSource {
    /// A source that answers `default` unless a sample is scripted.
    pub fn new(default: HealthSample) -> Self {
        Self {
            default,
            scripted: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            failing: Mutex::new(false),
        }
    }

    /// A source that always reports `200` on `endpoints` and no errors.
    pub fn healthy(endpoints: &[String]) -> Self {
        Self::new(HealthSample::healthy(endpoints))
    }

    /// Queue `sample` to be returned by the next unscripted poll.
    pub fn push(&self, sample: HealthSample) {
        self.scripted.lock().unwrap().push_back(sample);
    }

    /// Queue `healthy` polls of the default, then `sample`.
    pub fn push_after(&self, healthy: usize, sample: HealthSample) {
        let mut queue = self.scripted.lock().unwrap();
        for _ in 0..healthy {
            queue.push_back(self.default.clone());
        }
        queue.push_back(sample);
    }

    /// Make every poll fail (or succeed again).
    pub fn set_failing(&self, failing: bool) {
        *self.failing.lock().unwrap() = failing;
    }

    /// Every `(slot, window)` polled so far.
    pub fn calls(&self) -> Vec<(Slot, Duration)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl HealthSource for FakeHealthSource {
    async fn sample(&self, slot: &Slot, window: Duration) -> Result<HealthSample, HealthError> {
        self.calls.lock().unwrap().push((slot.clone(), window));
        if *self.failing.lock().unwrap() {
            return Err(HealthError("scripted failure".into()));
        }
        let next = self.scripted.lock().unwrap().pop_front();
        Ok(next.unwrap_or_else(|| self.default.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health() -> Health {
        Health {
            endpoints: vec!["/health/v1".into(), "/ready".into()],
            error_rate_max: 0.01,
            latency_p99_max_ms: 800,
            bake_time: Duration::minutes(30),
        }
    }

    #[test]
    fn healthy_sample_passes_every_gate() {
        let h = health();
        let sample = HealthSample::healthy(&h.endpoints);
        assert_eq!(sample.endpoint_statuses.len(), 2);
        assert!(check_health(&h, &sample, 0).is_none());
        let boundary = HealthSample {
            error_rate: 0.01,
            p99_latency_ms: 800,
            ..sample
        };
        assert!(check_health(&h, &boundary, 0).is_none());
    }

    #[test]
    fn breaches_are_reported_in_gate_order() {
        let h = health();
        let mut sample = HealthSample::healthy(&h.endpoints);
        sample.error_rate = 0.5;
        sample.p99_latency_ms = 5000;
        let breach = check_health(&h, &sample, 3).unwrap();
        assert_eq!(breach.threshold, HealthThreshold::ErrorRateMax(0.01));
        assert_eq!(breach.observed, HealthObservation::ErrorRate(0.5));
        assert_eq!(
            breach.to_string(),
            "step 3: error_rate_max 0.01 breached by error rate 0.5"
        );
        sample.error_rate = 0.0;
        let breach = check_health(&h, &sample, 3).unwrap();
        assert_eq!(
            breach.to_string(),
            "step 3: latency_p99_max_ms 800 breached by p99 5000 ms"
        );
        sample.p99_latency_ms = 10;
        sample.endpoint_statuses.insert("/ready".into(), 503);
        let breach = check_health(&h, &sample, 3).unwrap();
        assert_eq!(
            breach.threshold,
            HealthThreshold::EndpointOk("/ready".into())
        );
        assert_eq!(
            breach.to_string(),
            "step 3: endpoint /ready ok breached by status 503"
        );
        sample.endpoint_statuses.remove("/health/v1");
        let breach = check_health(&h, &sample, 3).unwrap();
        assert_eq!(breach.observed, HealthObservation::EndpointMissing);
        assert_eq!(
            breach.to_string(),
            "step 3: endpoint /health/v1 ok breached by no sample"
        );
        let json = serde_json::to_string(&breach).unwrap();
        assert_eq!(serde_json::from_str::<HealthBreach>(&json).unwrap(), breach);
    }

    #[test]
    fn breach_evidence_carries_the_worst_offending_endpoints_capped_and_ordered() {
        let h = health();
        let mut sample = HealthSample::healthy(&h.endpoints);
        sample.error_rate = 0.5;
        sample.endpoint_statuses.insert("/ready".into(), 503);
        sample.endpoint_statuses.insert("/metrics".into(), 500);
        sample.endpoint_statuses.insert("/version".into(), 429);
        let breach = check_health(&h, &sample, 1).unwrap();
        assert_eq!(
            breach.evidence,
            vec![
                EvidenceSample {
                    endpoint: "/ready".into(),
                    status: 503
                },
                EvidenceSample {
                    endpoint: "/metrics".into(),
                    status: 500
                },
                EvidenceSample {
                    endpoint: "/version".into(),
                    status: 429
                },
            ]
        );
        assert_eq!(worst_endpoints(&sample, 3).len(), EVIDENCE_CAP);
        assert!(worst_endpoints(&HealthSample::healthy(&h.endpoints), 3).is_empty());
    }

    #[test]
    fn breach_evidence_defaults_when_missing_from_an_older_log_line() {
        let json =
            r#"{"threshold":{"error_rate_max":0.01},"observed":{"error_rate":0.5},"step":0}"#;
        let breach: HealthBreach = serde_json::from_str(json).unwrap();
        assert!(breach.evidence.is_empty());
    }

    #[tokio::test]
    async fn fake_source_scripts_samples_in_order_then_defaults() {
        let h = health();
        let source = FakeHealthSource::healthy(&h.endpoints);
        let bad = HealthSample {
            error_rate: 1.0,
            ..HealthSample::healthy(&h.endpoints)
        };
        source.push_after(1, bad.clone());
        source.push(HealthSample {
            p99_latency_ms: 999,
            ..bad.clone()
        });
        let slot = Slot::new("slot-1");
        let w = Duration::minutes(1);
        assert!(check_health(&h, &source.sample(&slot, w).await.unwrap(), 0).is_none());
        assert_eq!(source.sample(&slot, w).await.unwrap(), bad);
        assert_eq!(source.sample(&slot, w).await.unwrap().p99_latency_ms, 999);
        assert!(check_health(&h, &source.sample(&slot, w).await.unwrap(), 0).is_none());
        assert_eq!(source.calls().len(), 4);
        assert_eq!(source.calls()[0], (slot.clone(), w));
        source.set_failing(true);
        let err = source.sample(&slot, w).await.unwrap_err();
        assert_eq!(err.to_string(), "health source failed: scripted failure");
        source.set_failing(false);
        assert!(source.sample(&slot, w).await.is_ok());
    }
}
