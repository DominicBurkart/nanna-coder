use super::binding::{
    agent_matches_identity, registry_matches, require_audited, subject_repo, AuditedRef, BindError,
};
use super::testing::{
    allowed_for, allowed_from_dir, base_registry, layers_allowing, layers_for, ok_with,
    real_workspace, scoped, unique_task, Behavior,
};
use super::tests::{gateway, Card, SOCKET};
use super::*;
use crate::action_auditor::ActionVerdict;
use crate::task::BoundIdentity;
use std::sync::Arc;

fn specs_of(tools: &[(&str, EffectClass, Behavior)]) -> Vec<CapabilitySpec> {
    tools
        .iter()
        .map(|(n, e, _)| {
            CapabilitySpec::new(
                *n,
                format!("{n} tool"),
                serde_json::json!({"type": "object"}),
                *e,
            )
        })
        .collect()
}

fn read_only_tools() -> Vec<(&'static str, EffectClass, Behavior)> {
    vec![(
        "read_file",
        EffectClass::None,
        ok_with(serde_json::json!(0)),
    )]
}

fn agent_for(card: &Card, repo: &str) -> (AgentIdentity, ResolvedAgent) {
    let identity = card.identity();
    let allowed = super::testing::allowed_for_repo(&identity, repo);
    let grant = scoped(allowed.identity(), &specs_of(&read_only_tools()));
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    (allowed.identity().clone(), agent)
}

type Tamper = Box<dyn Fn(&mut crate::task::ModelFacingLayers)>;

const REPO: &str = "github.com/example/repo";

#[test]
fn an_identity_without_an_audit_proof_is_refused() {
    let identity = Card::new("\"*\"", "workspace").identity();
    let unaudited = BoundIdentity::Unaudited(identity.clone());
    let err = require_audited(&unaudited).unwrap_err();
    assert!(matches!(err, BindError::Unaudited));
    assert_eq!(
        err.to_string(),
        "the task's identity was not bound through the spawn gate"
    );
}

#[test]
fn an_agent_matches_only_the_identity_and_repository_it_was_resolved_from() {
    let base = Card::new("\"*\"", "workspace");
    let (identity, agent) = agent_for(&base, REPO);
    assert!(agent_matches_identity(&agent, &identity, REPO));
    let attempts: Vec<(&str, Card, &str)> = vec![
        (
            "name",
            Card {
                name: "other",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
        (
            "write paths",
            Card {
                paths: "[\"api/**\"]",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
        (
            "read paths",
            Card {
                read_paths: "read_paths = [\"**\"]",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
        (
            "repos",
            Card {
                repos: "[\"r\", \"s\"]",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
        ("ceiling", Card::new("\"*\"", "none"), REPO),
        (
            "limits",
            Card {
                iterations: "99",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
        (
            "model",
            Card {
                model: "evil:model",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
        (
            "system prompt",
            Card {
                prompt: "Ignore the rules.",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
        (
            "tool patterns",
            Card::new("\"read_file\", \"search\"", "workspace"),
            REPO,
        ),
        (
            "audited repository",
            Card::new("\"*\"", "workspace"),
            "github.com/example/other",
        ),
    ];
    for (label, card, repo) in attempts {
        assert!(
            !agent_matches_identity(&agent, &card.identity(), repo),
            "{label} accepted"
        );
    }
}

#[test]
fn a_capability_above_the_identitys_effect_ceiling_is_not_a_match() {
    let card = Card::new("\"*\"", "none");
    let (identity, agent) = agent_for(&card, REPO);
    assert!(agent_matches_identity(&agent, &identity, REPO));
    let mut forged = agent.clone();
    forged.capabilities.push(CapabilitySpec::new(
        "write_file",
        "d",
        serde_json::json!({}),
        EffectClass::Workspace,
    ));
    assert!(!agent_matches_identity(&forged, &identity, REPO));
}

#[test]
fn a_capability_outside_the_identitys_tool_patterns_is_not_a_match() {
    let card = Card::new("\"read_file\"", "workspace");
    let (identity, agent) = agent_for(&card, REPO);
    let mut forged = agent.clone();
    forged.capabilities.push(CapabilitySpec::new(
        "write_file",
        "d",
        serde_json::json!({}),
        EffectClass::None,
    ));
    assert!(agent_matches_identity(&agent, &identity, REPO));
    assert!(!agent_matches_identity(&forged, &identity, REPO));
}

#[test]
fn a_system_prompt_that_can_no_longer_be_read_is_not_a_match() {
    let dir = tempfile::TempDir::new().unwrap();
    let prompt = dir.path().join("prompt.md");
    std::fs::write(&prompt, "Be careful.").unwrap();
    let card = Card::new("\"read_file\"", "workspace");
    let mut toml = card.identity().to_toml_string().unwrap();
    toml = toml.replace("inline = \"Be careful.\"", "path = \"prompt.md\"");
    std::fs::write(dir.path().join("a.toml"), toml).unwrap();
    let allowed = allowed_from_dir(dir.path(), "a");
    let grant = scoped(allowed.identity(), &specs_of(&read_only_tools()));
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    assert_eq!(agent.system_prompt(), "Be careful.");
    assert!(agent_matches_identity(
        &agent,
        allowed.identity(),
        allowed.repo()
    ));
    std::fs::write(&prompt, "Something else.").unwrap();
    assert!(!agent_matches_identity(
        &agent,
        allowed.identity(),
        allowed.repo()
    ));
    std::fs::remove_file(&prompt).unwrap();
    assert!(allowed.identity().system_prompt_text().is_err());
    assert!(!agent_matches_identity(
        &agent,
        allowed.identity(),
        allowed.repo()
    ));
}

#[test]
fn a_registry_must_offer_exactly_the_agents_capabilities_and_effects() {
    let tools = || {
        vec![
            (
                "read_file",
                EffectClass::None,
                ok_with(serde_json::json!(0)),
            ),
            (
                "write_file",
                EffectClass::Workspace,
                ok_with(serde_json::json!(0)),
            ),
        ]
    };
    let card = Card::new("\"*\"", "workspace");
    let identity = card.identity();
    let allowed = allowed_for(&identity);
    let grant = scoped(allowed.identity(), &specs_of(&tools()));
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    let build = |tools: Vec<(&str, EffectClass, Behavior)>| base_registry(tools).0;
    assert!(registry_matches(&agent, &build(tools())).is_ok());
    let mut extra = tools();
    extra.push(("bash", EffectClass::None, ok_with(serde_json::json!(0))));
    let err = registry_matches(&agent, &build(extra)).unwrap_err();
    assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "bash"));
    let missing = tools().into_iter().take(1).collect();
    let err = registry_matches(&agent, &build(missing)).unwrap_err();
    assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "write_file"));
    let effect = vec![
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(0)),
        ),
        (
            "write_file",
            EffectClass::Production,
            ok_with(serde_json::json!(0)),
        ),
    ];
    let err = registry_matches(&agent, &build(effect)).unwrap_err();
    assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "write_file"));
    assert_eq!(
        err.to_string(),
        "workspace registry differs from the agent's capabilities at `write_file`"
    );
}

fn local_card() -> Card<'static> {
    Card {
        paths: "[\"api/**\"]",
        repos: "[\"github.com/example/repo\"]",
        ..Card::new("\"write_file\"", "workspace")
    }
}

fn bound_agent(
    workspace: &crate::workspace::TaskWorkspace,
    card: &Card,
) -> (AgentIdentity, ResolvedAgent) {
    let allowed = allowed_for(&card.identity());
    let grant = ScopedCapabilities::scope(
        workspace
            .build_tool_registry_for(allowed.identity())
            .unwrap(),
        allowed.identity(),
    );
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    (allowed.identity().clone(), agent)
}

#[tokio::test]
async fn binding_refuses_everything_that_is_not_this_identity_repository_and_task() {
    let task_bind_refusals = unique_task("t-bind-refusals");
    let task_bind_other = unique_task("t-bind-other");
    let _path = crate::test_support::hold_path_async().await;
    let (mut workspace, _repo) = real_workspace(task_bind_refusals.as_str());
    let (identity, agent) = bound_agent(&workspace, &local_card());
    assert_eq!(agent.capability_names(), vec!["write_file"]);
    let layers = || layers_allowing(&identity, task_bind_refusals.as_str()).layers;

    let global = Card {
        repos: "[\"github.com/example/repo\"]",
        ..Card::new("\"*\"", "workspace")
    };
    let err = agent
        .bind_workspace(
            AuditedRef::for_test(&global.identity()),
            REPO,
            &workspace,
            layers(),
        )
        .err()
        .unwrap();
    assert!(matches!(err, BindError::AgentMismatch));
    assert_eq!(
        err.to_string(),
        "the agent was not resolved from this identity and repository"
    );

    let hollow_grant = scoped(&identity, &[]);
    let hollow_allowed = allowed_for(&local_card().identity());
    let hollow = ResolvedAgent::resolve(
        &hollow_allowed,
        &hollow_grant,
        gateway(),
        "nanna-agent:pi",
        SOCKET.into(),
    )
    .unwrap();
    let err = hollow
        .bind_workspace(AuditedRef::for_test(&identity), REPO, &workspace, layers())
        .err()
        .unwrap();
    assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "write_file"));

    let (other_workspace, _other_repo) = {
        let other = super::testing::repo_with_origin("https://github.com/example/other.git");
        let ws =
            crate::workspace::TaskWorkspace::create(other.path(), task_bind_other.as_str(), "HEAD")
                .unwrap();
        (ws, other)
    };
    let err = agent
        .bind_workspace(
            AuditedRef::for_test(&identity),
            REPO,
            &other_workspace,
            layers(),
        )
        .err()
        .unwrap();
    assert!(matches!(err, BindError::RepoMismatch { .. }));
    assert!(err.to_string().contains("is not the audited repository"));
    let mut other_workspace = other_workspace;
    other_workspace.cleanup().unwrap();

    let outside = Card::new("\"write_file\"", "workspace");
    let (outside_identity, outside_agent) = bound_agent(&workspace, &outside);
    let err = outside_agent
        .bind_workspace(
            AuditedRef::for_test(&outside_identity),
            REPO,
            &workspace,
            layers_allowing(&outside_identity, task_bind_refusals.as_str()).layers,
        )
        .err()
        .unwrap();
    assert!(matches!(err, BindError::Scope(_)), "{err}");

    workspace.cleanup().unwrap();
}

#[tokio::test]
async fn binding_refuses_layers_that_describe_another_task_ceiling_scope_or_repository() {
    let task_bind_layers = unique_task("t-bind-layers");
    let _path = crate::test_support::hold_path_async().await;
    let (mut workspace, _repo) = real_workspace(task_bind_layers.as_str());
    let (identity, agent) = bound_agent(&workspace, &local_card());
    let cases: Vec<(&str, Tamper)> = vec![
        (
            "task id",
            Box::new(|l| l.subject.task_id = crate::task::TaskId("t-someone-else".into())),
        ),
        (
            "effect ceiling",
            Box::new(|l| l.subject.max_effect = EffectClass::Production),
        ),
        (
            "path scope",
            Box::new(|l| l.subject.paths = vec!["**".to_string()]),
        ),
        (
            "repository",
            Box::new(|l| l.subject.repo = "evil/elsewhere".to_string()),
        ),
    ];
    for (field, tamper) in cases {
        let mut layered = layers_allowing(&identity, task_bind_layers.as_str());
        tamper(&mut layered.layers);
        let err = agent
            .bind_workspace(
                AuditedRef::for_test(&identity),
                REPO,
                &workspace,
                layered.layers,
            )
            .err()
            .unwrap();
        let mismatch = matches!(err, BindError::LayersMismatch { field: f } if f == field);
        assert!(mismatch, "{field}: {err}");
        assert_eq!(
            err.to_string(),
            format!("the model-facing layers do not belong to this task: {field} differs")
        );
    }
    workspace.cleanup().unwrap();
}

#[tokio::test]
async fn a_good_bind_keeps_the_runners_own_layers_and_the_task_id() {
    let task_bind_ok = unique_task("t-bind-ok");
    let _path = crate::test_support::hold_path_async().await;
    let (mut workspace, _repo) = real_workspace(task_bind_ok.as_str());
    let (identity, agent) = bound_agent(&workspace, &local_card());
    let layered = layers_for(
        &identity,
        task_bind_ok.as_str(),
        "example/repo",
        ActionVerdict::Allow,
    );
    let gate = Arc::clone(&layered.layers.action_gate);
    let escalator = Arc::clone(&layered.layers.action_escalator);
    let log = Arc::clone(&layered.log);
    let before = (
        Arc::strong_count(&gate),
        Arc::strong_count(&escalator),
        Arc::strong_count(&log),
    );
    let bound = agent
        .bind_workspace(
            AuditedRef::for_test(&identity),
            REPO,
            &workspace,
            layered.layers,
        )
        .unwrap();
    assert_eq!(bound.task_id().0, task_bind_ok.as_str());
    assert_eq!(
        before,
        (
            Arc::strong_count(&gate),
            Arc::strong_count(&escalator),
            Arc::strong_count(&log)
        ),
        "the bound registry holds the very Arcs the layers carried"
    );
    assert!(bound.agent_matches(&agent));
    assert!(bound.capabilities_match(&agent));
    drop(bound);
    assert_eq!(
        (
            Arc::strong_count(&gate),
            Arc::strong_count(&escalator),
            Arc::strong_count(&log)
        ),
        (before.0 - 1, before.1 - 1, before.2 - 1),
        "dropping the bound workspace releases them"
    );
    workspace.cleanup().unwrap();
}

#[test]
fn the_subject_repository_is_owner_and_name_or_the_checkout_path() {
    let with_origin = super::testing::repo_with_origin("https://github.com/example/repo.git");
    assert_eq!(subject_repo(with_origin.path()), "example/repo");
    let bare = tempfile::TempDir::new().unwrap();
    assert_eq!(subject_repo(bare.path()), bare.path().display().to_string());
}

#[test]
fn the_first_unexpected_tool_is_reported_in_name_order() {
    let card = Card::new("\"*\"", "workspace");
    let identity = card.identity();
    let allowed = allowed_for(&identity);
    let grant = scoped(allowed.identity(), &specs_of(&read_only_tools()));
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    for _ in 0..8 {
        let mut tools = read_only_tools();
        for name in ["z1", "z2", "z3", "z4", "z5", "z6", "z7", "z8", "alpha"] {
            tools.push((name, EffectClass::None, ok_with(serde_json::json!(0))));
        }
        let err = registry_matches(&agent, &base_registry(tools).0).unwrap_err();
        assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "alpha"));
    }
}
