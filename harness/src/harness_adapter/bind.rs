use super::ResolvedAgent;
use crate::auditor::Allowed;
use crate::effects::EffectClass;
use crate::identity::AgentIdentity;
use crate::scope::{check_repo, names_repo, ScopeError};
use crate::task::{model_facing_registry, ModelFacingLayers};
use crate::tools::ToolRegistry;
use crate::workspace::TaskWorkspace;
use thiserror::Error;

/// Why an agent could not be bound to a workspace.
///
/// ```
/// use harness::harness_adapter::BindError;
///
/// let error = BindError::AgentMismatch;
/// assert_eq!(error.to_string(), "the agent was not resolved from this audited spawn");
/// ```
#[derive(Debug, Error)]
pub enum BindError {
    /// The audited spawn is not the one the agent was resolved from.
    #[error("the agent was not resolved from this audited spawn")]
    AgentMismatch,
    /// The workspace's source repository is not the one the auditor allowed.
    #[error("workspace repository {target} is not the audited repository {audited}")]
    RepoMismatch {
        /// Repository label from the audit.
        audited: String,
        /// The workspace's source repository.
        target: String,
    },
    /// The repository is outside the identity's `scope.repos`, or the identity's
    /// scope could not be compiled into a registry.
    #[error(transparent)]
    Scope(#[from] ScopeError),
    /// The workspace's registry does not offer exactly the capabilities the
    /// agent was resolved with.
    #[error("workspace registry differs from the agent's capabilities at `{tool}`")]
    CapabilityMismatch {
        /// The first capability that differs.
        tool: String,
    },
}

/// A tool registry built for one audited agent from a real task workspace.
///
/// The only way to obtain one is [`ResolvedAgent::bind_workspace`], which runs
/// the same repository checks as `TaskManager::submit_spawn` and builds the
/// registry through `model_facing_registry`, the constructor `TaskRunner` uses, so the
/// container-or-host choice and the path scope come from the code the runner
/// already uses. There is no way to hand a [`Broker`](super::Broker) a registry
/// built elsewhere:
///
/// ```compile_fail
/// use harness::harness_adapter::BoundWorkspace;
/// use harness::tools::ToolRegistry;
///
/// let _ = BoundWorkspace { registry: ToolRegistry::new() };
/// ```
pub struct BoundWorkspace {
    registry: ToolRegistry,
    identity: AgentIdentity,
    audited_repo: String,
    capabilities: Vec<(String, EffectClass)>,
}

impl BoundWorkspace {
    pub(super) fn matches(&self, allowed: &Allowed, agent: &ResolvedAgent) -> bool {
        self.identity == *allowed.identity()
            && self.audited_repo == allowed.repo()
            && self.capabilities == capability_snapshot(agent)
    }

    pub(super) fn into_registry(self) -> ToolRegistry {
        self.registry
    }

    #[cfg(test)]
    pub(super) fn for_test(
        registry: ToolRegistry,
        allowed: &Allowed,
        agent: &ResolvedAgent,
    ) -> Self {
        Self {
            registry,
            identity: allowed.identity().clone(),
            audited_repo: allowed.repo().to_string(),
            capabilities: capability_snapshot(agent),
        }
    }
}

fn capability_snapshot(agent: &ResolvedAgent) -> Vec<(String, EffectClass)> {
    agent
        .capabilities()
        .iter()
        .map(|c| (c.name.clone(), c.effect))
        .collect()
}

pub(super) fn agent_matches_audit(agent: &ResolvedAgent, allowed: &Allowed) -> bool {
    let identity = allowed.identity();
    let scope = agent.scope();
    let tool_patterns: Vec<String> = identity
        .scope
        .tools
        .iter()
        .map(ToString::to_string)
        .collect();
    let prompt_matches = identity
        .system_prompt_text()
        .is_ok_and(|text| text == agent.system_prompt());
    let capabilities_allowed = agent
        .capabilities()
        .iter()
        .all(|c| identity.allows_tool(&c.name) && identity.allows_effect(c.effect));
    agent.name() == identity.name()
        && scope.repos == identity.scope.repos
        && scope.paths == identity.scope.paths
        && scope.read_paths == identity.scope.read_paths
        && scope.tools == tool_patterns
        && agent.max_effect() == identity.scope.max_effect
        && agent.limits() == identity.limits
        && agent.model() == allowed.model()
        && agent.task_prompt() == allowed.request().subtask
        && agent.audited_repo() == allowed.repo()
        && prompt_matches
        && capabilities_allowed
}

pub(super) fn registry_matches(
    agent: &ResolvedAgent,
    registry: &ToolRegistry,
) -> Result<(), BindError> {
    let mut offered: Vec<String> = registry
        .get_definitions()
        .into_iter()
        .map(|def| def.function.name)
        .collect();
    offered.sort();
    let granted: Vec<&str> = agent.capability_names();
    for name in &offered {
        if !granted.contains(&name.as_str()) {
            return Err(BindError::CapabilityMismatch { tool: name.clone() });
        }
    }
    for capability in agent.capabilities() {
        let same = registry.effect_class_of(&capability.name) == Some(capability.effect);
        if !same {
            return Err(BindError::CapabilityMismatch {
                tool: capability.name.clone(),
            });
        }
    }
    Ok(())
}

impl ResolvedAgent {
    /// Bind this agent to `workspace` for the audited spawn `allowed`.
    ///
    /// Checks, in order: `allowed` is the spawn this agent was resolved from
    /// (identity name, scope and audited repository); the workspace's source
    /// repository is the audited one (`names_repo`) and is in the identity's
    /// `scope.repos` (`check_repo`), exactly as `TaskManager::submit_spawn`
    /// does; then builds the registry with
    /// `model_facing_registry` (identity scope, production hold, action gate and escalator) and asserts that it offers
    /// exactly this agent's capabilities, with the effect classes the agent was
    /// resolved with. Derive the agent's grant from that same constructor's
    /// registry (`ScopedCapabilities::scope`) or the assertion will refuse.
    /// ```
    /// use harness::auditor::Allowed;
    /// use harness::harness_adapter::{BindError, BoundWorkspace, ResolvedAgent};
    /// use harness::task::ModelFacingLayers;
    /// use harness::workspace::TaskWorkspace;
    ///
    /// fn bind(
    ///     agent: &ResolvedAgent,
    ///     allowed: &Allowed,
    ///     workspace: &TaskWorkspace,
    ///     layers: ModelFacingLayers,
    /// ) -> Result<BoundWorkspace, BindError> {
    ///     agent.bind_workspace(allowed, workspace, layers)
    /// }
    /// ```
    pub fn bind_workspace(
        &self,
        allowed: &Allowed,
        workspace: &TaskWorkspace,
        layers: ModelFacingLayers,
    ) -> Result<BoundWorkspace, BindError> {
        let identity = allowed.identity();
        if !agent_matches_audit(self, allowed) {
            return Err(BindError::AgentMismatch);
        }
        let source = &workspace.source_repo;
        if !names_repo(allowed.repo(), source) {
            return Err(BindError::RepoMismatch {
                audited: allowed.repo().to_string(),
                target: source.display().to_string(),
            });
        }
        check_repo(identity, source)?;
        let registry = model_facing_registry(workspace, Some(identity), layers)?;
        registry_matches(self, &registry)?;
        Ok(BoundWorkspace {
            registry,
            identity: identity.clone(),
            audited_repo: allowed.repo().to_string(),
            capabilities: capability_snapshot(self),
        })
    }
}
