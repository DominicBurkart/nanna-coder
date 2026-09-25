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
mod executor;
mod health;
mod hooks;
mod log;
#[cfg(feature = "serverless-adapter")]
mod serverless;
mod state;

pub use adapter::{AdapterCall, AdapterError, AdapterOp, FakeAdapter, Slot, TargetAdapter};
pub use executor::{fake_executor, RolloutConfig, RolloutExecutor};
pub use health::{
    check_health, FakeHealthSource, HealthBreach, HealthError, HealthObservation, HealthSample,
    HealthSource, HealthThreshold,
};
pub use hooks::{
    AuditDenied, AuditHook, EscalationHook, LogEscalation, NoAudit, RecordingAudit,
    RecordingEscalation, RolloutEscalation,
};
pub use log::{
    default_rollout_path, rollout_path_from, RolloutLog, RolloutTransition, ROLLOUT_PATH_ENV,
};
#[cfg(feature = "serverless-adapter")]
pub use serverless::{
    CommandOutput, CommandRunner, ProcessRunner, ServerlessAdapter, ServerlessConfig,
    SERVERLESS_ENV,
};
pub use state::{RolloutRecord, RolloutState};

use crate::leases::LeaseError;
use crate::windows::WindowError;
use thiserror::Error;

/// Errors produced while starting, driving or inspecting a rollout.
#[derive(Debug, Error)]
pub enum RolloutError {
    /// The rollout log could not be read or written.
    #[error("rollout log I/O error: {0}")]
    Io(String),
    /// A persisted record could not be decoded.
    #[error("rollout log record is not valid JSON: {0}")]
    Serde(String),
    /// No rollout with this id is in the log.
    #[error("unknown rollout `{0}`")]
    UnknownRollout(String),
    /// The requested state does not follow the current one.
    #[error("rollout {id} cannot move from {from} to {to}")]
    InvalidTransition {
        /// Rollout concerned.
        id: String,
        /// Its current state.
        from: RolloutState,
        /// The refused next state.
        to: RolloutState,
    },
    /// A traffic percent above what the current step permits.
    #[error("rollout {id} asked for {requested}% traffic but step allows at most {ceiling}%")]
    TrafficExceedsStep {
        /// Rollout concerned.
        id: String,
        /// Percent that was asked for.
        requested: u8,
        /// Highest percent the state permits.
        ceiling: u8,
    },
    /// The plan's lease string is not `deploy:<repo>:<env>`.
    #[error("plan lease `{0}` is not of the form deploy:<repo>:<env>")]
    BadLeaseName(String),
    /// A step kind this executor does not implement yet.
    #[error("step kind `{0}` is not supported by the rollout executor yet")]
    UnsupportedStep(&'static str),
    /// A step beyond the first found no deployed slot to route to.
    #[error("rollout {0} has no deployed slot")]
    NoSlot(String),
    /// A persisted state names a step the plan does not have.
    #[error("rollout {id} is at step {step}, which its plan does not have")]
    NoSuchStep {
        /// Rollout concerned.
        id: String,
        /// The missing step index.
        step: usize,
    },
    /// A roll-forward without the pull request that justifies it.
    #[error("roll-forward requires a linked pull request reference")]
    PrRequired,
    /// The target adapter failed.
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    /// The health source failed.
    #[error(transparent)]
    Health(#[from] HealthError),
    /// The lease store failed.
    #[error(transparent)]
    Lease(#[from] LeaseError),
    /// The window set failed.
    #[error(transparent)]
    Window(#[from] WindowError),
    /// The escalation hook failed after the rollout was halted.
    #[error("rollout halted but escalation failed: {0}")]
    Escalation(String),
}

impl From<std::io::Error> for RolloutError {
    fn from(e: std::io::Error) -> Self {
        RolloutError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for RolloutError {
    fn from(e: serde_json::Error) -> Self {
        RolloutError::Serde(e.to_string())
    }
}
