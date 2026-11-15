use super::binding::{AuditedRef, BoundWorkspace};
use super::broker::{clip, ArmError, ArmedBroker, Broker, BrokerError};
use super::testing::{
    allowed_for, allowed_for_repo, allowed_for_subtask, allowed_from_dir_for_repo, base_registry,
    calls, fail_with, layers_for, ok_with, real_workspace, scoped, unique_task, Behavior, Counters,
};
use super::tests::{gateway, Card, SOCKET};
use super::*;
use crate::action_auditor::ActionVerdict;
use crate::auditor::{Reason, ReasonCode};
use std::sync::Arc;

const REPO: &str = "github.com/example/repo";

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

struct Parts {
    identity: AgentIdentity,
    agent: ResolvedAgent,
    plan: CheckedPlan,
    counters: Counters,
    sink: Arc<RingAuditSink>,
}

impl Parts {
    fn reported(&self) -> Vec<String> {
        self.plan.exposed_capabilities.clone()
    }
}

fn build_with(
    card: &Card,
    tools: Vec<(&str, EffectClass, Behavior)>,
    policy: BrokerPolicy,
) -> (Broker, Parts) {
    let allowed = allowed_for(&card.identity());
    let grant = scoped(allowed.identity(), &specs_of(&tools));
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    let plan = launch_plan(&PiAdapter, &agent).unwrap();
    let (registry, counters) = base_registry(tools);
    let bound = BoundWorkspace::for_test(
        registry.scoped_for(allowed.identity()),
        allowed.identity(),
        allowed.repo(),
        &agent,
    );
    let sink = Arc::new(RingAuditSink::default());
    let broker = Broker::for_agent(&agent, bound, policy, sink.clone()).unwrap();
    (
        broker,
        Parts {
            identity: allowed.identity().clone(),
            agent,
            plan,
            counters,
            sink,
        },
    )
}

fn armed_with(
    card: &Card,
    tools: Vec<(&str, EffectClass, Behavior)>,
    policy: BrokerPolicy,
) -> (ArmedBroker, Parts) {
    let (broker, parts) = build_with(card, tools, policy);
    let armed = broker.arm(&parts.plan, &parts.reported()).unwrap();
    (armed, parts)
}

fn armed(tools: Vec<(&str, EffectClass, Behavior)>) -> (ArmedBroker, Parts) {
    armed_with(
        &Card::new("\"*\"", "workspace"),
        tools,
        BrokerPolicy::default(),
    )
}

fn req(tool: &str) -> BrokerRequest {
    BrokerRequest {
        tool: tool.to_string(),
        args: serde_json::json!({"k": 1}),
    }
}

fn kind_of(outcome: &BrokerOutcome) -> Option<ToolErrorKind> {
    outcome.to_response().kind
}

fn two_tools() -> Vec<(&'static str, EffectClass, Behavior)> {
    vec![
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(1)),
        ),
        ("search", EffectClass::None, ok_with(serde_json::json!(2))),
    ]
}

#[test]
fn the_documented_limits_are_pinned() {
    assert_eq!(MAX_OUTPUT_BYTES, 1_000_000);
    assert_eq!(MAX_REQUEST_BYTES, 1_048_576);
    assert_eq!(BrokerPolicy::default().max_effect(), EffectClass::Workspace);
}

#[test]
fn pi_plans_name_the_agent_and_model() {
    let (_, parts) = build_with(
        &Card::new("\"*\"", "workspace"),
        two_tools(),
        BrokerPolicy::default(),
    );
    assert_eq!(parts.plan.agent, "a");
    assert_eq!(parts.plan.model, "gemma4:e4b");
}

#[test]
fn arming_requires_the_runtime_to_report_exactly_the_planned_tools() {
    let extra = vec![
        "read_file".to_string(),
        "search".to_string(),
        "bash".to_string(),
    ];
    let (broker, parts) = build_with(
        &Card::new("\"*\"", "workspace"),
        two_tools(),
        BrokerPolicy::default(),
    );
    let err = broker.arm(&parts.plan, &extra).err().unwrap();
    assert!(matches!(err, ArmError::Handshake(_)));
    assert!(err.to_string().contains("bash"));
    let (broker, parts) = build_with(
        &Card::new("\"*\"", "workspace"),
        two_tools(),
        BrokerPolicy::default(),
    );
    let missing = vec!["read_file".to_string()];
    assert!(matches!(
        broker.arm(&parts.plan, &missing),
        Err(ArmError::Handshake(_))
    ));
}

#[test]
fn arming_refuses_a_plan_built_for_another_agent_or_task() {
    let base = Card::new("\"*\"", "workspace");
    let plan_for = |card: &Card,
                    repo: &str,
                    socket: &str,
                    image: &str,
                    tools: Vec<(&str, EffectClass, Behavior)>| {
        let allowed = allowed_for_repo(&card.identity(), repo);
        let grant = scoped(allowed.identity(), &specs_of(&tools));
        let agent =
            ResolvedAgent::resolve(&allowed, &grant, gateway(), image, socket.into()).unwrap();
        launch_plan(&PiAdapter, &agent).unwrap()
    };
    let other_agent = Card {
        name: "other",
        ..Card::new("\"*\"", "workspace")
    };
    let other_limits = Card {
        iterations: "8",
        ..Card::new("\"*\"", "workspace")
    };
    let other_model = Card {
        model: "other-model",
        ..Card::new("\"*\"", "workspace")
    };
    let other_scope = Card {
        paths: "[\"api/**\"]",
        ..Card::new("\"*\"", "workspace")
    };
    let pi = "nanna-agent:pi";
    let cases: Vec<(&str, CheckedPlan)> = vec![
        (
            "agent",
            plan_for(&other_agent, REPO, SOCKET, pi, two_tools()),
        ),
        (
            "audited repository",
            plan_for(&base, "github.com/example/other", SOCKET, pi, two_tools()),
        ),
        (
            "model",
            plan_for(&other_model, REPO, SOCKET, pi, two_tools()),
        ),
        (
            "image",
            plan_for(&base, REPO, SOCKET, "nanna-agent:other", two_tools()),
        ),
        (
            "limits",
            plan_for(&other_limits, REPO, SOCKET, pi, two_tools()),
        ),
        (
            "scope",
            plan_for(&other_scope, REPO, SOCKET, pi, two_tools()),
        ),
        (
            "broker socket",
            plan_for(&base, REPO, "/run/nanna/other.sock", pi, two_tools()),
        ),
        (
            "capabilities",
            plan_for(
                &base,
                REPO,
                SOCKET,
                pi,
                two_tools().into_iter().take(1).collect(),
            ),
        ),
    ];
    for (field, plan) in cases {
        let (broker, parts) = build_with(&base, two_tools(), BrokerPolicy::default());
        let err = broker.arm(&plan, &parts.reported()).err().unwrap();
        let mismatch = matches!(err, ArmError::PlanMismatch { field: f } if f == field);
        assert!(mismatch, "{field}: {err}");
        assert_eq!(
            err.to_string(),
            format!("plan does not belong to this broker: {field} differs")
        );
    }
    let (broker, parts) = build_with(&base, two_tools(), BrokerPolicy::default());
    assert!(broker.arm(&parts.plan, &parts.reported()).is_ok());
}

#[test]
fn a_plan_exposing_capabilities_above_the_policy_cannot_arm() {
    let tools = || {
        vec![
            (
                "read_file",
                EffectClass::None,
                ok_with(serde_json::json!(0)),
            ),
            (
                "git_push",
                EffectClass::Repository,
                ok_with(serde_json::json!(1)),
            ),
            ("trigger_ci", EffectClass::Ci, ok_with(serde_json::json!(2))),
            (
                "deploy_prod",
                EffectClass::Production,
                ok_with(serde_json::json!(3)),
            ),
        ]
    };
    for (ceiling, unserved) in [
        ("repository", vec!["git_push"]),
        ("ci", vec!["git_push", "trigger_ci"]),
        ("production", vec!["deploy_prod", "git_push", "trigger_ci"]),
    ] {
        let (broker, parts) = build_with(
            &Card::new("\"*\"", ceiling),
            tools(),
            BrokerPolicy::default(),
        );
        assert_eq!(broker.served(), vec!["read_file"], "{ceiling}");
        let err = broker.arm(&parts.plan, &parts.reported()).err().unwrap();
        let exposed = matches!(&err, ArmError::ExposedNotServed { names } if *names == unserved);
        assert!(exposed, "{ceiling}: {err}");
        assert_eq!(calls(&parts.counters, "git_push"), 0);
    }
    let (broker, parts) = build_with(
        &Card::new("\"*\"", "ci"),
        tools(),
        BrokerPolicy::new(EffectClass::Ci),
    );
    assert!(BrokerPolicy::new(EffectClass::Ci).admits(EffectClass::Ci));
    assert!(!BrokerPolicy::new(EffectClass::Ci).admits(EffectClass::Sandbox));
    assert_eq!(broker.served(), vec!["git_push", "read_file", "trigger_ci"]);
    assert!(broker.arm(&parts.plan, &parts.reported()).is_ok());
}

#[tokio::test]
async fn granted_calls_run_and_ungranted_calls_never_reach_the_tool() {
    let (broker, parts) = armed(vec![
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!("body")),
        ),
        (
            "write_file",
            EffectClass::Workspace,
            ok_with(serde_json::json!("w")),
        ),
    ]);
    assert_eq!(
        broker.handle(req("read_file")).await,
        BrokerOutcome::Output(serde_json::json!("body"))
    );
    assert_eq!(calls(&parts.counters, "read_file"), 1);
    let unknown = broker.handle(req("bash")).await;
    assert_eq!(kind_of(&unknown), Some(ToolErrorKind::NotGranted));
    for name in [
        "READ_FILE",
        "read_file ",
        "read_file\0",
        "",
        "a/../read_file",
    ] {
        let outcome = broker.handle(req(name)).await;
        assert_eq!(
            kind_of(&outcome),
            Some(ToolErrorKind::NotGranted),
            "{name:?}"
        );
    }
}

#[tokio::test]
async fn the_not_granted_echo_is_truncated() {
    let (broker, _) = armed(two_tools());
    let huge = "x".repeat(5000);
    let reply = broker.handle(req(&huge)).await.to_response();
    let message = reply.output.as_str().unwrap().to_string();
    assert_eq!(reply.kind, Some(ToolErrorKind::NotGranted));
    assert!(message.len() < 200, "{}", message.len());
    assert!(message.contains(&"x".repeat(128)));
    assert!(!message.contains(&"x".repeat(129)));
}

#[tokio::test]
async fn a_narrower_identity_is_not_served_what_only_the_global_one_holds() {
    let tools = vec![
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!("body")),
        ),
        (
            "write_file",
            EffectClass::Workspace,
            ok_with(serde_json::json!("w")),
        ),
    ];
    let (broker, parts) = armed_with(
        &Card::new("\"read_file\"", "workspace"),
        tools,
        BrokerPolicy::default(),
    );
    assert_eq!(parts.plan.exposed_capabilities, vec!["read_file"]);
    let outcome = broker.handle(req("write_file")).await;
    assert_eq!(kind_of(&outcome), Some(ToolErrorKind::NotGranted));
    assert_eq!(calls(&parts.counters, "write_file"), 0);
}

#[tokio::test]
async fn tool_errors_map_to_kinds_and_the_production_hold_halts_the_task() {
    use crate::protected::ProtectedPathViolation;
    use crate::scope::{DenialReason, ScopeDenial};
    use crate::tools::ToolError;
    let tools = vec![
        (
            "t_args",
            EffectClass::None,
            fail_with(|| ToolError::InvalidArguments {
                message: "m".into(),
            }),
        ),
        (
            "t_exec",
            EffectClass::None,
            fail_with(|| ToolError::ExecutionFailed {
                message: "m".into(),
            }),
        ),
        (
            "t_nf",
            EffectClass::None,
            fail_with(|| ToolError::NotFound { name: "n".into() }),
        ),
        (
            "t_ser",
            EffectClass::None,
            fail_with(|| {
                ToolError::Serialization(
                    serde_json::from_str::<serde_json::Value>("{").unwrap_err(),
                )
            }),
        ),
        (
            "t_path",
            EffectClass::None,
            fail_with(|| ToolError::PathSecurityViolation {
                message: "m".into(),
            }),
        ),
        (
            "t_scope",
            EffectClass::None,
            fail_with(|| {
                ToolError::ScopeDenied(ScopeDenial {
                    identity: "a".into(),
                    tool: "t_scope".into(),
                    reason: DenialReason::ToolNotInScope,
                })
            }),
        ),
        (
            "t_prot",
            EffectClass::None,
            fail_with(|| {
                ToolError::ProtectedPath(ProtectedPathViolation {
                    path: ".nanna/x".into(),
                    rule: "r".into(),
                })
            }),
        ),
        (
            "t_io",
            EffectClass::None,
            fail_with(|| ToolError::Io(std::io::Error::other("io"))),
        ),
        (
            "t_budget",
            EffectClass::None,
            fail_with(|| ToolError::Budget(crate::budget::BudgetError::InvalidMinutes(-1.0))),
        ),
        (
            "t_held",
            EffectClass::None,
            fail_with(|| ToolError::ProductionHeld {
                repo: "r".into(),
                escalation_id: "e1".into(),
                summary: "down".into(),
            }),
        ),
        (
            "t_ok",
            EffectClass::None,
            ok_with(serde_json::json!("fine")),
        ),
    ];
    let (broker, parts) = armed(tools);
    let expect = [
        ("t_args", ToolErrorKind::InvalidArguments),
        ("t_exec", ToolErrorKind::ExecutionFailed),
        ("t_nf", ToolErrorKind::NotFound),
        ("t_ser", ToolErrorKind::ExecutionFailed),
        ("t_path", ToolErrorKind::PathSecurity),
        ("t_scope", ToolErrorKind::ScopeDenied),
        ("t_prot", ToolErrorKind::ProtectedPath),
        ("t_io", ToolErrorKind::ExecutionFailed),
        ("t_budget", ToolErrorKind::BudgetRefused),
    ];
    for (tool, kind) in expect {
        let reply = broker.handle(req(tool)).await.to_response();
        assert_eq!(reply.kind, Some(kind), "{tool}");
        assert!(!reply.output.as_str().unwrap().is_empty());
    }
    assert!(!broker.is_halted());
    let held = broker.handle(req("t_held")).await;
    let reason = HaltReason::ProductionHeld {
        repo: "r".into(),
        escalation_id: "e1".into(),
        summary: "down".into(),
    };
    assert_eq!(held, BrokerOutcome::Halt(reason.clone()));
    assert_eq!(reason.error_type(), "StateError");
    assert_eq!(reason.message(), "production hold e1 on r halted the task");
    assert!(broker.is_halted());
    assert_eq!(
        parts.sink.entries().pop().unwrap().outcome,
        AuditOutcome::Halt
    );
    let reply = held.to_response();
    assert!(reply.halt && !reply.ok && reply.kind.is_none());
    let text = reply.output.as_str().unwrap();
    assert_eq!(text, "the task was halted by the host");
    for secret in ["incident", "e1", "down"] {
        assert!(!text.contains(secret), "{secret}");
    }
    let after = broker.handle(req("t_ok")).await;
    assert_eq!(kind_of(&after), Some(ToolErrorKind::TaskHalted));
    assert_eq!(calls(&parts.counters, "t_ok"), 0);
}

#[tokio::test]
async fn output_over_the_cap_is_replaced_by_an_error_measured_in_bytes() {
    let at_cap = "a".repeat(MAX_OUTPUT_BYTES - 2);
    let over_cap = "a".repeat(MAX_OUTPUT_BYTES - 1);
    let multibyte = "é".repeat(MAX_OUTPUT_BYTES / 2);
    assert!(multibyte.chars().count() < MAX_OUTPUT_BYTES);
    let (broker, parts) = armed(vec![
        (
            "at_cap",
            EffectClass::None,
            ok_with(serde_json::json!(at_cap)),
        ),
        (
            "over_cap",
            EffectClass::None,
            ok_with(serde_json::json!(over_cap)),
        ),
        (
            "multibyte",
            EffectClass::None,
            ok_with(serde_json::json!(multibyte)),
        ),
    ]);
    assert!(matches!(
        broker.handle(req("at_cap")).await,
        BrokerOutcome::Output(_)
    ));
    let over = broker.handle(req("over_cap")).await;
    assert_eq!(kind_of(&over), Some(ToolErrorKind::ReplyTooLarge));
    let wide = broker.handle(req("multibyte")).await;
    assert_eq!(kind_of(&wide), Some(ToolErrorKind::ReplyTooLarge));
    assert_eq!(calls(&parts.counters, "over_cap"), 1);
    assert_eq!(
        parts.sink.entries()[1].outcome,
        AuditOutcome::Error(ToolErrorKind::ReplyTooLarge)
    );
}

#[tokio::test]
async fn wire_lines_round_trip_and_bad_or_oversized_requests_are_not_logged() {
    let (broker, parts) = armed(vec![(
        "read_file",
        EffectClass::None,
        ok_with(serde_json::json!({"n": 1})),
    )]);
    let ok: serde_json::Value = serde_json::from_str(
        &broker
            .handle_line(r#"{"tool":"read_file","args":{}}"#)
            .await,
    )
    .unwrap();
    assert_eq!(
        ok,
        serde_json::json!({"ok": true, "output": {"n": 1}, "halt": false})
    );
    let no_args: serde_json::Value =
        serde_json::from_str(&broker.handle_line(r#"{"tool":"read_file"}"#).await).unwrap();
    assert_eq!(no_args["ok"], true);
    for bad in ["not json", r#"{"args":{}}"#, ""] {
        let reply: serde_json::Value =
            serde_json::from_str(&broker.handle_line(bad).await).unwrap();
        assert_eq!(reply["ok"], false, "{bad}");
        assert_eq!(reply["kind"], "bad_request", "{bad}");
        assert_eq!(reply["halt"], false);
        let text = reply["output"].as_str().unwrap();
        assert!(text.starts_with("bad request: "), "{bad}");
    }
    let prefix = r#"{"tool":"read_file","args":""#;
    let suffix = r#""}"#;
    let pad = |total: usize| "a".repeat(total - prefix.len() - suffix.len());
    let at_cap = format!("{prefix}{}{suffix}", pad(MAX_REQUEST_BYTES));
    assert_eq!(at_cap.len(), MAX_REQUEST_BYTES);
    let accepted: serde_json::Value =
        serde_json::from_str(&broker.handle_line(&at_cap).await).unwrap();
    assert_eq!(accepted["ok"], true);
    let over = format!("{prefix}{}{suffix}", pad(MAX_REQUEST_BYTES + 1));
    let rejected: serde_json::Value =
        serde_json::from_str(&broker.handle_line(&over).await).unwrap();
    assert_eq!(rejected["kind"], "bad_request");
    assert_eq!(rejected["output"], "request too large");
    assert_eq!(parts.sink.entries().len(), 3);
    let refused: serde_json::Value =
        serde_json::from_str(&broker.handle_line(r#"{"tool":"bash"}"#).await).unwrap();
    assert_eq!(refused["kind"], "not_granted");
}

#[tokio::test]
async fn audit_records_hold_sizes_and_fingerprints_not_content_and_are_bounded() {
    let (broker, parts) = armed(vec![(
        "read_file",
        EffectClass::None,
        ok_with(serde_json::json!("body")),
    )]);
    let secret = "hunter2-SECRET-TOKEN";
    let args = serde_json::json!({"path": secret});
    let call = |tool: &str, args: &serde_json::Value| BrokerRequest {
        tool: tool.to_string(),
        args: args.clone(),
    };
    broker.handle(call("read_file", &args)).await;
    broker.handle(call("evil\nname\u{1b}[31m", &args)).await;
    let long = "x".repeat(500);
    broker.handle(call(&long, &args)).await;
    let records = parts.sink.entries();
    assert_eq!(records.len(), 3);
    let text = format!("{records:?}");
    assert!(!text.contains(secret));
    let text_args = args.to_string();
    assert_eq!(records[0].args_bytes, text_args.len());
    assert_eq!(
        records[0].args_fingerprint,
        crate::auditor::content_hash(&text_args)
    );
    assert_eq!(records[0].effect, Some(EffectClass::None));
    assert_eq!(records[0].outcome, AuditOutcome::Ok);
    assert_eq!(records[1].tool, "evil\\nname\\u{1b}[31m");
    assert_eq!(records[1].effect, None);
    assert_eq!(records[2].tool.len(), 128);
    for (i, record) in records.iter().enumerate() {
        assert_eq!(record.started, i as u64);
        assert_eq!(record.completed, i as u64);
    }
}

#[test]
fn the_ring_sink_drops_the_oldest_and_counts_drops() {
    let record = |n: u64| CallRecord {
        started: n,
        completed: n,
        tool: format!("t{n}"),
        args_bytes: 0,
        args_fingerprint: String::new(),
        effect: None,
        outcome: AuditOutcome::Ok,
    };
    let sink = RingAuditSink::new(2);
    for n in 0..5 {
        sink.record(record(n));
    }
    let kept: Vec<u64> = sink.entries().iter().map(|r| r.started).collect();
    assert_eq!(kept, vec![3, 4]);
    assert_eq!(sink.dropped(), 3);
    let tiny = RingAuditSink::new(0);
    tiny.record(record(1));
    assert_eq!(tiny.dropped(), 0);
    tiny.record(record(2));
    assert_eq!(tiny.entries().len(), 1);
    assert_eq!(tiny.dropped(), 1);
    let default = RingAuditSink::default();
    for n in 0..10_001 {
        default.record(record(n));
    }
    assert_eq!(default.entries().len(), 10_000);
    assert_eq!(default.dropped(), 1);
}

fn bound_for(
    identity: &AgentIdentity,
    repo: &str,
    agent: &ResolvedAgent,
    tools: Vec<(&str, EffectClass, Behavior)>,
) -> BoundWorkspace {
    let (registry, _) = base_registry(tools);
    BoundWorkspace::for_test(registry.scoped_for(identity), identity, repo, agent)
}

fn sink() -> Arc<RingAuditSink> {
    Arc::new(RingAuditSink::default())
}

#[test]
fn building_refuses_a_workspace_bound_for_a_different_identity_or_repository() {
    let (_, parts) = build_with(
        &Card::new("\"*\"", "workspace"),
        two_tools(),
        BrokerPolicy::default(),
    );
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
            "scope",
            Card {
                paths: "[\"api/**\"]",
                ..Card::new("\"*\"", "workspace")
            },
            REPO,
        ),
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
        ("ceiling", Card::new("\"*\"", "none"), REPO),
        (
            "repo",
            Card::new("\"*\"", "workspace"),
            "github.com/example/other",
        ),
    ];
    for (label, card, repo) in attempts {
        let identity = card.identity();
        let bound = bound_for(&identity, repo, &parts.agent, two_tools());
        let err = Broker::for_agent(&parts.agent, bound, BrokerPolicy::default(), sink())
            .err()
            .unwrap_or_else(|| panic!("{label} accepted"));
        let mismatch = matches!(err, BrokerError::IdentityMismatch { .. });
        assert!(mismatch, "{label}");
        assert_eq!(
            err.to_string(),
            "the bound workspace's identity does not match the resolved agent `a`"
        );
    }
    let same = bound_for(&parts.identity, REPO, &parts.agent, two_tools());
    assert!(Broker::for_agent(&parts.agent, same, BrokerPolicy::default(), sink()).is_ok());
}

#[test]
fn an_agent_holding_a_capability_its_identity_does_not_allow_is_refused() {
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
    for card in [
        Card::new("\"read_file\"", "workspace"),
        Card::new("\"*\"", "none"),
    ] {
        let (_, parts) = build_with(
            &card,
            tools().into_iter().take(1).collect(),
            BrokerPolicy::default(),
        );
        let mut forged = parts.agent.clone();
        forged.capabilities.push(CapabilitySpec::new(
            "write_file",
            "d",
            serde_json::json!({}),
            EffectClass::Workspace,
        ));
        let bound = bound_for(&parts.identity, REPO, &forged, tools());
        let err = Broker::for_agent(&forged, bound, BrokerPolicy::default(), sink())
            .err()
            .unwrap();
        assert!(matches!(err, BrokerError::IdentityMismatch { .. }), "{err}");
    }
}

#[test]
fn a_workspace_bound_for_the_same_identity_but_another_repository_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let card = Card::new("\"read_file\"", "workspace");
    std::fs::write(
        dir.path().join("a.toml"),
        card.identity().to_toml_string().unwrap(),
    )
    .unwrap();
    let tools = || {
        vec![(
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(0)),
        )]
    };
    let resolve_for = |repo: &str| {
        let allowed = allowed_from_dir_for_repo(dir.path(), "a", repo);
        let grant = scoped(allowed.identity(), &specs_of(&tools()));
        let agent =
            ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
                .unwrap();
        (allowed, agent)
    };
    let (allowed_x, agent_x) = resolve_for("github.com/example/x");
    let (allowed_y, agent_y) = resolve_for("github.com/example/y");
    assert_eq!(allowed_x.identity(), allowed_y.identity());
    let bound_x = bound_for(allowed_x.identity(), allowed_x.repo(), &agent_x, tools());
    let err = Broker::for_agent(&agent_y, bound_x, BrokerPolicy::default(), sink())
        .err()
        .unwrap();
    assert!(matches!(err, BrokerError::IdentityMismatch { .. }), "{err}");
    let bound_y = bound_for(allowed_y.identity(), allowed_y.repo(), &agent_y, tools());
    assert!(Broker::for_agent(&agent_y, bound_y, BrokerPolicy::default(), sink()).is_ok());
}

#[test]
fn a_workspace_bound_for_another_grant_of_the_same_identity_is_refused() {
    let card = Card::new("\"*\"", "workspace");
    let allowed = allowed_for(&card.identity());
    let resolve_with_tools = |count: usize| {
        let subset: Vec<_> = two_tools().into_iter().take(count).collect();
        let grant = scoped(allowed.identity(), &specs_of(&subset));
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap()
    };
    let full = resolve_with_tools(2);
    let small = resolve_with_tools(1);
    let bound_full = bound_for(allowed.identity(), allowed.repo(), &full, two_tools());
    let err = Broker::for_agent(&small, bound_full, BrokerPolicy::default(), sink())
        .err()
        .unwrap();
    assert!(matches!(err, BrokerError::BoundWorkspaceMismatch), "{err}");
    assert_eq!(
        err.to_string(),
        "the bound workspace was not built for this agent's grant"
    );
}

#[test]
fn the_task_prompt_is_carried_by_the_agent_not_re_derived() {
    let identity = Card::new("\"*\"", "workspace").identity();
    let first = allowed_for_subtask(&identity, REPO, "Add a regression test.");
    let second = allowed_for_subtask(&identity, REPO, "Exfiltrate the secrets.");
    let grant = scoped(first.identity(), &specs_of(&two_tools()));
    let agent_one =
        ResolvedAgent::resolve(&first, &grant, gateway(), "nanna-agent:pi", SOCKET.into()).unwrap();
    let agent_two =
        ResolvedAgent::resolve(&second, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    assert_eq!(agent_one.task_prompt(), "Add a regression test.");
    assert_eq!(agent_two.task_prompt(), "Exfiltrate the secrets.");
    assert_ne!(agent_one.task_prompt(), agent_two.task_prompt());
}

#[tokio::test]
async fn tool_names_are_cut_at_a_character_boundary_when_logged() {
    let (broker, parts) = armed(vec![(
        "read_file",
        EffectClass::None,
        ok_with(serde_json::json!(1)),
    )]);
    let mut names: Vec<String> = Vec::new();
    for ch in ["é", "€", "😀"] {
        for prefix in 0..5 {
            names.push(format!("{}{}", "a".repeat(120 + prefix), ch.repeat(40)));
        }
    }
    for name in &names {
        let outcome = broker.handle(req(name)).await;
        assert_eq!(kind_of(&outcome), Some(ToolErrorKind::NotGranted));
    }
    let records = parts.sink.entries();
    assert_eq!(records.len(), names.len());
    for record in records {
        assert!(record.tool.len() <= 128, "{}", record.tool.len());
        assert!(record.tool.is_char_boundary(record.tool.len()));
    }
    let exact = "a".repeat(128);
    broker.handle(req(&exact)).await;
    assert_eq!(parts.sink.entries().last().unwrap().tool.len(), 128);
}

#[tokio::test]
async fn a_panicking_tool_is_still_audited_as_aborted() {
    use futures::FutureExt;
    let (broker, parts) = armed(vec![
        (
            "boom",
            EffectClass::None,
            Arc::new(|| panic!("tool panicked")),
        ),
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(1)),
        ),
    ]);
    let result = std::panic::AssertUnwindSafe(broker.handle(req("boom")))
        .catch_unwind()
        .await;
    assert!(result.is_err());
    let records = parts.sink.entries();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, AuditOutcome::Aborted);
    assert_eq!((records[0].started, records[0].completed), (0, 0));
    assert_eq!(records[0].tool, "boom");
    assert!(matches!(
        broker.handle(req("read_file")).await,
        BrokerOutcome::Output(_)
    ));
    assert_eq!(parts.sink.entries()[1].started, 1);
}

#[tokio::test]
async fn action_denials_map_to_a_typed_error_or_a_halt_and_the_audit_is_surfaced() {
    use crate::action_auditor::ActionDenied;
    use crate::tools::ToolError;
    let reason = || Reason::new(ReasonCode::Other, "needs a human");
    let tools = vec![
        (
            "t_block",
            EffectClass::None,
            fail_with(|| {
                ToolError::ActionDenied(ActionDenied::Block {
                    reasons: vec![Reason::new(ReasonCode::Other, "needs a human")],
                })
            }),
        ),
        (
            "t_escalate",
            EffectClass::None,
            fail_with(|| {
                ToolError::ActionDenied(ActionDenied::Escalate {
                    reasons: vec![Reason::new(ReasonCode::Other, "needs a human")],
                })
            }),
        ),
        (
            "t_ok",
            EffectClass::None,
            ok_with(serde_json::json!("fine")),
        ),
    ];
    let (broker, parts) = armed(tools);
    let blocked = broker.handle(req("t_block")).await;
    assert_eq!(kind_of(&blocked), Some(ToolErrorKind::ActionBlocked));
    assert!(!broker.is_halted());
    assert_eq!(
        serde_json::to_value(ToolErrorKind::ActionBlocked).unwrap(),
        "action_blocked"
    );
    let escalated = broker.handle(req("t_escalate")).await;
    let halt = HaltReason::ActionEscalated {
        tool: "t_escalate".into(),
        reasons: vec![reason()],
    };
    assert_eq!(escalated, BrokerOutcome::Halt(halt.clone()));
    assert!(escalated.is_halt());
    assert_eq!(halt.error_type(), "StateError");
    assert_eq!(
        halt.message(),
        "action review escalated `t_escalate`; see action_audit in the task result"
    );
    assert!(broker.is_halted());
    let reply = escalated.to_response();
    assert!(reply.halt && !reply.ok);
    assert_eq!(reply.output, "the task was halted by the host");
    let after = broker.handle(req("t_ok")).await;
    assert_eq!(kind_of(&after), Some(ToolErrorKind::TaskHalted));
    assert_eq!(calls(&parts.counters, "t_ok"), 0);
    let records = parts.sink.entries();
    assert_eq!(
        records[0].outcome,
        AuditOutcome::Error(ToolErrorKind::ActionBlocked)
    );
    assert_eq!(records[1].outcome, AuditOutcome::Halt);
    assert!(broker.action_audit().is_empty());
    assert_eq!(broker.task_id().0, "t-test");
}

struct Real {
    workspace: crate::workspace::TaskWorkspace,
    _repo: tempfile::TempDir,
    identity: AgentIdentity,
    agent: ResolvedAgent,
    plan: CheckedPlan,
}

fn real_card(paths: &str, tools: &str, effect: &str) -> Card<'static> {
    let paths: &'static str = Box::leak(paths.to_string().into_boxed_str());
    let tools: &'static str = Box::leak(tools.to_string().into_boxed_str());
    let effect: &'static str = Box::leak(effect.to_string().into_boxed_str());
    Card {
        paths,
        repos: "[\"github.com/example/repo\"]",
        ..Card::new(tools, effect)
    }
}

fn real(task: &str, card: &Card) -> Real {
    let (workspace, repo) = real_workspace(task);
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
    let plan = launch_plan(&PiAdapter, &agent).unwrap();
    Real {
        workspace,
        _repo: repo,
        identity: allowed.identity().clone(),
        agent,
        plan,
    }
}

fn armed_real(
    r: &Real,
    task: &str,
    verdict: ActionVerdict,
    policy: BrokerPolicy,
) -> (ArmedBroker, Arc<crate::escalation::EscalationLog>) {
    let layered = layers_for(&r.identity, task, "example/repo", verdict);
    let bound = r
        .agent
        .bind_workspace(
            AuditedRef::for_test(&r.identity),
            REPO,
            &r.workspace,
            layered.layers,
        )
        .unwrap();
    assert_eq!(bound.task_id().0, task);
    let broker = Broker::for_agent(&r.agent, bound, policy, sink()).unwrap();
    assert_eq!(broker.task_id().0, task);
    let armed = broker
        .arm(&r.plan, &r.plan.exposed_capabilities.clone())
        .unwrap();
    (armed, layered.log)
}

#[tokio::test]
async fn bound_to_a_real_workspace_an_out_of_path_write_is_refused_end_to_end() {
    let task_e2e = unique_task("t-e2e");
    let _path = crate::test_support::hold_path_async().await;
    let mut r = real(
        task_e2e.as_str(),
        &real_card("[\"api/**\"]", "\"write_file\"", "workspace"),
    );
    assert_eq!(r.agent.capability_names(), vec!["write_file"]);
    let (armed, _) = armed_real(
        &r,
        task_e2e.as_str(),
        ActionVerdict::Allow,
        BrokerPolicy::default(),
    );
    assert_eq!(armed.task_id().0, task_e2e.as_str());
    let denied = armed
        .handle(BrokerRequest {
            tool: "write_file".into(),
            args: serde_json::json!({"path": "src/x.rs", "content": "x"}),
        })
        .await;
    assert_eq!(kind_of(&denied), Some(ToolErrorKind::ScopeDenied));
    assert!(!r.workspace.workspace_path.join("src/x.rs").exists());
    let written = armed
        .handle(BrokerRequest {
            tool: "write_file".into(),
            args: serde_json::json!({"path": "api/x.rs", "content": "x"}),
        })
        .await;
    assert!(matches!(written, BrokerOutcome::Output(_)));
    assert!(r.workspace.workspace_path.join("api/x.rs").exists());
    r.workspace.cleanup().unwrap();
}

#[tokio::test]
async fn the_bound_registry_installs_every_model_facing_layer() {
    let task_layers_b = unique_task("t-layers-b");
    let _path = crate::test_support::hold_path_async().await;
    let mut r = real(
        task_layers_b.as_str(),
        &real_card("[\"api/**\"]", "\"write_file\"", "workspace"),
    );
    let layered = layers_for(
        &r.identity,
        task_layers_b.as_str(),
        "example/repo",
        ActionVerdict::Allow,
    );
    let bound = r
        .agent
        .bind_workspace(
            AuditedRef::for_test(&r.identity),
            REPO,
            &r.workspace,
            layered.layers,
        )
        .unwrap();
    let broker = Broker::for_agent(&r.agent, bound, BrokerPolicy::default(), sink()).unwrap();
    assert_eq!(
        broker.installed_layers(),
        crate::tools::InstalledLayers::ALL
    );
    let armed = broker
        .arm(&r.plan, &r.plan.exposed_capabilities.clone())
        .unwrap();
    assert_eq!(armed.installed_layers(), crate::tools::InstalledLayers::ALL);
    r.workspace.cleanup().unwrap();
}

#[tokio::test]
async fn a_real_gate_blocks_and_a_real_escalation_halts_and_is_audited() {
    let task_escalate_b = unique_task("t-escalate-b");
    let task_block_b = unique_task("t-block-b");
    let _path = crate::test_support::hold_path_async().await;
    let card = real_card("[\"**\"]", "\"github_pr_status\"", "repository");
    let widened = BrokerPolicy::new(EffectClass::Repository);
    let status = || BrokerRequest {
        tool: "github_pr_status".into(),
        args: serde_json::json!({}),
    };

    let mut g = real(task_block_b.as_str(), &card);
    let (armed, _) = armed_real(
        &g,
        task_block_b.as_str(),
        ActionVerdict::block(vec![Reason::new(ReasonCode::Other, "no")]),
        widened,
    );
    let blocked = armed.handle(status()).await;
    assert_eq!(kind_of(&blocked), Some(ToolErrorKind::ActionBlocked));
    assert!(!armed.is_halted());
    assert_eq!(armed.action_audit().len(), 1);
    g.workspace.cleanup().unwrap();

    let mut g = real(task_escalate_b.as_str(), &card);
    let (armed, log) = armed_real(
        &g,
        task_escalate_b.as_str(),
        ActionVerdict::escalate(vec![Reason::new(ReasonCode::Other, "needs a human")]),
        widened,
    );
    let halted = armed.handle(status()).await;
    assert!(matches!(
        halted,
        BrokerOutcome::Halt(HaltReason::ActionEscalated { ref tool, .. }) if tool == "github_pr_status"
    ));
    assert!(armed.is_halted());
    assert!(log.snapshot(chrono::Utc::now()).occurrences > 0);
    assert!(!armed.action_audit().is_empty());
    if let BrokerOutcome::Halt(reason) = halted {
        assert_eq!(reason.error_type(), "StateError");
        assert!(reason.message().contains("see action_audit"));
    }
    g.workspace.cleanup().unwrap();
}

#[tokio::test]
async fn a_workspace_call_below_repository_never_reaches_the_action_gate() {
    let task_below_gate = unique_task("t-below-gate");
    let _path = crate::test_support::hold_path_async().await;
    let mut r = real(
        task_below_gate.as_str(),
        &real_card("[\"api/**\"]", "\"write_file\"", "workspace"),
    );
    let (armed, _) = armed_real(
        &r,
        task_below_gate.as_str(),
        ActionVerdict::block(vec![Reason::new(ReasonCode::Other, "no")]),
        BrokerPolicy::default(),
    );
    let written = armed
        .handle(BrokerRequest {
            tool: "write_file".into(),
            args: serde_json::json!({"path": "api/w.rs", "content": "x"}),
        })
        .await;
    assert!(matches!(written, BrokerOutcome::Output(_)));
    assert!(armed.action_audit().is_empty());
    assert!(r.workspace.workspace_path.join("api/w.rs").exists());
    r.workspace.cleanup().unwrap();
}

#[tokio::test]
async fn a_workspace_bound_for_another_audited_spawn_is_refused() {
    let task_cross_b = unique_task("t-cross-b");
    let task_cross_a = unique_task("t-cross-a");
    let _path = crate::test_support::hold_path_async().await;
    let mut a = real(
        task_cross_a.as_str(),
        &real_card("[\"**\"]", "\"write_file\"", "workspace"),
    );
    let mut b = real(
        task_cross_b.as_str(),
        &real_card("[\"api/**\"]", "\"write_file\"", "workspace"),
    );
    let layered = layers_for(
        &a.identity,
        task_cross_a.as_str(),
        "example/repo",
        ActionVerdict::Allow,
    );
    let bound_a = a
        .agent
        .bind_workspace(
            AuditedRef::for_test(&a.identity),
            REPO,
            &a.workspace,
            layered.layers,
        )
        .unwrap();
    let err = Broker::for_agent(&b.agent, bound_a, BrokerPolicy::default(), sink())
        .err()
        .unwrap();
    assert!(matches!(err, BrokerError::IdentityMismatch { .. }), "{err}");
    assert!(!b.workspace.workspace_path.join("src/x.rs").exists());
    let layered = layers_for(
        &b.identity,
        task_cross_b.as_str(),
        "example/repo",
        ActionVerdict::Allow,
    );
    let bound_b = b
        .agent
        .bind_workspace(
            AuditedRef::for_test(&b.identity),
            REPO,
            &b.workspace,
            layered.layers,
        )
        .unwrap();
    assert!(Broker::for_agent(&b.agent, bound_b, BrokerPolicy::default(), sink()).is_ok());
    a.workspace.cleanup().unwrap();
    b.workspace.cleanup().unwrap();
}

#[tokio::test]
async fn every_error_message_on_the_wire_is_clipped_on_a_character_boundary() {
    use crate::tools::ToolError;
    let (broker, _) = armed(vec![
        (
            "wide",
            EffectClass::None,
            fail_with(|| ToolError::ExecutionFailed {
                message: "é".repeat(3_000_000),
            }),
        ),
        (
            "quotes",
            EffectClass::None,
            fail_with(|| ToolError::ExecutionFailed {
                message: "\"".repeat(3_000_000),
            }),
        ),
        (
            "short",
            EffectClass::None,
            fail_with(|| ToolError::ExecutionFailed {
                message: "tiny".into(),
            }),
        ),
    ]);
    for tool in ["wide", "quotes"] {
        let line = broker.handle_line(&format!(r#"{{"tool":"{tool}"}}"#)).await;
        assert!(line.len() <= MAX_OUTPUT_BYTES, "{tool}: {}", line.len());
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["kind"], "execution_failed");
        assert!(reply["output"].as_str().unwrap().len() <= MAX_MESSAGE_BYTES);
    }
    let short = broker.handle_line(r#"{"tool":"short"}"#).await;
    assert!(short.contains("Execution failed: tiny"));
    let bad = broker.handle_line(&"x".repeat(MAX_REQUEST_BYTES + 1)).await;
    assert!(bad.len() < 100, "{}", bad.len());
}

#[test]
fn the_message_cap_is_one_sixth_of_the_output_cap() {
    assert_eq!(MAX_MESSAGE_BYTES, 166_666);
    assert_eq!(MAX_MESSAGE_BYTES, MAX_OUTPUT_BYTES / 6);
}

#[tokio::test]
async fn calls_are_serialised_so_a_call_queued_behind_an_escalation_is_refused() {
    use crate::action_auditor::ActionDenied;
    use crate::tools::ToolError;
    let (broker, parts) = armed(vec![
        (
            "t_escalate",
            EffectClass::None,
            fail_with(|| {
                ToolError::ActionDenied(ActionDenied::Escalate {
                    reasons: vec![Reason::new(ReasonCode::Other, "needs a human")],
                })
            }),
        ),
        (
            "t_ok",
            EffectClass::None,
            ok_with(serde_json::json!("fine")),
        ),
    ]);
    let (first, second, third) = tokio::join!(
        broker.handle(req("t_escalate")),
        broker.handle(req("t_ok")),
        broker.handle(req("t_ok"))
    );
    assert!(first.is_halt());
    assert_eq!(kind_of(&second), Some(ToolErrorKind::TaskHalted));
    assert_eq!(kind_of(&third), Some(ToolErrorKind::TaskHalted));
    assert_eq!(calls(&parts.counters, "t_ok"), 0);
}

#[tokio::test]
async fn reply_lines_never_contain_the_unicode_line_separators_or_a_newline() {
    let (broker, _) = armed(vec![(
        "echo",
        EffectClass::None,
        ok_with(serde_json::json!("a\u{2028}b\u{2029}c\nd")),
    )]);
    let line = broker
        .handle_line("{\"tool\":\"echo\",\"args\":{\"k\":\"x\u{2028}y\u{2029}z\"}}")
        .await;
    assert!(!line.contains('\u{2028}'));
    assert!(!line.contains('\u{2029}'));
    assert!(!line.contains('\n'));
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["output"], "a\u{2028}b\u{2029}c\nd");
}

#[test]
fn clipping_never_splits_a_character() {
    assert_eq!(clip("éééé", 3), "é");
    assert_eq!(clip("éééé", 4), "éé");
    assert_eq!(clip("😀😀", 5), "😀");
    assert_eq!(clip("abc", 10), "abc");
    assert_eq!(clip("abc", 0), "");
}

#[tokio::test]
async fn the_output_cap_is_measured_on_the_escaped_reply_line() {
    let separators = ["\u{2028}", "\u{2029}"];
    for separator in separators {
        let over = separator.repeat(333_332);
        let exact = separator.repeat(166_666);
        let (broker, _) = armed(vec![
            ("over", EffectClass::None, ok_with(serde_json::json!(over))),
            (
                "exact",
                EffectClass::None,
                ok_with(serde_json::json!(exact)),
            ),
        ]);
        let refused = broker.handle_line(r#"{"tool":"over"}"#).await;
        assert!(refused.len() <= 1_048_576, "{}", refused.len());
        assert!(refused.contains("reply_too_large"));
        let accepted = broker.handle_line(r#"{"tool":"exact"}"#).await;
        assert!(accepted.len() <= 1_048_576, "{}", accepted.len());
        assert!(accepted.contains("\"ok\":true"));
    }
    let controls = "\u{1}".repeat(MAX_MESSAGE_BYTES);
    let (broker, _) = armed(vec![(
        "ctl",
        EffectClass::None,
        fail_with(|| crate::tools::ToolError::ExecutionFailed {
            message: "\u{1}".repeat(3_000_000),
        }),
    )]);
    let line = broker.handle_line(r#"{"tool":"ctl"}"#).await;
    assert!(line.len() <= 1_048_576, "{}", line.len());
    assert_eq!(controls.len(), MAX_MESSAGE_BYTES);
}

#[test]
fn a_workspace_bound_for_the_same_names_with_other_effect_classes_is_refused() {
    let card = Card::new("\"*\"", "workspace");
    let allowed = allowed_for(&card.identity());
    let resolve_with_effect = |effect: EffectClass| {
        let tools = vec![("read_file", effect, ok_with(serde_json::json!(0)))];
        let grant = scoped(allowed.identity(), &specs_of(&tools));
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap()
    };
    let quiet = resolve_with_effect(EffectClass::None);
    let loud = resolve_with_effect(EffectClass::Workspace);
    let tools = || {
        vec![(
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(0)),
        )]
    };
    let bound_quiet = bound_for(allowed.identity(), allowed.repo(), &quiet, tools());
    let err = Broker::for_agent(&loud, bound_quiet, BrokerPolicy::default(), sink())
        .err()
        .unwrap();
    assert!(matches!(err, BrokerError::BoundWorkspaceMismatch), "{err}");
}

#[tokio::test]
async fn a_bad_request_that_echoes_its_input_in_the_parse_error_is_clipped() {
    let (broker, _) = armed(two_tools());
    let line = format!("\"{}\"", "a".repeat(1_000_000));
    let reply: serde_json::Value = serde_json::from_str(&broker.handle_line(&line).await).unwrap();
    assert_eq!(reply["kind"], "bad_request");
    let text = reply["output"].as_str().unwrap();
    assert!(text.starts_with("bad request: "));
    assert!(text.len() <= MAX_MESSAGE_BYTES, "{}", text.len());
}
