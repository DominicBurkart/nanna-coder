use super::ResolvedAgent;
use crate::effects::EffectClass;
use crate::identity::AgentIdentity;
use crate::scope::{check_repo, names_repo, origin_slug, ScopeError};
use crate::task::{model_facing_registry, BoundIdentity, ModelFacingLayers, TaskId};
use crate::tools::ToolRegistry;
use crate::workspace::TaskWorkspace;
use std::path::Path;
use thiserror::Error;

/// Why an agent could not be bound to a workspace.
#[derive(Debug, Error)]
pub(crate) enum BindError {
    /// The task's identity carries no audit proof, so no broker may serve it.
    #[error("the task's identity was not bound through the spawn gate")]
    Unaudited,
    /// The agent was not resolved from this identity and audited repository.
    #[error("the agent was not resolved from this identity and repository")]
    AgentMismatch,
    /// The workspace's source repository is not the audited one.
    #[error("workspace repository {target} is not the audited repository {audited}")]
    RepoMismatch {
        /// Repository label from the audit.
        audited: String,
        /// The workspace's source repository.
        target: String,
    },
    /// The layers handed to the bind describe a different task, ceiling,
    /// path scope or repository than the identity and workspace.
    #[error("the model-facing layers do not belong to this task: {field} differs")]
    LayersMismatch {
        /// The field that differs.
        field: &'static str,
    },
    /// The repository is outside the identity's `scope.repos`, or the
    /// identity's scope could not be compiled into a registry.
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
/// Only [`ResolvedAgent::bind_workspace`] builds one, so a registry built any
/// other way cannot reach a broker.
pub(crate) struct BoundWorkspace {
    registry: ToolRegistry,
    identity: AgentIdentity,
    audited_repo: String,
    task_id: TaskId,
    capabilities: Vec<(String, EffectClass)>,
}

impl BoundWorkspace {
    pub(crate) fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub(super) fn agent_matches(&self, agent: &ResolvedAgent) -> bool {
        agent_matches_identity(agent, &self.identity, &self.audited_repo)
    }

    pub(super) fn capabilities_match(&self, agent: &ResolvedAgent) -> bool {
        self.capabilities == capability_snapshot(agent)
    }

    pub(super) fn into_registry(self) -> ToolRegistry {
        self.registry
    }

    #[cfg(test)]
    pub(super) fn for_test(
        registry: ToolRegistry,
        identity: &AgentIdentity,
        audited_repo: &str,
        agent: &ResolvedAgent,
    ) -> Self {
        Self {
            registry,
            identity: identity.clone(),
            audited_repo: audited_repo.to_string(),
            task_id: TaskId("t-test".to_string()),
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

pub(super) fn agent_matches_identity(
    agent: &ResolvedAgent,
    identity: &AgentIdentity,
    audited_repo: &str,
) -> bool {
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
        && agent.model() == identity.identity.model
        && agent.audited_repo() == audited_repo
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

#[derive(Debug)]
pub(crate) struct AuditedRef<'a> {
    identity: &'a AgentIdentity,
}

impl AuditedRef<'_> {
    #[cfg(test)]
    pub(crate) fn for_test(identity: &AgentIdentity) -> AuditedRef<'_> {
        AuditedRef { identity }
    }
}

pub(crate) fn require_audited(bound: &BoundIdentity) -> Result<AuditedRef<'_>, BindError> {
    let unaudited = matches!(bound, BoundIdentity::Unaudited(_));
    let identity = bound.identity();
    (!unaudited)
        .then_some(AuditedRef { identity })
        .ok_or(BindError::Unaudited)
}

pub(super) fn subject_repo(source: &Path) -> String {
    let slug = origin_slug(source).unwrap_or_default();
    let (_, name) = slug.split_once('/').unwrap_or(("", ""));
    if name.is_empty() {
        source.display().to_string()
    } else {
        name.to_string()
    }
}

fn layers_match(
    identity: &AgentIdentity,
    workspace: &TaskWorkspace,
    layers: &ModelFacingLayers,
) -> Result<(), BindError> {
    let subject = &layers.subject;
    let checks: [(&'static str, bool); 4] = [
        ("task id", subject.task_id.0 == workspace.task_id),
        (
            "effect ceiling",
            subject.max_effect == identity.scope.max_effect,
        ),
        ("path scope", subject.paths == identity.scope.paths),
        (
            "repository",
            subject.repo == subject_repo(&workspace.source_repo),
        ),
    ];
    match checks.iter().find(|(_, same)| !same) {
        Some((field, _)) => Err(BindError::LayersMismatch { field }),
        None => Ok(()),
    }
}

impl ResolvedAgent {
    pub(crate) fn bind_workspace(
        &self,
        audited: AuditedRef<'_>,
        audited_repo: &str,
        workspace: &TaskWorkspace,
        layers: ModelFacingLayers,
    ) -> Result<BoundWorkspace, BindError> {
        let identity = audited.identity;
        if !agent_matches_identity(self, identity, audited_repo) {
            return Err(BindError::AgentMismatch);
        }
        let source = &workspace.source_repo;
        if !names_repo(audited_repo, source) {
            return Err(BindError::RepoMismatch {
                audited: audited_repo.to_string(),
                target: source.display().to_string(),
            });
        }
        check_repo(identity, source)?;
        layers_match(identity, workspace, &layers)?;
        let task_id = layers.subject.task_id.clone();
        let registry = model_facing_registry(workspace, Some(identity), layers)?;
        registry_matches(self, &registry)?;
        Ok(BoundWorkspace {
            registry,
            identity: identity.clone(),
            audited_repo: audited_repo.to_string(),
            task_id,
            capabilities: capability_snapshot(self),
        })
    }
}
