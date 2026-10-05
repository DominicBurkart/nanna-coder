use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Mutex;
use thiserror::Error;

/// A deployed-but-not-necessarily-live instance of an image on the target.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Slot(String);

impl Slot {
    /// A slot named `name` by the target.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The target's name for the slot.
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The adapter operations, for scripting failures and reporting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AdapterOp {
    /// [`TargetAdapter::deploy_inactive`].
    DeployInactive,
    /// [`TargetAdapter::set_traffic`].
    SetTraffic,
    /// [`TargetAdapter::current_image`].
    CurrentImage,
    /// [`TargetAdapter::rollback_to`].
    RollbackTo,
    /// [`TargetAdapter::retire`].
    Retire,
    /// [`TargetAdapter::mirror`].
    Mirror,
    /// [`TargetAdapter::swap`].
    Swap,
    /// [`TargetAdapter::set_fallback`].
    SetFallback,
    /// [`TargetAdapter::clear_fallback`].
    ClearFallback,
}

impl AdapterOp {
    /// Snake-case name of the operation.
    pub const fn name(self) -> &'static str {
        match self {
            AdapterOp::DeployInactive => "deploy_inactive",
            AdapterOp::SetTraffic => "set_traffic",
            AdapterOp::CurrentImage => "current_image",
            AdapterOp::RollbackTo => "rollback_to",
            AdapterOp::Retire => "retire",
            AdapterOp::Mirror => "mirror",
            AdapterOp::Swap => "swap",
            AdapterOp::SetFallback => "set_fallback",
            AdapterOp::ClearFallback => "clear_fallback",
        }
    }
}

impl fmt::Display for AdapterOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A target operation that failed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("target adapter {op} failed: {reason}")]
pub struct AdapterError {
    /// Operation that failed.
    pub op: AdapterOp,
    /// What the target reported.
    pub reason: String,
}

/// The outcome of an atomic [`TargetAdapter::swap`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Swapped {
    /// The slot now serving 100% of live traffic.
    pub active: Slot,
    /// The slot that served before; retained until it is retired.
    pub retired_candidate: Slot,
}

/// Retry `5xx` answers from a new slot against the previous one, once.
///
/// The executor installs this on the new slot for the duration of a
/// rollout so a bad deployment degrades to one extra hop instead of an
/// error page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackPolicy {
    /// How many times a matching answer is retried against the previous slot.
    pub retries: u8,
    /// Lowest status code that triggers a retry.
    pub status_min: u16,
    /// Highest status code that triggers a retry.
    pub status_max: u16,
}

impl Default for FallbackPolicy {
    fn default() -> Self {
        Self {
            retries: 1,
            status_min: 500,
            status_max: 599,
        }
    }
}

impl fmt::Display for FallbackPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "retry {}-{} x{} against the previous slot",
            self.status_min, self.status_max, self.retries
        )
    }
}

/// How faithfully a target honours a [`FallbackPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FallbackSupport {
    /// The edge retries exactly as the policy says.
    Native,
    /// The provider cannot retry at the edge; the policy is recorded and
    /// the previous slot is kept deployable, but a `5xx` may reach clients.
    BestEffort,
}

impl FallbackSupport {
    /// Kebab-case name used in records and the CLI.
    pub const fn name(self) -> &'static str {
        match self {
            FallbackSupport::Native => "native",
            FallbackSupport::BestEffort => "best-effort",
        }
    }
}

impl fmt::Display for FallbackSupport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The effects a rollout has on a deployment target.
///
/// Implementations are provider-specific and live behind cargo features;
/// the core crate ships only [`FakeAdapter`]. Gradual rollouts use
/// `deploy_inactive`, `set_traffic` and `rollback_to`; shadow rollouts add
/// `mirror`; blue/green rollouts add `swap` and `retire`; every rollout
/// may install a [`FallbackPolicy`] on the new slot.
///
/// `split` is the same operation as `set_traffic` under the name the
/// deployment template uses; it is provided here so callers can use either.
///
/// ```
/// use harness::rollout::{AdapterCall, FakeAdapter, Slot, TargetAdapter};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let adapter = FakeAdapter::new("registry.example.invalid/ns/app:v1");
/// let slot = adapter.deploy_inactive("registry.example.invalid/ns/app:v2").await.unwrap();
/// adapter.set_traffic(&slot, 10).await.unwrap();
/// assert_eq!(adapter.traffic(&slot), Some(10));
/// assert_eq!(adapter.current_image().await.unwrap(), "registry.example.invalid/ns/app:v1");
///
/// adapter.split(&slot, 100).await.unwrap();
/// assert_eq!(adapter.current_image().await.unwrap(), "registry.example.invalid/ns/app:v2");
///
/// adapter.rollback_to("registry.example.invalid/ns/app:v1").await.unwrap();
/// assert_eq!(adapter.traffic(&slot), Some(0));
/// adapter.retire(&slot).await.unwrap();
/// assert_eq!(adapter.calls().len(), 7);
/// assert_eq!(adapter.calls()[1], AdapterCall::SetTraffic(Slot::new("slot-1"), 10));
/// # });
/// ```
///
/// Shadow, then blue/green, with a fallback policy on the new slot:
///
/// ```
/// use harness::rollout::{FallbackPolicy, FallbackSupport, FakeAdapter, Slot, TargetAdapter};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let adapter = FakeAdapter::new("registry.example.invalid/ns/app:v1");
/// let green = adapter.deploy_inactive("registry.example.invalid/ns/app:v2").await.unwrap();
/// let support = adapter.set_fallback(&green, &FallbackPolicy::default()).await.unwrap();
/// assert_eq!(support, FallbackSupport::Native);
///
/// adapter.mirror(&green, 5).await.unwrap();
/// assert_eq!(adapter.mirrored(&green), Some(5));
/// assert_eq!(adapter.traffic(&green), Some(0), "mirroring never moves live traffic");
/// assert_eq!(adapter.traffic(&adapter.active()), Some(100));
///
/// let swapped = adapter.swap().await.unwrap();
/// assert_eq!(swapped.active, green);
/// assert_eq!(swapped.retired_candidate, Slot::new("slot-0"));
/// assert_eq!(adapter.current_image().await.unwrap(), "registry.example.invalid/ns/app:v2");
/// assert_eq!(adapter.traffic(&swapped.retired_candidate), Some(0));
///
/// adapter.clear_fallback(&green).await.unwrap();
/// adapter.retire(&swapped.retired_candidate).await.unwrap();
/// assert_eq!(adapter.image_in(&swapped.retired_candidate), None);
/// # });
/// ```
#[async_trait]
pub trait TargetAdapter: Send + Sync {
    /// Deploy `image` to a slot that receives no live traffic.
    async fn deploy_inactive(&self, image: &str) -> Result<Slot, AdapterError>;
    /// Route `percent` of live traffic to `slot`.
    async fn set_traffic(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError>;
    /// [`set_traffic`](Self::set_traffic) under the template's name.
    async fn split(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError> {
        self.set_traffic(slot, percent).await
    }
    /// The image serving 100% of traffic, or the majority of it.
    async fn current_image(&self) -> Result<String, AdapterError>;
    /// Restore `image` at 100%, taking every other slot to zero.
    async fn rollback_to(&self, image: &str) -> Result<(), AdapterError>;
    /// Tear `slot` down. A slot still serving traffic is refused.
    async fn retire(&self, slot: &Slot) -> Result<(), AdapterError>;
    /// Copy `percent` of live requests to `slot`, discarding its answers.
    /// Live traffic is never affected; `0` stops mirroring.
    async fn mirror(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError>;
    /// Atomically exchange the active slot with the most recently deployed
    /// inactive one, so the target never serves from neither.
    async fn swap(&self) -> Result<Swapped, AdapterError>;
    /// Install `policy` so answers from `slot` matching it are retried
    /// against the slot that was active before the rollout. The result
    /// says how faithfully the target can honour it.
    async fn set_fallback(
        &self,
        slot: &Slot,
        policy: &FallbackPolicy,
    ) -> Result<FallbackSupport, AdapterError>;
    /// Remove the fallback installed on `slot`, if any.
    async fn clear_fallback(&self, slot: &Slot) -> Result<(), AdapterError>;
}

/// One call recorded by [`FakeAdapter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterCall {
    /// [`TargetAdapter::deploy_inactive`] with the image.
    DeployInactive(String),
    /// [`TargetAdapter::set_traffic`] with the slot and percent.
    SetTraffic(Slot, u8),
    /// [`TargetAdapter::current_image`].
    CurrentImage,
    /// [`TargetAdapter::rollback_to`] with the image.
    RollbackTo(String),
    /// [`TargetAdapter::retire`] with the slot.
    Retire(Slot),
    /// [`TargetAdapter::mirror`] with the slot and percent.
    Mirror(Slot, u8),
    /// [`TargetAdapter::swap`].
    Swap,
    /// [`TargetAdapter::set_fallback`] with the slot and policy.
    SetFallback(Slot, FallbackPolicy),
    /// [`TargetAdapter::clear_fallback`] with the slot.
    ClearFallback(Slot),
}

#[derive(Debug)]
struct FakeInner {
    calls: Vec<AdapterCall>,
    current_image: String,
    active: Slot,
    images: BTreeMap<Slot, String>,
    traffic: BTreeMap<Slot, u8>,
    max_traffic: BTreeMap<Slot, u8>,
    mirrored: BTreeMap<Slot, u8>,
    fallbacks: BTreeMap<Slot, FallbackPolicy>,
    fallback_support: FallbackSupport,
    failing: BTreeSet<AdapterOp>,
    deployed: usize,
}

/// In-memory target that records every call and can be scripted to fail.
///
/// The image the target starts with lives in `slot-0`, the active slot;
/// deployments are named `slot-1`, `slot-2`, ... in order. Setting a slot
/// to 100% makes it active and its image current; a rollback makes the
/// slot holding the given image active at 100% and zeroes every other
/// slot; a swap exchanges the active slot with the newest inactive one.
#[derive(Debug)]
pub struct FakeAdapter {
    inner: Mutex<FakeInner>,
}

impl FakeAdapter {
    /// A target currently serving `current_image` at 100% from `slot-0`.
    pub fn new(current_image: &str) -> Self {
        let active = Slot::new("slot-0");
        Self {
            inner: Mutex::new(FakeInner {
                calls: Vec::new(),
                current_image: current_image.to_string(),
                active: active.clone(),
                images: BTreeMap::from([(active.clone(), current_image.to_string())]),
                traffic: BTreeMap::from([(active.clone(), 100)]),
                max_traffic: BTreeMap::from([(active, 100)]),
                mirrored: BTreeMap::new(),
                fallbacks: BTreeMap::new(),
                fallback_support: FallbackSupport::Native,
                failing: BTreeSet::new(),
                deployed: 0,
            }),
        }
    }

    /// Every call so far, in order.
    pub fn calls(&self) -> Vec<AdapterCall> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// Make every call to `op` fail until [`succeed`](Self::succeed).
    pub fn fail(&self, op: AdapterOp) {
        self.inner.lock().unwrap().failing.insert(op);
    }

    /// Let `op` succeed again.
    pub fn succeed(&self, op: AdapterOp) {
        self.inner.lock().unwrap().failing.remove(&op);
    }

    /// What [`set_fallback`](TargetAdapter::set_fallback) reports from now on.
    pub fn set_fallback_support(&self, support: FallbackSupport) {
        self.inner.lock().unwrap().fallback_support = support;
    }

    /// Traffic percent currently routed to `slot`, if it exists.
    pub fn traffic(&self, slot: &Slot) -> Option<u8> {
        self.inner.lock().unwrap().traffic.get(slot).copied()
    }

    /// Highest traffic percent ever routed to `slot`.
    pub fn max_traffic(&self, slot: &Slot) -> Option<u8> {
        self.inner.lock().unwrap().max_traffic.get(slot).copied()
    }

    /// Share of live requests currently mirrored to `slot`, if it exists.
    pub fn mirrored(&self, slot: &Slot) -> Option<u8> {
        let inner = self.inner.lock().unwrap();
        inner
            .images
            .contains_key(slot)
            .then(|| inner.mirrored.get(slot).copied().unwrap_or(0))
    }

    /// Fallback policy installed on `slot`, if any.
    pub fn fallback(&self, slot: &Slot) -> Option<FallbackPolicy> {
        self.inner.lock().unwrap().fallbacks.get(slot).cloned()
    }

    /// Image deployed to `slot`, if it exists.
    pub fn image_in(&self, slot: &Slot) -> Option<String> {
        self.inner.lock().unwrap().images.get(slot).cloned()
    }

    /// Image reported as current, without recording a call.
    pub fn current(&self) -> String {
        self.inner.lock().unwrap().current_image.clone()
    }

    /// The slot serving 100% (or the last one that did).
    pub fn active(&self) -> Slot {
        self.inner.lock().unwrap().active.clone()
    }

    fn record(
        &self,
        op: AdapterOp,
        call: AdapterCall,
    ) -> Result<std::sync::MutexGuard<'_, FakeInner>, AdapterError> {
        let mut inner = self.inner.lock().unwrap();
        inner.calls.push(call);
        if inner.failing.contains(&op) {
            return Err(AdapterError {
                op,
                reason: "scripted failure".into(),
            });
        }
        Ok(inner)
    }
}

fn unknown_slot(op: AdapterOp, slot: &Slot) -> AdapterError {
    AdapterError {
        op,
        reason: format!("unknown slot {slot}"),
    }
}

#[async_trait]
impl TargetAdapter for FakeAdapter {
    async fn deploy_inactive(&self, image: &str) -> Result<Slot, AdapterError> {
        let mut inner = self.record(
            AdapterOp::DeployInactive,
            AdapterCall::DeployInactive(image.to_string()),
        )?;
        inner.deployed += 1;
        let slot = Slot::new(format!("slot-{}", inner.deployed));
        inner.images.insert(slot.clone(), image.to_string());
        inner.traffic.insert(slot.clone(), 0);
        inner.max_traffic.insert(slot.clone(), 0);
        Ok(slot)
    }

    async fn set_traffic(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError> {
        let mut inner = self.record(
            AdapterOp::SetTraffic,
            AdapterCall::SetTraffic(slot.clone(), percent),
        )?;
        let Some(image) = inner.images.get(slot).cloned() else {
            return Err(unknown_slot(AdapterOp::SetTraffic, slot));
        };
        inner.traffic.insert(slot.clone(), percent);
        let seen = inner.max_traffic.entry(slot.clone()).or_insert(0);
        *seen = (*seen).max(percent);
        if percent == 100 {
            inner.current_image = image;
            inner.active = slot.clone();
        }
        Ok(())
    }

    async fn current_image(&self) -> Result<String, AdapterError> {
        let inner = self.record(AdapterOp::CurrentImage, AdapterCall::CurrentImage)?;
        Ok(inner.current_image.clone())
    }

    async fn rollback_to(&self, image: &str) -> Result<(), AdapterError> {
        let mut inner = self.record(
            AdapterOp::RollbackTo,
            AdapterCall::RollbackTo(image.to_string()),
        )?;
        inner.current_image = image.to_string();
        let restored: Vec<Slot> = inner
            .images
            .iter()
            .filter(|(_, held)| *held == image)
            .map(|(slot, _)| slot.clone())
            .collect();
        for (slot, percent) in inner.traffic.iter_mut() {
            *percent = if restored.contains(slot) { 100 } else { 0 };
        }
        if let Some(slot) = restored.into_iter().next() {
            inner.active = slot;
        }
        Ok(())
    }

    async fn retire(&self, slot: &Slot) -> Result<(), AdapterError> {
        let mut inner = self.record(AdapterOp::Retire, AdapterCall::Retire(slot.clone()))?;
        match inner.traffic.get(slot) {
            None => return Err(unknown_slot(AdapterOp::Retire, slot)),
            Some(0) => {}
            Some(percent) => {
                return Err(AdapterError {
                    op: AdapterOp::Retire,
                    reason: format!("slot {slot} still serves {percent}% of traffic"),
                })
            }
        }
        inner.images.remove(slot);
        inner.traffic.remove(slot);
        inner.mirrored.remove(slot);
        inner.fallbacks.remove(slot);
        Ok(())
    }

    async fn mirror(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError> {
        let mut inner = self.record(
            AdapterOp::Mirror,
            AdapterCall::Mirror(slot.clone(), percent),
        )?;
        if !inner.images.contains_key(slot) {
            return Err(unknown_slot(AdapterOp::Mirror, slot));
        }
        if percent > 100 {
            return Err(AdapterError {
                op: AdapterOp::Mirror,
                reason: format!("{percent} is not a percentage"),
            });
        }
        inner.mirrored.insert(slot.clone(), percent);
        Ok(())
    }

    async fn swap(&self) -> Result<Swapped, AdapterError> {
        let mut inner = self.record(AdapterOp::Swap, AdapterCall::Swap)?;
        let retired_candidate = inner.active.clone();
        let Some(active) = inner
            .images
            .keys()
            .filter(|slot| **slot != retired_candidate)
            .max_by_key(|slot| slot.name().len())
            .cloned()
        else {
            return Err(AdapterError {
                op: AdapterOp::Swap,
                reason: "no inactive slot to swap in".into(),
            });
        };
        inner.traffic.insert(retired_candidate.clone(), 0);
        inner.traffic.insert(active.clone(), 100);
        inner.max_traffic.insert(active.clone(), 100);
        inner.mirrored.remove(&active);
        inner.current_image = inner.images[&active].clone();
        inner.active = active.clone();
        Ok(Swapped {
            active,
            retired_candidate,
        })
    }

    async fn set_fallback(
        &self,
        slot: &Slot,
        policy: &FallbackPolicy,
    ) -> Result<FallbackSupport, AdapterError> {
        let mut inner = self.record(
            AdapterOp::SetFallback,
            AdapterCall::SetFallback(slot.clone(), policy.clone()),
        )?;
        if !inner.images.contains_key(slot) {
            return Err(unknown_slot(AdapterOp::SetFallback, slot));
        }
        inner.fallbacks.insert(slot.clone(), policy.clone());
        Ok(inner.fallback_support)
    }

    async fn clear_fallback(&self, slot: &Slot) -> Result<(), AdapterError> {
        let mut inner = self.record(
            AdapterOp::ClearFallback,
            AdapterCall::ClearFallback(slot.clone()),
        )?;
        inner.fallbacks.remove(slot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::{prop, prop_assert_eq, prop_oneof, proptest, Just, Strategy as _};

    #[tokio::test]
    async fn fake_tracks_slots_traffic_and_current_image() {
        let adapter = FakeAdapter::new("app:v1");
        let a = adapter.deploy_inactive("app:v2").await.unwrap();
        let b = adapter.deploy_inactive("app:v3").await.unwrap();
        assert_eq!((a.name(), b.name()), ("slot-1", "slot-2"));
        assert_eq!(adapter.image_in(&a).unwrap(), "app:v2");
        assert_eq!(adapter.image_in(&Slot::new("slot-9")), None);
        adapter.set_traffic(&a, 50).await.unwrap();
        adapter.set_traffic(&a, 10).await.unwrap();
        assert_eq!(adapter.traffic(&a), Some(10));
        assert_eq!(adapter.max_traffic(&a), Some(50));
        assert_eq!(adapter.max_traffic(&Slot::new("slot-9")), None);
        assert_eq!(adapter.current(), "app:v1");
        assert_eq!(adapter.active(), Slot::new("slot-0"));
        adapter.set_traffic(&b, 100).await.unwrap();
        assert_eq!(adapter.current_image().await.unwrap(), "app:v3");
        assert_eq!(adapter.active(), b);
        adapter.rollback_to("app:v1").await.unwrap();
        assert_eq!(adapter.current(), "app:v1");
        assert_eq!(adapter.active(), Slot::new("slot-0"));
        assert_eq!(adapter.traffic(&Slot::new("slot-0")), Some(100));
        assert_eq!(adapter.traffic(&a), Some(0));
        assert_eq!(adapter.traffic(&b), Some(0));
        adapter.retire(&b).await.unwrap();
        assert_eq!(adapter.traffic(&b), None);
        assert_eq!(adapter.calls().len(), 8);
        assert_eq!(adapter.calls()[7], AdapterCall::Retire(b.clone()));
        assert_eq!(adapter.calls()[6], AdapterCall::RollbackTo("app:v1".into()));
        assert_eq!(adapter.calls()[5], AdapterCall::CurrentImage);
    }

    #[tokio::test]
    async fn rollback_to_an_image_no_slot_holds_keeps_the_active_slot() {
        let adapter = FakeAdapter::new("app:v1");
        let a = adapter.deploy_inactive("app:v2").await.unwrap();
        adapter.set_traffic(&a, 100).await.unwrap();
        adapter.rollback_to("app:v0").await.unwrap();
        assert_eq!(adapter.current(), "app:v0");
        assert_eq!(adapter.active(), a);
        assert_eq!(adapter.traffic(&a), Some(0));
    }

    #[tokio::test]
    async fn unknown_slots_are_refused() {
        let adapter = FakeAdapter::new("app:v1");
        let ghost = Slot::new("ghost");
        let err = adapter.set_traffic(&ghost, 1).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter set_traffic failed: unknown slot ghost"
        );
        let err = adapter.retire(&ghost).await.unwrap_err();
        assert_eq!(err.op, AdapterOp::Retire);
        let err = adapter.mirror(&ghost, 1).await.unwrap_err();
        assert_eq!(err.op, AdapterOp::Mirror);
        let policy = FallbackPolicy::default();
        let err = adapter.set_fallback(&ghost, &policy).await.unwrap_err();
        assert_eq!(err.op, AdapterOp::SetFallback);
        assert_eq!(adapter.mirrored(&ghost), None);
        assert_eq!(adapter.calls().len(), 4);
    }

    #[tokio::test]
    async fn a_serving_slot_cannot_be_retired() {
        let adapter = FakeAdapter::new("app:v1");
        let err = adapter.retire(&Slot::new("slot-0")).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter retire failed: slot slot-0 still serves 100% of traffic"
        );
        assert_eq!(adapter.image_in(&Slot::new("slot-0")).unwrap(), "app:v1");
    }

    #[tokio::test]
    async fn mirror_copies_traffic_without_moving_it() {
        let adapter = FakeAdapter::new("app:v1");
        let a = adapter.deploy_inactive("app:v2").await.unwrap();
        assert_eq!(adapter.mirrored(&a), Some(0));
        adapter.mirror(&a, 25).await.unwrap();
        assert_eq!(adapter.mirrored(&a), Some(25));
        assert_eq!(adapter.traffic(&a), Some(0));
        assert_eq!(adapter.max_traffic(&a), Some(0));
        assert_eq!(adapter.traffic(&Slot::new("slot-0")), Some(100));
        assert_eq!(adapter.current(), "app:v1");
        let err = adapter.mirror(&a, 101).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter mirror failed: 101 is not a percentage"
        );
        assert_eq!(adapter.mirrored(&a), Some(25));
        adapter.mirror(&a, 0).await.unwrap();
        assert_eq!(adapter.mirrored(&a), Some(0));
        assert_eq!(adapter.calls()[1], AdapterCall::Mirror(a.clone(), 25));
        adapter.mirror(&a, 5).await.unwrap();
        adapter.retire(&a).await.unwrap();
        assert_eq!(adapter.mirrored(&a), None);
    }

    #[tokio::test]
    async fn swap_exchanges_active_and_newest_inactive_atomically() {
        let adapter = FakeAdapter::new("app:v1");
        let err = adapter.swap().await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "target adapter swap failed: no inactive slot to swap in"
        );
        let a = adapter.deploy_inactive("app:v2").await.unwrap();
        adapter.mirror(&a, 10).await.unwrap();
        let swapped = adapter.swap().await.unwrap();
        assert_eq!(swapped.active, a);
        assert_eq!(swapped.retired_candidate, Slot::new("slot-0"));
        assert_eq!(adapter.active(), a);
        assert_eq!(adapter.current(), "app:v2");
        assert_eq!(adapter.traffic(&a), Some(100));
        assert_eq!(adapter.max_traffic(&a), Some(100));
        assert_eq!(adapter.mirrored(&a), Some(0));
        assert_eq!(adapter.traffic(&Slot::new("slot-0")), Some(0));
        let back = adapter.swap().await.unwrap();
        assert_eq!(back.active, Slot::new("slot-0"));
        assert_eq!(back.retired_candidate, a);
        assert_eq!(adapter.current(), "app:v1");
        let b = adapter.deploy_inactive("app:v3").await.unwrap();
        let c = adapter.deploy_inactive("app:v4").await.unwrap();
        assert_eq!(adapter.swap().await.unwrap().active, c);
        assert_eq!(adapter.traffic(&b), Some(0));
        assert_eq!(adapter.calls()[3], AdapterCall::Swap);
        let json = serde_json::to_string(&swapped).unwrap();
        assert_eq!(serde_json::from_str::<Swapped>(&json).unwrap(), swapped);
    }

    #[tokio::test]
    async fn fallback_policy_is_installed_per_slot_with_scripted_support() {
        let adapter = FakeAdapter::new("app:v1");
        let a = adapter.deploy_inactive("app:v2").await.unwrap();
        let policy = FallbackPolicy::default();
        assert_eq!(
            policy.to_string(),
            "retry 500-599 x1 against the previous slot"
        );
        assert_eq!(adapter.fallback(&a), None);
        assert_eq!(
            adapter.set_fallback(&a, &policy).await.unwrap(),
            FallbackSupport::Native
        );
        assert_eq!(adapter.fallback(&a), Some(policy.clone()));
        adapter.set_fallback_support(FallbackSupport::BestEffort);
        assert_eq!(
            adapter.set_fallback(&a, &policy).await.unwrap(),
            FallbackSupport::BestEffort
        );
        adapter.clear_fallback(&a).await.unwrap();
        assert_eq!(adapter.fallback(&a), None);
        adapter.clear_fallback(&Slot::new("ghost")).await.unwrap();
        assert_eq!(
            adapter.calls()[1],
            AdapterCall::SetFallback(a.clone(), policy.clone())
        );
        assert_eq!(
            adapter.calls()[4],
            AdapterCall::ClearFallback(Slot::new("ghost"))
        );
        adapter.set_fallback(&a, &policy).await.unwrap();
        adapter.retire(&a).await.unwrap();
        assert_eq!(adapter.fallback(&a), None);
        assert_eq!(FallbackSupport::Native.to_string(), "native");
        assert_eq!(FallbackSupport::BestEffort.to_string(), "best-effort");
        assert_eq!(
            serde_json::to_string(&FallbackSupport::BestEffort).unwrap(),
            "\"best-effort\""
        );
        let json = serde_json::to_string(&policy).unwrap();
        assert_eq!(
            serde_json::from_str::<FallbackPolicy>(&json).unwrap(),
            policy
        );
    }

    #[tokio::test]
    async fn scripted_failures_apply_per_operation_and_are_recorded() {
        let adapter = FakeAdapter::new("app:v1");
        for op in [
            AdapterOp::DeployInactive,
            AdapterOp::SetTraffic,
            AdapterOp::CurrentImage,
            AdapterOp::RollbackTo,
            AdapterOp::Retire,
            AdapterOp::Mirror,
            AdapterOp::Swap,
            AdapterOp::SetFallback,
            AdapterOp::ClearFallback,
        ] {
            adapter.fail(op);
        }
        let slot = Slot::new("slot-1");
        assert_eq!(
            adapter.deploy_inactive("app:v2").await.unwrap_err().op,
            AdapterOp::DeployInactive
        );
        assert_eq!(
            adapter.set_traffic(&slot, 1).await.unwrap_err().op,
            AdapterOp::SetTraffic
        );
        assert_eq!(
            adapter.current_image().await.unwrap_err().op,
            AdapterOp::CurrentImage
        );
        assert_eq!(
            adapter.rollback_to("app:v1").await.unwrap_err().op,
            AdapterOp::RollbackTo
        );
        assert_eq!(
            adapter.retire(&slot).await.unwrap_err().op,
            AdapterOp::Retire
        );
        assert_eq!(
            adapter.mirror(&slot, 1).await.unwrap_err().op,
            AdapterOp::Mirror
        );
        assert_eq!(adapter.swap().await.unwrap_err().op, AdapterOp::Swap);
        let policy = FallbackPolicy::default();
        assert_eq!(
            adapter.set_fallback(&slot, &policy).await.unwrap_err().op,
            AdapterOp::SetFallback
        );
        assert_eq!(
            adapter.clear_fallback(&slot).await.unwrap_err().op,
            AdapterOp::ClearFallback
        );
        assert_eq!(adapter.calls().len(), 9);
        adapter.succeed(AdapterOp::DeployInactive);
        assert_eq!(adapter.deploy_inactive("app:v2").await.unwrap(), slot);
    }

    #[test]
    fn op_and_slot_display() {
        assert_eq!(AdapterOp::DeployInactive.to_string(), "deploy_inactive");
        assert_eq!(AdapterOp::SetTraffic.to_string(), "set_traffic");
        assert_eq!(AdapterOp::CurrentImage.to_string(), "current_image");
        assert_eq!(AdapterOp::RollbackTo.to_string(), "rollback_to");
        assert_eq!(AdapterOp::Retire.to_string(), "retire");
        assert_eq!(AdapterOp::Mirror.to_string(), "mirror");
        assert_eq!(AdapterOp::Swap.to_string(), "swap");
        assert_eq!(AdapterOp::SetFallback.to_string(), "set_fallback");
        assert_eq!(AdapterOp::ClearFallback.to_string(), "clear_fallback");
        assert_eq!(Slot::new("s").to_string(), "s");
        assert_eq!(serde_json::to_string(&Slot::new("s")).unwrap(), "\"s\"");
    }

    #[derive(Debug, Clone)]
    enum Action {
        Deploy,
        Traffic(u8, u8),
        Mirror(u8, u8),
    }

    fn arb_action() -> impl proptest::strategy::Strategy<Value = Action> {
        prop_oneof![
            Just(Action::Deploy),
            (0u8..4, 0u8..=100).prop_map(|(s, p)| Action::Traffic(s, p)),
            (0u8..4, 0u8..=100).prop_map(|(s, p)| Action::Mirror(s, p)),
        ]
    }

    proptest! {
        #[test]
        fn mirrored_traffic_never_affects_live_traffic(actions in prop::collection::vec(arb_action(), 1..24)) {
            let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let adapter = FakeAdapter::new("app:v1");
            let mut slots = vec![Slot::new("slot-0")];
            for action in actions {
                match action {
                    Action::Deploy => slots.push(runtime.block_on(adapter.deploy_inactive("app:vN")).unwrap()),
                    Action::Traffic(s, p) => runtime.block_on(adapter.set_traffic(&slots[s as usize % slots.len()], p)).unwrap(),
                    Action::Mirror(s, p) => {
                        let slot = slots[s as usize % slots.len()].clone();
                        let active = adapter.active();
                        let before: Vec<Option<u8>> = slots.iter().map(|s| adapter.traffic(s)).collect();
                        let image = adapter.current();
                        runtime.block_on(adapter.mirror(&slot, p)).unwrap();
                        let after: Vec<Option<u8>> = slots.iter().map(|s| adapter.traffic(s)).collect();
                        prop_assert_eq!(before, after);
                        prop_assert_eq!(adapter.active(), active);
                        prop_assert_eq!(adapter.current(), image);
                        prop_assert_eq!(adapter.mirrored(&slot), Some(p));
                    }
                }
            }
            let mirrors = adapter.calls().iter().filter(|c| matches!(c, AdapterCall::Mirror(..))).count();
            let traffic_calls = adapter.calls().iter().filter(|c| matches!(c, AdapterCall::SetTraffic(..))).count();
            let deploys = adapter.calls().iter().filter(|c| matches!(c, AdapterCall::DeployInactive(_))).count();
            prop_assert_eq!(mirrors + traffic_calls + deploys, adapter.calls().len());
        }
    }
}
