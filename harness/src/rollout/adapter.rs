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

/// The effects a rollout has on a deployment target.
///
/// Implementations are provider-specific and live behind cargo features;
/// the core crate ships only [`FakeAdapter`]. Shadow and blue/green
/// primitives (`mirror`, `split`, `swap`) extend this trait later.
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
/// adapter.set_traffic(&slot, 100).await.unwrap();
/// assert_eq!(adapter.current_image().await.unwrap(), "registry.example.invalid/ns/app:v2");
///
/// adapter.rollback_to("registry.example.invalid/ns/app:v1").await.unwrap();
/// assert_eq!(adapter.traffic(&slot), Some(0));
/// adapter.retire(&slot).await.unwrap();
/// assert_eq!(adapter.calls().len(), 7);
/// assert_eq!(adapter.calls()[1], AdapterCall::SetTraffic(Slot::new("slot-1"), 10));
/// # });
/// ```
#[async_trait]
pub trait TargetAdapter: Send + Sync {
    /// Deploy `image` to a slot that receives no live traffic.
    async fn deploy_inactive(&self, image: &str) -> Result<Slot, AdapterError>;
    /// Route `percent` of live traffic to `slot`.
    async fn set_traffic(&self, slot: &Slot, percent: u8) -> Result<(), AdapterError>;
    /// The image serving 100% of traffic, or the majority of it.
    async fn current_image(&self) -> Result<String, AdapterError>;
    /// Restore `image` at 100%, taking every other slot to zero.
    async fn rollback_to(&self, image: &str) -> Result<(), AdapterError>;
    /// Tear `slot` down.
    async fn retire(&self, slot: &Slot) -> Result<(), AdapterError>;
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
}

#[derive(Debug)]
struct FakeInner {
    calls: Vec<AdapterCall>,
    current_image: String,
    images: BTreeMap<Slot, String>,
    traffic: BTreeMap<Slot, u8>,
    max_traffic: BTreeMap<Slot, u8>,
    failing: BTreeSet<AdapterOp>,
    deployed: usize,
}

/// In-memory target that records every call and can be scripted to fail.
///
/// Slots are named `slot-1`, `slot-2`, ... in deployment order. Setting a
/// slot to 100% makes its image current; a rollback makes the given image
/// current and zeroes every slot.
#[derive(Debug)]
pub struct FakeAdapter {
    inner: Mutex<FakeInner>,
}

impl FakeAdapter {
    /// A target currently serving `current_image` at 100%.
    pub fn new(current_image: &str) -> Self {
        Self {
            inner: Mutex::new(FakeInner {
                calls: Vec::new(),
                current_image: current_image.to_string(),
                images: BTreeMap::new(),
                traffic: BTreeMap::new(),
                max_traffic: BTreeMap::new(),
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

    /// Traffic percent currently routed to `slot`, if it exists.
    pub fn traffic(&self, slot: &Slot) -> Option<u8> {
        self.inner.lock().unwrap().traffic.get(slot).copied()
    }

    /// Highest traffic percent ever routed to `slot`.
    pub fn max_traffic(&self, slot: &Slot) -> Option<u8> {
        self.inner.lock().unwrap().max_traffic.get(slot).copied()
    }

    /// Image deployed to `slot`, if it exists.
    pub fn image_in(&self, slot: &Slot) -> Option<String> {
        self.inner.lock().unwrap().images.get(slot).cloned()
    }

    /// Image reported as current, without recording a call.
    pub fn current(&self) -> String {
        self.inner.lock().unwrap().current_image.clone()
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
            return Err(AdapterError {
                op: AdapterOp::SetTraffic,
                reason: format!("unknown slot {slot}"),
            });
        };
        inner.traffic.insert(slot.clone(), percent);
        let seen = inner.max_traffic.entry(slot.clone()).or_insert(0);
        *seen = (*seen).max(percent);
        if percent == 100 {
            inner.current_image = image;
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
        for percent in inner.traffic.values_mut() {
            *percent = 0;
        }
        Ok(())
    }

    async fn retire(&self, slot: &Slot) -> Result<(), AdapterError> {
        let mut inner = self.record(AdapterOp::Retire, AdapterCall::Retire(slot.clone()))?;
        if inner.images.remove(slot).is_none() {
            return Err(AdapterError {
                op: AdapterOp::Retire,
                reason: format!("unknown slot {slot}"),
            });
        }
        inner.traffic.remove(slot);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        adapter.set_traffic(&b, 100).await.unwrap();
        assert_eq!(adapter.current_image().await.unwrap(), "app:v3");
        adapter.rollback_to("app:v1").await.unwrap();
        assert_eq!(adapter.current(), "app:v1");
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
        assert_eq!(adapter.calls().len(), 2);
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
        assert_eq!(adapter.calls().len(), 5);
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
        assert_eq!(Slot::new("s").to_string(), "s");
        assert_eq!(serde_json::to_string(&Slot::new("s")).unwrap(), "\"s\"");
    }
}
