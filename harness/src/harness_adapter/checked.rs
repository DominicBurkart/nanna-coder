use super::{IsolationPolicy, IsolationViolation, LaunchPlan, ResolvedAgent, Unsupported};
use std::ops::Deref;
use thiserror::Error;

/// A [`LaunchPlan`] that has passed [`IsolationPolicy`] for a specific agent.
///
/// The field is private to this module and [`launch_plan`] is the only code
/// here that builds one, so an executor that accepts only `CheckedPlan` cannot
/// be handed a plan that skipped the policy, including by a sibling adapter
/// module. This is a structural guarantee only: [`IsolationPolicy`] checks the
/// plan's shape (image, broker socket, gateway, env pairs, file locations,
/// limits, scope, exposed capabilities). The argv, stdin and file contents are
/// pinned per adapter by that adapter's tests, not by the policy, so a custom
/// adapter can pass the policy with an arbitrary command.
///
/// A `CheckedPlan` cannot be constructed outside `launch_plan`:
///
/// ```compile_fail
/// use harness::harness_adapter::{CheckedPlan, LaunchPlan};
///
/// fn forge(plan: LaunchPlan) -> CheckedPlan {
///     CheckedPlan(plan)
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedPlan(LaunchPlan);

impl Deref for CheckedPlan {
    type Target = LaunchPlan;

    fn deref(&self) -> &LaunchPlan {
        &self.0
    }
}

/// Why a launch plan was refused.
///
/// ```
/// use harness::harness_adapter::{IsolationViolation, PlanError, Unsupported};
///
/// let refused = PlanError::from(IsolationViolation::EmptyArgv);
/// assert_eq!(refused.to_string(), "plan has an empty argv");
/// let unsupported = PlanError::from(Unsupported::NoCapabilities { agent: "a".into() });
/// assert_eq!(unsupported.to_string(), "agent `a` has no capabilities");
/// ```
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
///
/// ```
/// use harness::harness_adapter::{HarnessAdapter, LaunchPlan, ResolvedAgent, Unsupported};
///
/// struct Refuses;
///
/// impl HarnessAdapter for Refuses {
///     fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported> {
///         Err(Unsupported::NoCapabilities { agent: agent.name().to_string() })
///     }
/// }
/// ```
pub trait HarnessAdapter {
    /// Build the draft plan, or refuse with [`Unsupported`].
    fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported>;
}

/// Build a plan with `adapter` and verify it against [`IsolationPolicy`]. This
/// is the only way to obtain a [`CheckedPlan`].
///
/// ```
/// # #[tokio::main]
/// # async fn main() {
/// # use harness::auditor::{AuditContext, AuditLog, Gate, RuleAuditor, SpawnRequest, TaskSummary};
/// # use harness::effects::EffectClass;
/// # use harness::harness_adapter::{launch_plan, Endpoint, PiAdapter, PlanError, ResolvedAgent, ScopedCapabilities};
/// # use harness::identity::{DevLoop, IdentityCatalog};
/// # use harness::tools::create_tool_registry;
/// # let dir = tempfile::tempdir().unwrap();
/// # let card = |name: &str, tools: &str| format!(
/// #     "[identity]\nname = \"{name}\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\n\
/// #      system_prompt = {{ inline = \"p\" }}\n\n[scope]\nrepos = []\npaths = [\"**\"]\n\
/// #      max_effect = \"none\"\ntools = [{tools}]\n\n[limits]\nmax_iterations = 1\n\
/// #      max_wall_clock_secs = 1\nmax_concurrent = 1\n"
/// # );
/// # std::fs::write(dir.path().join("reader.toml"), card("reader", "\"read_file\"")).unwrap();
/// # std::fs::write(dir.path().join("idle.toml"), card("idle", "\"nothing\"")).unwrap();
/// # std::fs::write(dir.path().join("auditor.toml"), card("auditor", "")).unwrap();
/// # let catalog = IdentityCatalog::load(dir.path()).unwrap();
/// # let context = AuditContext::new(catalog.clone(), catalog.get("auditor").unwrap().clone()).unwrap();
/// # let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
/// # let mut agents = Vec::new();
/// # for name in ["reader", "idle"] {
/// #     let request = SpawnRequest {
/// #         parent_task: TaskSummary::new("t1", "Read code", "github.com/example/repo"),
/// #         identity: name.to_string(),
/// #         subtask: "Summarise src/lib.rs.".to_string(),
/// #         dev_loop: DevLoop::Inner,
/// #         requested_effect: EffectClass::None,
/// #     };
/// #     let allowed = gate.check(request, &context).await.unwrap();
/// #     let scoped = ScopedCapabilities::scope(create_tool_registry(std::path::Path::new(".")), allowed.identity());
/// #     agents.push(ResolvedAgent::resolve(
/// #         &allowed, &scoped, Endpoint::new("model-gateway", 11434).unwrap(), "nanna-agent:pi",
/// #         "/run/nanna/broker.sock".into(),
/// #     ).unwrap());
/// # }
/// let plan = launch_plan(&PiAdapter, &agents[0]).unwrap();
/// assert_eq!(plan.exposed_capabilities, vec!["read_file"]);
/// assert_eq!(plan.gateway, *agents[0].endpoint());
///
/// let refused = launch_plan(&PiAdapter, &agents[1]).unwrap_err();
/// assert!(matches!(refused, PlanError::Unsupported(_)));
/// # }
/// ```
pub fn launch_plan(
    adapter: &dyn HarnessAdapter,
    agent: &ResolvedAgent,
) -> Result<CheckedPlan, PlanError> {
    let plan = adapter.plan(agent)?;
    IsolationPolicy::check(&plan, agent)?;
    Ok(CheckedPlan(plan))
}
