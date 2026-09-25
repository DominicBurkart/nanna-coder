use super::adapter::Slot;
use super::health::{HealthBreach, HealthObservation, HealthThreshold};
use crate::deploy::{Shadow, ShadowCompare};
use async_trait::async_trait;
use chrono::Duration;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Mutex;
use thiserror::Error;

/// One live request and its mirrored copy, observed together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSample {
    /// Status the active slot answered with.
    pub status_active: u16,
    /// Status the shadow slot answered with.
    pub status_shadow: u16,
    /// Latency of the active slot's answer in milliseconds.
    pub latency_active_ms: u32,
    /// Latency of the shadow slot's answer in milliseconds.
    pub latency_shadow_ms: u32,
}

impl ShadowSample {
    /// A pair whose answers agree on everything.
    pub fn identical(status: u16, latency_ms: u32) -> Self {
        Self {
            status_active: status,
            status_shadow: status,
            latency_active_ms: latency_ms,
            latency_shadow_ms: latency_ms,
        }
    }
}

/// Divergence of the shadow slot from the active one on one attribute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowDivergence {
    /// Attribute compared.
    pub compare: ShadowCompare,
    /// Pairs observed.
    pub samples: usize,
    /// Pairs that diverged.
    pub diverged: usize,
}

impl ShadowDivergence {
    /// `diverged / samples`, or `0.0` with no samples.
    pub fn rate(&self) -> f64 {
        if self.samples == 0 {
            return 0.0;
        }
        self.diverged as f64 / self.samples as f64
    }
}

/// Share by which a mirrored answer may be slower than the live one
/// before it counts as a latency divergence, unless overridden with
/// [`ShadowComparator::with_latency_slack`].
pub const DEFAULT_LATENCY_SLACK: f64 = 0.5;

/// Compares paired observations per the template's `[shadow].compare` and
/// turns a divergence rate above `[shadow].max_divergence` into a
/// [`HealthBreach`], so the executor's breach policy applies unchanged.
///
/// Status diverges when the two codes differ. Latency diverges when the
/// shadow answer is slower than the live one by more than the slack
/// ratio (`latency_shadow > latency_active * (1 + slack)`). Rates are
/// per attribute over every sample given; no samples means no divergence.
///
/// ```
/// use harness::deploy::ShadowCompare;
/// use harness::rollout::{HealthObservation, HealthThreshold, ShadowComparator, ShadowSample};
///
/// let comparator = ShadowComparator::new(vec![ShadowCompare::Status, ShadowCompare::Latency], 0.25);
/// let samples = [
///     ShadowSample::identical(200, 10),
///     ShadowSample::identical(200, 12),
///     ShadowSample { status_active: 200, status_shadow: 500, latency_active_ms: 10, latency_shadow_ms: 10 },
///     ShadowSample { status_active: 200, status_shadow: 500, latency_active_ms: 10, latency_shadow_ms: 40 },
/// ];
/// let rates: Vec<f64> = comparator.divergence(&samples).iter().map(|d| d.rate()).collect();
/// assert_eq!(rates, [0.5, 0.25]);
/// let breach = comparator.check(&samples, 0).unwrap();
/// assert_eq!(breach.threshold, HealthThreshold::ShadowDivergenceMax { compare: ShadowCompare::Status, max: 0.25 });
/// assert_eq!(breach.observed, HealthObservation::ShadowDivergence(0.5));
/// assert!(comparator.check(&samples[..2], 0).is_none());
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowComparator {
    compare: Vec<ShadowCompare>,
    max_divergence: f64,
    latency_slack: f64,
}

impl ShadowComparator {
    /// Compare `compare` attributes; a rate above `max_divergence` breaches.
    pub fn new(compare: Vec<ShadowCompare>, max_divergence: f64) -> Self {
        Self {
            compare,
            max_divergence,
            latency_slack: DEFAULT_LATENCY_SLACK,
        }
    }

    /// The comparator a template's `[shadow]` section describes.
    pub fn from_template(shadow: &Shadow) -> Self {
        Self::new(shadow.compare.clone(), shadow.max_divergence)
    }

    /// Replace the latency slack ratio.
    pub fn with_latency_slack(mut self, slack: f64) -> Self {
        self.latency_slack = slack;
        self
    }

    /// Attributes compared, in template order.
    pub fn compare(&self) -> &[ShadowCompare] {
        &self.compare
    }

    /// Rate above which an attribute's divergence breaches.
    pub fn max_divergence(&self) -> f64 {
        self.max_divergence
    }

    fn diverges(&self, compare: ShadowCompare, sample: &ShadowSample) -> bool {
        match compare {
            ShadowCompare::Status => sample.status_active != sample.status_shadow,
            ShadowCompare::Latency => {
                let allowed = f64::from(sample.latency_active_ms) * (1.0 + self.latency_slack);
                f64::from(sample.latency_shadow_ms) > allowed
            }
        }
    }

    /// Divergence per compared attribute over `samples`.
    pub fn divergence(&self, samples: &[ShadowSample]) -> Vec<ShadowDivergence> {
        self.compare
            .iter()
            .map(|&compare| ShadowDivergence {
                compare,
                samples: samples.len(),
                diverged: samples.iter().filter(|s| self.diverges(compare, s)).count(),
            })
            .collect()
    }

    /// The first attribute whose divergence rate exceeds the threshold,
    /// as a breach of `step`.
    pub fn check(&self, samples: &[ShadowSample], step: usize) -> Option<HealthBreach> {
        self.divergence(samples)
            .into_iter()
            .find(|d| d.rate() > self.max_divergence)
            .map(|d| HealthBreach {
                threshold: HealthThreshold::ShadowDivergenceMax {
                    compare: d.compare,
                    max: self.max_divergence,
                },
                observed: HealthObservation::ShadowDivergence(d.rate()),
                step,
            })
    }
}

/// The shadow source could not observe the pair.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("shadow source failed: {0}")]
pub struct ShadowError(pub String);

/// Where the executor reads paired live/mirrored observations from while
/// a shadow step bakes.
///
/// Real sources come with the monitoring work; [`FakeShadowSource`]
/// scripts batches for tests and the executor refuses a shadow step with
/// [`NoShadowSource`] rather than passing it unobserved.
#[async_trait]
pub trait ShadowSource: Send + Sync {
    /// Every pair observed for the mirror into `shadow` over the last `window`.
    async fn sample(
        &self,
        shadow: &Slot,
        window: Duration,
    ) -> Result<Vec<ShadowSample>, ShadowError>;
}

/// A source for executors that never run shadow steps; every sample fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoShadowSource;

#[async_trait]
impl ShadowSource for NoShadowSource {
    async fn sample(
        &self,
        shadow: &Slot,
        _window: Duration,
    ) -> Result<Vec<ShadowSample>, ShadowError> {
        Err(ShadowError(format!(
            "no shadow source is configured to observe {shadow}"
        )))
    }
}

/// Shadow source scripted from a queue of batches.
///
/// Each [`sample`](ShadowSource::sample) call pops the next scripted batch
/// and falls back to the default once the queue is empty, so a test
/// scripts one batch per bake poll. Every call is recorded.
#[derive(Debug)]
pub struct FakeShadowSource {
    default: Vec<ShadowSample>,
    scripted: Mutex<VecDeque<Vec<ShadowSample>>>,
    calls: Mutex<Vec<(Slot, Duration)>>,
    failing: Mutex<bool>,
}

impl FakeShadowSource {
    /// A source that answers `default` unless a batch is scripted.
    pub fn new(default: Vec<ShadowSample>) -> Self {
        Self {
            default,
            scripted: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            failing: Mutex::new(false),
        }
    }

    /// A source whose default batch is `pairs` identical `200`/`10 ms` pairs.
    pub fn agreeing(pairs: usize) -> Self {
        Self::new(vec![ShadowSample::identical(200, 10); pairs])
    }

    /// Queue `batch` to be returned by the next unscripted poll.
    pub fn push(&self, batch: Vec<ShadowSample>) {
        self.scripted.lock().unwrap().push_back(batch);
    }

    /// Queue `agreeing` polls of the default, then `batch`.
    pub fn push_after(&self, agreeing: usize, batch: Vec<ShadowSample>) {
        let mut queue = self.scripted.lock().unwrap();
        for _ in 0..agreeing {
            queue.push_back(self.default.clone());
        }
        queue.push_back(batch);
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
impl ShadowSource for FakeShadowSource {
    async fn sample(
        &self,
        shadow: &Slot,
        window: Duration,
    ) -> Result<Vec<ShadowSample>, ShadowError> {
        self.calls.lock().unwrap().push((shadow.clone(), window));
        if *self.failing.lock().unwrap() {
            return Err(ShadowError("scripted failure".into()));
        }
        let next = self.scripted.lock().unwrap().pop_front();
        Ok(next.unwrap_or_else(|| self.default.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::DeployTemplate;

    fn pair(status_shadow: u16, latency_shadow_ms: u32) -> ShadowSample {
        ShadowSample {
            status_active: 200,
            status_shadow,
            latency_active_ms: 100,
            latency_shadow_ms,
        }
    }

    #[test]
    fn status_divergence_counts_differing_codes() {
        let c = ShadowComparator::new(vec![ShadowCompare::Status], 0.05);
        let samples = [
            pair(200, 100),
            pair(201, 100),
            pair(500, 100),
            pair(200, 900),
        ];
        let d = c.divergence(&samples);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].compare, ShadowCompare::Status);
        assert_eq!((d[0].samples, d[0].diverged), (4, 2));
        assert_eq!(d[0].rate(), 0.5);
        let json = serde_json::to_string(&d[0]).unwrap();
        assert_eq!(
            serde_json::from_str::<ShadowDivergence>(&json).unwrap(),
            d[0]
        );
    }

    #[test]
    fn latency_divergence_allows_the_slack() {
        let c = ShadowComparator::new(vec![ShadowCompare::Latency], 0.05);
        let samples = [
            pair(200, 150),
            pair(200, 151),
            pair(500, 50),
            pair(200, 100),
        ];
        let d = c.divergence(&samples);
        assert_eq!((d[0].samples, d[0].diverged), (4, 1));
        assert_eq!(d[0].rate(), 0.25);
        let strict = c.clone().with_latency_slack(0.0);
        assert_eq!(strict.divergence(&samples)[0].diverged, 2);
        assert_eq!(c.compare(), [ShadowCompare::Latency]);
        assert_eq!(c.max_divergence(), 0.05);
    }

    #[test]
    fn no_samples_means_no_divergence() {
        let c = ShadowComparator::new(ShadowCompare::ALL.to_vec(), 0.0);
        let d = c.divergence(&[]);
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|d| d.rate() == 0.0));
        assert!(c.check(&[], 3).is_none());
    }

    #[test]
    fn breach_is_reported_for_the_first_attribute_over_threshold() {
        let c = ShadowComparator::new(ShadowCompare::ALL.to_vec(), 0.3);
        let samples = [
            pair(200, 100),
            pair(200, 100),
            pair(200, 100),
            pair(200, 100),
            pair(500, 100),
        ];
        assert!(c.check(&samples, 1).is_none(), "0.2 is under 0.3");
        let samples = [
            pair(200, 100),
            pair(200, 100),
            pair(500, 400),
            pair(500, 400),
        ];
        let breach = c.check(&samples, 1).unwrap();
        assert_eq!(breach.step, 1);
        assert_eq!(
            breach.threshold,
            HealthThreshold::ShadowDivergenceMax {
                compare: ShadowCompare::Status,
                max: 0.3
            }
        );
        assert_eq!(breach.observed, HealthObservation::ShadowDivergence(0.5));
        assert_eq!(
            breach.to_string(),
            "step 1: shadow status divergence max 0.3 breached by divergence rate 0.5"
        );
        let json = serde_json::to_string(&breach).unwrap();
        assert_eq!(serde_json::from_str::<HealthBreach>(&json).unwrap(), breach);
        let latency_only = ShadowComparator::new(vec![ShadowCompare::Latency], 0.3);
        let breach = latency_only.check(&samples, 2).unwrap();
        assert_eq!(
            breach.threshold,
            HealthThreshold::ShadowDivergenceMax {
                compare: ShadowCompare::Latency,
                max: 0.3
            }
        );
        let boundary = ShadowComparator::new(vec![ShadowCompare::Status], 0.5);
        assert!(
            boundary.check(&samples, 0).is_none(),
            "equal to the threshold is not a breach"
        );
    }

    #[test]
    fn comparator_from_template_uses_compare_and_max_divergence() {
        let template = DeployTemplate::parse(
            "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"unused\"\n[rollout]\nstrategy = \"shadow-then-gradual\"\nsteps = [100]\n[shadow]\nenabled = true\nmirror_percent = 5\ncompare = [\"latency\"]\nmax_divergence = 0.2\n",
        )
        .unwrap();
        let c = ShadowComparator::from_template(template.shadow.as_ref().unwrap());
        assert_eq!(c, ShadowComparator::new(vec![ShadowCompare::Latency], 0.2));
        assert!(format!("{c:?}").contains("Latency"));
    }

    #[tokio::test]
    async fn fake_source_scripts_batches_in_order_then_defaults() {
        let source = FakeShadowSource::agreeing(3);
        let bad = vec![pair(500, 100)];
        source.push_after(1, bad.clone());
        source.push(vec![]);
        let slot = Slot::new("slot-1");
        let w = Duration::minutes(1);
        assert_eq!(
            source.sample(&slot, w).await.unwrap(),
            vec![ShadowSample::identical(200, 10); 3]
        );
        assert_eq!(source.sample(&slot, w).await.unwrap(), bad);
        assert!(source.sample(&slot, w).await.unwrap().is_empty());
        assert_eq!(source.sample(&slot, w).await.unwrap().len(), 3);
        assert_eq!(source.calls().len(), 4);
        assert_eq!(source.calls()[0], (slot.clone(), w));
        source.set_failing(true);
        let err = source.sample(&slot, w).await.unwrap_err();
        assert_eq!(err.to_string(), "shadow source failed: scripted failure");
        source.set_failing(false);
        assert!(source.sample(&slot, w).await.is_ok());
        let err = NoShadowSource.sample(&slot, w).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "shadow source failed: no shadow source is configured to observe slot-1"
        );
        let json = serde_json::to_string(&bad[0]).unwrap();
        assert_eq!(serde_json::from_str::<ShadowSample>(&json).unwrap(), bad[0]);
    }
}
