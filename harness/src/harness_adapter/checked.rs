use super::{IsolationPolicy, IsolationViolation, LaunchPlan, ResolvedAgent, Unsupported};
use std::ops::Deref;
use thiserror::Error;

/// A [`LaunchPlan`] that has passed [`IsolationPolicy`] for a specific agent.
///
/// The field is private to this module and [`launch_plan`] is the only code
/// here that builds one, so executors that accept only `CheckedPlan` cannot be
/// handed an unchecked plan, including by a sibling adapter module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedPlan(LaunchPlan);

impl Deref for CheckedPlan {
    type Target = LaunchPlan;

    fn deref(&self) -> &LaunchPlan {
        &self.0
    }
}

/// Why a launch plan was refused.
#[derive(Debug, Error)]
pub enum PlanError {
    /// The adapter cannot honour the agent definition.
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    /// The adapter produced a plan that breaks the isolation policy.
    #[error(transparent)]
    Isolation(#[from] IsolationViolation),
}

/// Translates a [`ResolvedAgent`] into a [`LaunchPlan`] for one runtime.
///
/// Implementations are pure: no I/O, no clock, no environment. Their output
/// is a draft; only [`launch_plan`] turns it into a [`CheckedPlan`].
pub trait HarnessAdapter {
    /// Build the draft plan, or refuse with [`Unsupported`].
    fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported>;
}

/// Build a plan with `adapter` and verify it against [`IsolationPolicy`]. This
/// is the only way to obtain a [`CheckedPlan`].
pub fn launch_plan(
    adapter: &dyn HarnessAdapter,
    agent: &ResolvedAgent,
) -> Result<CheckedPlan, PlanError> {
    let plan = adapter.plan(agent)?;
    IsolationPolicy::check(&plan, agent)?;
    Ok(CheckedPlan(plan))
}
