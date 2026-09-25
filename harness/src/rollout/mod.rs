//! Resumable gradual rollout executor.
//!
//! [`RolloutExecutor`] drives a [`DeployPlan`](crate::deploy::DeployPlan)
//! step by step: it takes the plan's deploy lease, waits for the plan's
//! availability window, lets an [`AuditHook`] review the step, applies the
//! step's traffic percent through a [`TargetAdapter`], bakes while polling a
//! [`HealthSource`], and then advances or applies the template's
//! `[rollback].on_breach`. Every transition is appended to a JSON Lines log
//! ([`RolloutLog`]) so a crashed harness or an expired window resumes at
//! the same step, and a human can halt any rollout from the CLI
//! (`nanna deploy halt <id>`); no agent tool exposes the kill switch.
//!
//! The executor is provider-agnostic: adapters for real targets live behind
//! cargo features (`serverless-adapter`) and the fixture target is
//! [`FakeAdapter`], which records every call.

mod adapter;
mod health;

pub use adapter::{AdapterCall, AdapterError, AdapterOp, FakeAdapter, Slot, TargetAdapter};
pub use health::{
    check_health, FakeHealthSource, HealthBreach, HealthError, HealthObservation, HealthSample,
    HealthSource, HealthThreshold,
};
