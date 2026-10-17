use super::testing::{
    allowed_for, allowed_for_repo, allowed_from_dir, base_registry, calls, fail_with, ok_with,
    scoped, Behavior, Counters,
};
use super::*;
use crate::identity::AgentIdentity;
use proptest::prelude::*;
use std::path::Path;
use std::sync::Arc;

const SOCKET: &str = "/run/nanna/broker.sock";

struct Card<'a> {
    name: &'a str,
    model: &'a str,
    paths: &'a str,
    read_paths: &'a str,
    effect: &'a str,
    tools: &'a str,
    iterations: &'a str,
    repos: &'a str,
}

impl<'a> Card<'a> {
    fn new(tools: &'a str, effect: &'a str) -> Self {
        Self {
            name: "a",
            model: "gemma4:e4b",
            paths: "[\"**\"]",
            read_paths: "",
            effect,
            tools,
            iterations: "7",
            repos: "[\"r\"]",
        }
    }

    fn identity(&self) -> AgentIdentity {
        let toml = format!(
            r#"
[identity]
name = "{}"
description = "d"
loop = "inner"
model = "{}"
system_prompt = {{ inline = "Be careful." }}
[scope]
repos = {}
paths = {}
{}
max_effect = "{}"
tools = [{}]
[limits]
max_iterations = {}
max_wall_clock_secs = 9
max_concurrent = 2
"#,
            self.name,
            self.model,
            self.repos,
            self.paths,
            self.read_paths,
            self.effect,
            self.tools,
            self.iterations
        );
        AgentIdentity::from_toml_str(&toml, "x.toml").unwrap()
    }
}

fn catalog() -> Vec<CapabilitySpec> {
    [
        ("read_file", EffectClass::None),
        ("write_file", EffectClass::Workspace),
        ("cargo_check", EffectClass::Workspace),
        ("git_push", EffectClass::Repository),
        ("deploy", EffectClass::Production),
    ]
    .into_iter()
    .map(|(n, e)| {
        CapabilitySpec::new(
            n,
            format!("{n} tool"),
            serde_json::json!({"type": "object"}),
            e,
        )
    })
    .collect()
}

fn gateway() -> Endpoint {
    Endpoint::new("model-gateway", 11434).unwrap()
}

fn resolve_with(
    card: &Card,
    available: &[CapabilitySpec],
    socket: &str,
) -> Result<ResolvedAgent, ResolveError> {
    let identity = card.identity();
    let allowed = allowed_for(&identity);
    let scoped = scoped(allowed.identity(), available);
    ResolvedAgent::resolve(
        &allowed,
        &scoped,
        gateway(),
        "nanna-agent:pi",
        socket.into(),
    )
}

fn agent(card: &Card, available: &[CapabilitySpec]) -> ResolvedAgent {
    resolve_with(card, available, SOCKET).unwrap()
}

fn wide_agent() -> ResolvedAgent {
    agent(&Card::new("\"*\"", "workspace"), &catalog())
}

#[test]
fn endpoint_accepts_plain_hosts() {
    for host in [
        "model-gateway",
        "10.0.0.5",
        "a-b.c-1",
        "gw.local",
        "1gw",
        "host.example",
    ] {
        assert!(Endpoint::new(host, 80).is_ok(), "{host}");
    }
    let endpoint = Endpoint::new("gw.local", 8080).unwrap();
    assert_eq!((endpoint.host(), endpoint.port()), ("gw.local", 8080));
}

#[test]
fn endpoint_rejects_hostile_or_ambient_hosts() {
    let hosts = [
        "",
        "a b",
        "evil/../host",
        "gw:80/@evil.com#",
        "localhost",
        "LOCALHOST",
        "127.0.0.1",
        "127.1",
        "2130706433",
        "0x7f.1",
        "0x7f000001",
        "169.254.169.254",
        "0.0.0.0",
        "224.0.0.1",
        ".a",
        "a.",
        "-a",
        "a-",
        "a..b",
        "1.2.3",
        "1.2.3.4.5",
    ];
    for host in hosts {
        assert!(Endpoint::new(host, 80).is_err(), "{host:?}");
    }
    assert!(Endpoint::new("gw", 0).is_err());
    let err = Endpoint::new("a b", 1).unwrap_err();
    assert!(err.to_string().contains("a b:1"));
}

#[test]
fn endpoint_serializes_without_a_deserializer() {
    assert_eq!(
        serde_json::to_string(&gateway()).unwrap(),
        r#"{"host":"model-gateway","port":11434}"#
    );
}

#[test]
fn resolve_carries_scope_limits_and_ceiling() {
    let card = Card {
        paths: "[\"api/**\"]",
        read_paths: "read_paths = [\"**\"]",
        ..Card::new("\"*\"", "ci")
    };
    let a = agent(&card, &[]);
    assert_eq!(a.name(), "a");
    assert_eq!(a.system_prompt(), "Be careful.");
    assert_eq!(a.task_prompt(), "Add a regression test.");
    assert_eq!(a.audited_repo(), "github.com/example/repo");
    assert_eq!(a.model(), "gemma4:e4b");
    assert_eq!(a.endpoint(), &gateway());
    assert_eq!(a.image(), "nanna-agent:pi");
    assert_eq!(a.broker_socket(), Path::new(SOCKET));
    assert_eq!(a.scope().repos, vec!["r"]);
    assert_eq!(a.scope().paths, vec!["api/**"]);
    assert_eq!(a.scope().read_paths, Some(vec!["**".to_string()]));
    assert_eq!(a.limits().max_iterations, 7);
    assert_eq!(a.limits().max_wall_clock_secs, 9);
    assert_eq!(a.max_effect(), EffectClass::Ci);
}

#[test]
fn resolve_grants_exactly_the_scoped_set_sorted() {
    let available = vec![
        CapabilitySpec::new("b_tool", "d", serde_json::json!({}), EffectClass::None),
        CapabilitySpec::new("c_tool", "d", serde_json::json!({}), EffectClass::None),
        CapabilitySpec::new("a_tool", "d", serde_json::json!({}), EffectClass::None),
    ];
    let a = agent(&Card::new("\"a_tool\", \"b_tool\"", "none"), &available);
    assert_eq!(a.capability_names(), vec!["a_tool", "b_tool"]);
    assert_eq!(a.capabilities().len(), 2);
}

#[test]
fn ceilings_grant_effectful_capabilities_up_to_the_ceiling() {
    let available: Vec<CapabilitySpec> = [
        ("read_file", EffectClass::None),
        ("git_push", EffectClass::Repository),
        ("trigger_ci", EffectClass::Ci),
        ("deploy_sandbox", EffectClass::Sandbox),
        ("deploy_prod", EffectClass::Production),
    ]
    .into_iter()
    .map(|(n, e)| CapabilitySpec::new(n, "d", serde_json::json!({}), e))
    .collect();
    for (ceiling, expected) in [
        ("none", 1),
        ("repository", 2),
        ("ci", 3),
        ("sandbox", 4),
        ("production", 5),
    ] {
        let a = agent(&Card::new("\"*\"", ceiling), &available);
        assert_eq!(a.capabilities().len(), expected, "{ceiling}");
        assert!(a.capabilities().iter().all(|c| c.effect <= a.max_effect()));
    }
}

#[test]
fn scope_mismatch_requires_both_name_and_scope() {
    let base = Card::new("\"*\"", "workspace");
    let allowed = allowed_for(&base.identity());
    let attempts: [(&str, Card); 3] = [
        (
            "different name, same scope",
            Card {
                name: "other",
                ..Card::new("\"*\"", "workspace")
            },
        ),
        (
            "same name, narrower paths",
            Card {
                paths: "[\"api/**\"]",
                ..Card::new("\"*\"", "workspace")
            },
        ),
        (
            "same name, other tools and ceiling",
            Card::new("\"read_file\"", "none"),
        ),
    ];
    for (label, other) in attempts {
        let wrong = scoped(&other.identity(), &catalog());
        let result = ResolvedAgent::resolve(&allowed, &wrong, gateway(), "img", SOCKET.into());
        let err = result.unwrap_err();
        assert!(matches!(err, ResolveError::ScopeMismatch { .. }), "{label}");
        assert_eq!(
            err.to_string(),
            "capabilities were scoped by a different identity than `a`",
            "{label}"
        );
    }
    let right = scoped(allowed.identity(), &catalog());
    assert!(ResolvedAgent::resolve(&allowed, &right, gateway(), "img", SOCKET.into()).is_ok());
}

#[test]
fn same_name_global_grant_cannot_serve_a_narrower_local_identity() {
    let global = Card::new("\"*\"", "workspace");
    let local = Card::new("\"read_file\"", "none");
    let global_grant = scoped(&global.identity(), &catalog());
    let allowed_local = allowed_for(&local.identity());
    let err = ResolvedAgent::resolve(
        &allowed_local,
        &global_grant,
        gateway(),
        "img",
        SOCKET.into(),
    )
    .unwrap_err();
    assert!(matches!(err, ResolveError::ScopeMismatch { .. }));
}

#[test]
fn a_widened_grant_fails_closed() {
    let card = Card::new("\"read_file\"", "none");
    let allowed = allowed_for(&card.identity());
    let wide = |spec: CapabilitySpec| {
        let mut grant = scoped(allowed.identity(), &[]);
        grant.specs.push(spec);
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "img", SOCKET.into())
    };
    let outside_patterns = wide(CapabilitySpec::new(
        "write_file",
        "d",
        serde_json::json!({}),
        EffectClass::None,
    ));
    let above_ceiling = wide(CapabilitySpec::new(
        "read_file",
        "d",
        serde_json::json!({}),
        EffectClass::Workspace,
    ));
    for err in [outside_patterns.unwrap_err(), above_ceiling.unwrap_err()] {
        assert!(matches!(err, ResolveError::GrantWiderThanIdentity { .. }));
    }
    let err = wide(CapabilitySpec::new(
        "write_file",
        "d",
        serde_json::json!({}),
        EffectClass::None,
    ))
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        "capability `write_file` is wider than identity `a` allows"
    );
}

#[test]
fn path_restricted_identities_never_hold_run_command() {
    let stub = vec![CapabilitySpec::new(
        "run_command",
        "shell",
        serde_json::json!({}),
        EffectClass::Workspace,
    )];
    let restricted = Card {
        paths: "[\"api/**\"]",
        ..Card::new("\"*\"", "workspace")
    };
    let a = agent(&restricted, &stub);
    assert!(
        a.capabilities().is_empty(),
        "scoped_for must drop run_command"
    );

    let allowed = allowed_for(&restricted.identity());
    let mut forged = scoped(allowed.identity(), &[]);
    forged.specs.push(stub[0].clone());
    let err =
        ResolvedAgent::resolve(&allowed, &forged, gateway(), "img", SOCKET.into()).unwrap_err();
    assert!(matches!(err, ResolveError::GrantWiderThanIdentity { .. }));

    let reads = Card {
        read_paths: "read_paths = [\"**\"]",
        ..Card::new("\"*\"", "workspace")
    };
    assert!(agent(&reads, &stub).capabilities().is_empty());

    let open = Card::new("\"*\"", "workspace");
    assert_eq!(agent(&open, &stub).capability_names(), vec!["run_command"]);
}

#[test]
fn broker_socket_must_live_under_a_nanna_owned_directory() {
    let card = Card::new("\"*\"", "none");
    let bad = [
        "",
        "relative.sock",
        "/",
        "/run",
        "/etc",
        "/dev/kvm",
        "/run/nanna",
        "/run/nanna/",
        "/run/nanna/../etc/passwd",
        "/run/nanna/./b.sock",
        "/run/nanna//b.sock",
        "/run/nanna/a b.sock",
        "/run/nanna/a:b.sock",
        "/run/nanna/a,b.sock",
        "/run/nanna/\u{e9}.sock",
        "/run/nanna/a\0b.sock",
        "/run/nanna/a\\b.sock",
        "/var/run/docker.sock",
        "/var/run/Docker.sock",
        "/run/crio/crio.sock",
        "/run/buildkit/buildkitd.sock",
        "/run/systemd/private",
        "/run/dbus/system_bus_socket",
        "/run/a:/etc:rw.sock",
        "/run/a,b.sock",
        "/run/user/x/nanna/b.sock",
        "/run/user//nanna/b.sock",
        "/run/user/1000/other/b.sock",
        "/run/user/1000/nanna/",
        "/run/user/1000",
        "/run/nannax/b.sock",
    ];
    for path in bad {
        let err = resolve_with(&card, &[], path).unwrap_err();
        assert!(
            matches!(err, ResolveError::InvalidBrokerSocket { .. }),
            "{path:?}"
        );
    }
    let good = [
        "/run/nanna/broker.sock",
        "/run/nanna/a/b-c_d.1.sock",
        "/run/user/1000/nanna/broker.sock",
        "/run/nanna/docker.sock",
        "/run/nanna/mydocker.sock",
    ];
    for path in good {
        assert!(resolve_with(&card, &[], path).is_ok(), "{path:?}");
    }
    let err = resolve_with(&card, &[], "/etc").unwrap_err();
    assert_eq!(err.to_string(), "invalid broker socket path `/etc`");
}

#[test]
fn image_must_be_an_image_reference() {
    let card = Card::new("\"*\"", "none");
    let bad = [
        "",
        "--privileged",
        "-v /:/h",
        "img\n--privileged",
        "img --privileged",
        "a b",
        "a\0b",
        "a\tb",
        "a;b",
        "a$b",
        "a`b",
        "a\u{e9}",
        &"a".repeat(256),
    ];
    for image in bad {
        let identity = card.identity();
        let allowed = allowed_for(&identity);
        let grant = scoped(allowed.identity(), &[]);
        let result = ResolvedAgent::resolve(&allowed, &grant, gateway(), image, SOCKET.into());
        assert!(
            matches!(result, Err(ResolveError::InvalidImage { .. })),
            "{image:?}"
        );
    }
    for image in [
        "nanna-agent:pi",
        "registry.example/team/img:1.2",
        "img@sha256:abc123",
        "a_b.c-d",
        &"a".repeat(255),
    ] {
        let identity = card.identity();
        let allowed = allowed_for(&identity);
        let grant = scoped(allowed.identity(), &[]);
        let result = ResolvedAgent::resolve(&allowed, &grant, gateway(), image, SOCKET.into());
        assert!(result.is_ok(), "{image:?}");
    }
    let identity = card.identity();
    let allowed = allowed_for(&identity);
    let grant = scoped(allowed.identity(), &[]);
    let err = ResolvedAgent::resolve(&allowed, &grant, gateway(), "-x", SOCKET.into()).unwrap_err();
    assert_eq!(err.to_string(), "invalid container image `-x`");
}

#[test]
fn the_agent_is_bound_to_the_audited_subtask_and_repo() {
    let a = wide_agent();
    assert_eq!(a.task_prompt(), "Add a regression test.");
    assert_eq!(a.audited_repo(), "github.com/example/repo");
    let plan = PiAdapter.plan(&a).unwrap();
    assert_eq!(plan.audited_repo, "github.com/example/repo");
    let line: serde_json::Value = serde_json::from_str(&plan.stdin[0]).unwrap();
    assert_eq!(line["message"], "Add a regression test.");
    let mut changed = plan;
    changed.audited_repo = "github.com/example/other".into();
    assert_eq!(
        IsolationPolicy::check(&changed, &a),
        Err(IsolationViolation::RepoChanged)
    );
    assert_eq!(
        IsolationViolation::RepoChanged.to_string(),
        "plan audited repository differs from the agent's"
    );
}

#[test]
fn model_names_cannot_act_as_options_or_hide_whitespace() {
    for model in [
        "--help",
        "-x",
        "a b",
        "a\\nb",
        "a\\tb",
        "a\\u0007b",
        "a\\u007fb",
    ] {
        let card = Card {
            model,
            ..Card::new("\"*\"", "none")
        };
        let err = resolve_with(&card, &[], SOCKET).unwrap_err();
        assert!(
            matches!(err, ResolveError::InvalidModel { .. }),
            "{model:?}"
        );
    }
    let card = Card {
        model: "gemma4:e4b",
        ..Card::new("\"*\"", "none")
    };
    assert!(resolve_with(&card, &[], SOCKET).is_ok());
    let bad = Card {
        model: "-x",
        ..Card::new("\"*\"", "none")
    };
    let err = resolve_with(&bad, &[], SOCKET).unwrap_err();
    assert_eq!(err.to_string(), "invalid model name `-x`");
}

#[test]
fn an_empty_model_is_not_valid() {
    assert!(!model_is_valid(""));
    assert!(!model_is_valid("-x"));
    assert!(!model_is_valid("a b"));
    assert!(!model_is_valid("a\u{7}b"));
    assert!(model_is_valid("gemma4:e4b"));
}

#[test]
fn missing_prompt_file_is_a_resolve_error() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("p.md"), "prompt").unwrap();
    let toml = "[identity]\nname = \"a\"\ndescription = \"d\"\nloop = \"inner\"\nmodel = \"m\"\nsystem_prompt = \"p.md\"\n[scope]\nrepos = [\"r\"]\npaths = [\"**\"]\nmax_effect = \"none\"\ntools = [\"*\"]\n[limits]\nmax_iterations = 1\nmax_wall_clock_secs = 1\nmax_concurrent = 1\n";
    std::fs::write(dir.path().join("a.toml"), toml).unwrap();
    let allowed = allowed_from_dir(dir.path(), "a");
    let grant = scoped(allowed.identity(), &[]);
    std::fs::remove_file(dir.path().join("p.md")).unwrap();
    let err =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "img", SOCKET.into()).unwrap_err();
    assert!(err.to_string().starts_with("system prompt:"));
}

#[test]
fn scoped_capabilities_describe_tools_with_their_effect() {
    let card = Card::new("\"write_file\"", "workspace");
    let identity = card.identity();
    let registry = crate::tools::create_tool_registry(Path::new("."));
    let grant = ScopedCapabilities::scope(registry, &identity);
    assert_eq!(grant.identity(), "a");
    let write = grant
        .specs()
        .iter()
        .find(|s| s.name == "write_file")
        .unwrap();
    assert_eq!(write.effect, EffectClass::Workspace);
    assert_eq!(write.parameters["type"], "object");
    assert_eq!(grant.specs().len(), 1);
}

#[test]
fn handshake_reports_unexpected_and_missing_tools() {
    let available = vec![
        CapabilitySpec::new("read_file", "d", serde_json::json!({}), EffectClass::None),
        CapabilitySpec::new("search", "d", serde_json::json!({}), EffectClass::None),
    ];
    let a = agent(&Card::new("\"read_file\", \"search\"", "none"), &available);
    let plan = launch_plan(&PiAdapter, &a).unwrap();
    let ok = ["search".to_string(), "read_file".to_string()];
    assert!(verify_runtime_tools(&plan, &ok).is_ok());
    let bad = [
        "read_file".to_string(),
        "bash".to_string(),
        "write".to_string(),
    ];
    let err = verify_runtime_tools(&plan, &bad).unwrap_err();
    assert_eq!(err.unexpected, vec!["bash", "write"]);
    assert_eq!(err.missing, vec!["search"]);
    assert!(err.to_string().contains("bash"));
    assert!(verify_runtime_tools(&plan, &[]).is_err());
}

struct Mutating(fn(&mut LaunchPlan));

impl HarnessAdapter for Mutating {
    fn plan(&self, agent: &ResolvedAgent) -> Result<LaunchPlan, Unsupported> {
        let mut plan = PiAdapter.plan(agent)?;
        (self.0)(&mut plan);
        Ok(plan)
    }
}

#[test]
fn launch_plan_rejects_a_misbehaving_adapter() {
    let a = wide_agent();
    let err = launch_plan(&Mutating(|p| p.image = "other".into()), &a).unwrap_err();
    assert!(matches!(
        err,
        PlanError::Isolation(IsolationViolation::Image { .. })
    ));
    assert!(err.to_string().contains("other"));
    assert!(launch_plan(&Mutating(|_| {}), &a).is_ok());
}

#[test]
fn launch_plan_surfaces_unsupported() {
    let a = agent(&Card::new("\"nothing\"", "none"), &[]);
    let err = launch_plan(&PiAdapter, &a).unwrap_err();
    assert!(matches!(
        err,
        PlanError::Unsupported(Unsupported::NoCapabilities { .. })
    ));
}

fn checked_plan() -> (ResolvedAgent, LaunchPlan) {
    let a = wide_agent();
    let plan = PiAdapter.plan(&a).unwrap();
    (a, plan)
}

fn first_violation(plan: &LaunchPlan, a: &ResolvedAgent) -> Option<IsolationViolation> {
    IsolationPolicy::check(plan, a).err()
}

#[test]
fn conforming_plan_passes_the_policy() {
    let (a, plan) = checked_plan();
    assert_eq!(IsolationPolicy::check(&plan, &a), Ok(()));
    assert!(IsolationPolicy::violations(&plan, &a).is_empty());
}

#[test]
fn policy_rejects_each_kind_of_violation() {
    let (a, base) = checked_plan();
    let mut cases: Vec<(&str, LaunchPlan)> = Vec::new();
    let mut p = base.clone();
    p.image = "other".into();
    cases.push(("image", p));
    let mut p = base.clone();
    p.broker_socket = "/var/run/docker.sock".into();
    cases.push(("socket", p));
    let mut p = base.clone();
    p.gateway = Endpoint::new("evil.example", 443).unwrap();
    cases.push(("gateway host", p));
    let mut p = base.clone();
    p.gateway = Endpoint::new("model-gateway", 11435).unwrap();
    cases.push(("gateway port", p));
    let mut p = base.clone();
    p.exposed_capabilities.push("bash".into());
    cases.push(("capability", p));
    let mut p = base.clone();
    p.argv.clear();
    cases.push(("argv", p));
    let mut p = base.clone();
    p.limits.max_iterations += 1;
    cases.push(("iterations", p));
    let mut p = base.clone();
    p.limits.max_wall_clock_secs += 1;
    cases.push(("deadline", p));
    let mut p = base.clone();
    p.scope.paths.clear();
    cases.push(("paths", p));
    let mut p = base.clone();
    p.scope.read_paths = Some(vec![]);
    cases.push(("read paths", p));
    let mut p = base.clone();
    p.scope.repos.push("other".into());
    cases.push(("repos", p));
    for (label, plan) in cases {
        assert!(first_violation(&plan, &a).is_some(), "{label}");
    }
    let mut p = base.clone();
    p.gateway = Endpoint::new("evil.example", 443).unwrap();
    assert_eq!(
        first_violation(&p, &a),
        Some(IsolationViolation::Gateway {
            host: "evil.example".into(),
            port: 443
        })
    );
    let mut p = base;
    p.exposed_capabilities.push("bash".into());
    assert_eq!(
        first_violation(&p, &a),
        Some(IsolationViolation::Capability {
            name: "bash".into()
        })
    );
}

#[test]
fn exposing_fewer_capabilities_than_granted_is_allowed() {
    let (a, mut plan) = checked_plan();
    plan.exposed_capabilities.clear();
    assert_eq!(IsolationPolicy::check(&plan, &a), Ok(()));
}

#[test]
fn env_is_an_exact_name_value_allowlist() {
    let (a, base) = checked_plan();
    let bad: [(&str, &str); 7] = [
        ("GITHUB_TOKEN", "x"),
        ("PATH", "/usr/bin"),
        ("LD_PRELOAD", "/x.so"),
        ("NANNA_ANYTHING", "x"),
        ("NANNA_GITHUB_TOKEN", "x"),
        ("HOME", "/root"),
        ("PI_CODING_AGENT_DIR", "/elsewhere"),
    ];
    for (name, value) in bad {
        let mut plan = base.clone();
        plan.env.push((name.into(), value.into()));
        assert_eq!(
            first_violation(&plan, &a),
            Some(IsolationViolation::EnvNotAllowed { name: name.into() }),
            "{name}={value}"
        );
    }
    let mut dup = base.clone();
    dup.env.push(dup.env[0].clone());
    assert!(matches!(
        first_violation(&dup, &a),
        Some(IsolationViolation::EnvNotAllowed { .. })
    ));
    let mut wrong_value = base;
    wrong_value.env[2].1 = "/other.sock".into();
    assert!(matches!(
        first_violation(&wrong_value, &a),
        Some(IsolationViolation::EnvNotAllowed { .. })
    ));
}

#[test]
fn files_must_live_under_nanna_without_escapes_or_duplicates() {
    let (a, base) = checked_plan();
    let bad = [
        "relative/x",
        "/etc/passwd",
        "/nanna/../etc/passwd",
        "/nanna/",
        "/nanna",
        "/nanna\\x",
        "C:\\nanna\\x",
        "/nanna/a\0b",
        "/nanna//..//x",
    ];
    for path in bad {
        let mut plan = base.clone();
        plan.files[0].path = path.into();
        assert!(
            matches!(
                first_violation(&plan, &a),
                Some(IsolationViolation::FilePath { .. })
            ),
            "{path:?}"
        );
    }
    let mut plan = base;
    let duplicate = plan.files[0].clone();
    plan.files.push(duplicate);
    assert!(matches!(
        first_violation(&plan, &a),
        Some(IsolationViolation::DuplicateFile { .. })
    ));
}

#[test]
fn violations_reports_every_problem() {
    let (a, mut plan) = checked_plan();
    plan.argv.clear();
    plan.image = "other".into();
    plan.env.push(("API_TOKEN".into(), "x".into()));
    assert_eq!(IsolationPolicy::violations(&plan, &a).len(), 3);
}

#[test]
fn pi_argv_is_pinned_exactly() {
    let (_, plan) = checked_plan();
    let expected: Vec<String> = [
        "pi",
        "--mode",
        "rpc",
        "--no-session",
        "--provider",
        "nanna",
        "--model",
        "gemma4:e4b",
        "--no-builtin-tools",
        "--no-extensions",
        "--extension",
        "/nanna/pi/extensions/nanna-capabilities.ts",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--no-mcp",
        "--no-approve",
        "--offline",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(plan.argv, expected);
}

#[test]
fn pi_env_files_socket_and_gateway_are_pinned_exactly() {
    let (a, plan) = checked_plan();
    let env: Vec<(&str, &str)> = plan
        .env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        env,
        vec![
            ("PI_CODING_AGENT_DIR", "/nanna/pi"),
            ("HOME", "/nanna/work"),
            ("NANNA_BROKER_SOCKET", "/nanna/broker.sock"),
            ("NANNA_CAPABILITIES", "/nanna/pi/capabilities.json"),
        ]
    );
    let paths: Vec<String> = plan
        .files
        .iter()
        .map(|f| f.path.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        paths,
        vec![
            "/nanna/pi/models.json",
            "/nanna/pi/SYSTEM.md",
            "/nanna/pi/capabilities.json",
            "/nanna/pi/extensions/nanna-capabilities.ts",
        ]
    );
    assert_eq!(plan.broker_socket, Path::new(SOCKET));
    assert_eq!(plan.gateway, *a.endpoint());
    assert_eq!(plan.image, "nanna-agent:pi");
}

#[test]
fn pi_plan_exposes_exactly_the_granted_capabilities() {
    let a = agent(
        &Card::new("\"read_file\", \"cargo_*\"", "workspace"),
        &catalog(),
    );
    let plan = launch_plan(&PiAdapter, &a).unwrap();
    assert_eq!(plan.exposed_capabilities, vec!["cargo_check", "read_file"]);
    let file = plan
        .files
        .iter()
        .find(|f| f.path.ends_with("capabilities.json"))
        .unwrap();
    let names: Vec<String> = serde_json::from_str::<Vec<serde_json::Value>>(&file.contents)
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, plan.exposed_capabilities);
}

#[test]
fn pi_models_json_prompt_and_system_files() {
    let a = agent(&Card::new("\"read_file\"", "none"), &catalog());
    let plan = PiAdapter.plan(&a).unwrap();
    let file = |suffix: &str| {
        plan.files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .unwrap()
    };
    let models: serde_json::Value = serde_json::from_str(&file("models.json").contents).unwrap();
    assert_eq!(
        models["providers"]["nanna"]["baseUrl"],
        "http://model-gateway:11434/v1"
    );
    assert_eq!(
        models["providers"]["nanna"]["models"][0]["id"],
        "gemma4:e4b"
    );
    assert_eq!(file("SYSTEM.md").contents, "Be careful.");
    let line: serde_json::Value = serde_json::from_str(&plan.stdin[0]).unwrap();
    assert_eq!(line["type"], "prompt");
    assert_eq!(line["id"], "task");
    assert_eq!(line["message"], "Add a regression test.");
    let ext = &file("nanna-capabilities.ts").contents;
    for needle in [
        "registerTool",
        "setEncoding",
        "TIMEOUT_MS",
        "MAX_REPLY_BYTES",
        "throw new Error",
    ] {
        assert!(ext.contains(needle), "{needle}");
    }
}

#[test]
fn pi_gateway_is_exactly_the_endpoint_at_every_ceiling() {
    for ceiling in EffectClass::ALL {
        let a = agent(&Card::new("\"read_file\"", ceiling.as_str()), &catalog());
        let plan = launch_plan(&PiAdapter, &a).unwrap();
        assert_eq!(plan.gateway, gateway(), "{ceiling}");
    }
}

#[test]
fn no_capabilities_is_unsupported() {
    let a = agent(&Card::new("\"nothing\"", "none"), &catalog());
    let err = PiAdapter.plan(&a).unwrap_err();
    assert_eq!(err, Unsupported::NoCapabilities { agent: "a".into() });
    assert_eq!(err.to_string(), "agent `a` has no capabilities");
}

#[test]
fn unrepresentable_capability_names_are_unsupported() {
    for name in ["Read", "read-file", "1read", "x\"y", &"a".repeat(65)] {
        let available = vec![CapabilitySpec::new(
            name,
            "d",
            serde_json::json!({}),
            EffectClass::None,
        )];
        let a = agent(&Card::new("\"*\"", "none"), &available);
        let err = PiAdapter.plan(&a).unwrap_err();
        assert!(matches!(err, Unsupported::CapabilityName { .. }), "{name}");
    }
    let available = vec![CapabilitySpec::new(
        "a".repeat(64),
        "d",
        serde_json::json!({}),
        EffectClass::None,
    )];
    assert!(PiAdapter
        .plan(&agent(&Card::new("\"*\"", "none"), &available))
        .is_ok());
}

#[test]
fn narrower_identity_never_widens_the_plan() {
    let wide = PiAdapter.plan(&wide_agent()).unwrap();
    let narrow = PiAdapter
        .plan(&agent(&Card::new("\"read_file\"", "none"), &catalog()))
        .unwrap();
    for name in &narrow.exposed_capabilities {
        assert!(wide.exposed_capabilities.contains(name));
    }
    assert!(narrow.exposed_capabilities.len() < wide.exposed_capabilities.len());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    #[test]
    fn plans_always_satisfy_isolation(
        mask in proptest::collection::vec(any::<bool>(), 5),
        effect in prop::sample::select(vec!["none", "workspace", "repository", "ci", "sandbox", "production"]),
    ) {
        let patterns: Vec<String> = catalog()
            .iter()
            .zip(&mask)
            .filter(|(_, keep)| **keep)
            .map(|(c, _)| format!("\"{}\"", c.name))
            .collect();
        prop_assume!(!patterns.is_empty());
        let joined = patterns.join(",");
        let a = agent(&Card::new(&joined, effect), &catalog());
        if let Ok(plan) = PiAdapter.plan(&a) {
            prop_assert_eq!(IsolationPolicy::check(&plan, &a), Ok(()));
            for name in &plan.exposed_capabilities {
                let spec = catalog().into_iter().find(|c| &c.name == name).unwrap();
                prop_assert!(spec.effect <= a.max_effect());
            }
        }
    }
}

#[test]
fn real_registry_grant_matches_patterns_and_is_sorted() {
    let card = Card::new("\"read_file\", \"git_*\", \"search\"", "workspace");
    let identity = card.identity();
    let allowed = allowed_for(&identity);
    let registry = crate::tools::create_tool_registry(Path::new("."));
    let grant = ScopedCapabilities::scope(registry, allowed.identity());
    let a = ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
        .unwrap();
    let names = a.capability_names();
    assert!(names.contains(&"read_file") && names.contains(&"search"));
    assert!(names.contains(&"git_status") && names.contains(&"git_diff"));
    for name in &names {
        assert!(identity.allows_tool(name), "{name}");
    }
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert!(a
        .capabilities()
        .iter()
        .all(|c| c.effect <= EffectClass::Workspace));
}

#[test]
fn metadata_hosts_are_rejected_but_private_ranges_are_not() {
    for host in ["metadata.google.internal", "METADATA.GOOGLE.INTERNAL"] {
        assert!(Endpoint::new(host, 80).is_err(), "{host}");
    }
    for host in ["10.0.0.5", "192.168.1.10", "100.64.0.1"] {
        assert!(Endpoint::new(host, 80).is_ok(), "{host}");
    }
}

#[test]
fn numeric_looking_and_non_ascii_hosts_are_rejected() {
    let hosts = [
        "0x",
        "0X",
        "0X7f000001",
        "0x7f000001",
        "1.0X7f",
        "gw\u{e9}",
        "\u{ff47}\u{ff57}",
        "g\u{e9}.example",
    ];
    for host in hosts {
        assert!(Endpoint::new(host, 80).is_err(), "{host:?}");
    }
    assert!(Endpoint::new("0xzz", 80).is_ok());
    assert!(Endpoint::new("gw1", 80).is_ok());
}

#[test]
fn file_paths_reject_backslashes_and_the_broker_mount_path() {
    let (a, base) = checked_plan();
    for path in [
        "/nanna/a\\b",
        "/nanna/pi\\models.json",
        "/nanna/broker.sock",
    ] {
        let mut plan = base.clone();
        plan.files[0].path = path.into();
        let result = first_violation(&plan, &a);
        assert!(
            matches!(result, Some(IsolationViolation::FilePath { .. })),
            "{path:?}"
        );
    }
    assert_eq!(first_violation(&base, &a), None);
}

#[test]
fn pi_models_json_pins_provider_fields() {
    let (_, plan) = checked_plan();
    let file = plan
        .files
        .iter()
        .find(|f| f.path.ends_with("models.json"))
        .unwrap();
    let models: serde_json::Value = serde_json::from_str(&file.contents).unwrap();
    let provider = &models["providers"]["nanna"];
    assert_eq!(provider["api"], "openai-completions");
    assert_eq!(provider["apiKey"], "nanna");
    assert_eq!(provider["baseUrl"], "http://model-gateway:11434/v1");
}

#[test]
fn capability_names_may_contain_digits_and_underscores() {
    for name in ["cargo_v2", "a1", "t_9", "x_"] {
        let available = vec![CapabilitySpec::new(
            name,
            "d",
            serde_json::json!({}),
            EffectClass::None,
        )];
        let a = agent(&Card::new("\"*\"", "none"), &available);
        assert!(PiAdapter.plan(&a).is_ok(), "{name}");
    }
}

#[test]
fn env_name_must_match_not_just_the_value() {
    let (a, base) = checked_plan();
    let mut plan = base;
    plan.env.push(("FOO".into(), "/nanna/work".into()));
    assert_eq!(
        first_violation(&plan, &a),
        Some(IsolationViolation::EnvNotAllowed { name: "FOO".into() })
    );
}

#[test]
fn gateway_host_must_match_even_with_the_same_port() {
    let (a, base) = checked_plan();
    let mut plan = base;
    plan.gateway = Endpoint::new("evil.example", 11434).unwrap();
    assert_eq!(
        first_violation(&plan, &a),
        Some(IsolationViolation::Gateway {
            host: "evil.example".into(),
            port: 11434
        })
    );
}

#[test]
fn check_returns_the_first_violation() {
    let (a, base) = checked_plan();
    let mut plan = base;
    plan.argv.clear();
    plan.image = "other".into();
    plan.audited_repo = "github.com/example/other".into();
    let all = IsolationPolicy::violations(&plan, &a);
    assert!(all.len() >= 3);
    assert_eq!(all[0], IsolationViolation::EmptyArgv);
    assert_eq!(IsolationPolicy::check(&plan, &a), Err(all[0].clone()));
}

#[test]
fn capability_names_may_not_contain_uppercase_after_the_first_character() {
    for name in ["aB", "cargo_Check", "readFile", "a_B_c"] {
        let available = vec![CapabilitySpec::new(
            name,
            "d",
            serde_json::json!({}),
            EffectClass::None,
        )];
        let a = agent(&Card::new("\"*\"", "none"), &available);
        let err = PiAdapter.plan(&a).unwrap_err();
        assert!(matches!(err, Unsupported::CapabilityName { .. }), "{name}");
    }
}

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
    allowed: crate::auditor::Allowed,
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
    let identity = card.identity();
    let allowed = allowed_for(&identity);
    let grant = scoped(allowed.identity(), &specs_of(&tools));
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    let plan = launch_plan(&PiAdapter, &agent).unwrap();
    let (registry, counters) = base_registry(tools);
    let bound = BoundWorkspace::for_test(registry.scoped_for(allowed.identity()));
    let sink = Arc::new(RingAuditSink::default());
    let broker = Broker::for_agent(&allowed, &agent, bound, policy, sink.clone()).unwrap();
    (
        broker,
        Parts {
            allowed,
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
    match outcome {
        BrokerOutcome::ToolError { kind, .. } => Some(*kind),
        _ => None,
    }
}

#[test]
fn the_documented_limits_are_pinned() {
    assert_eq!(MAX_OUTPUT_BYTES, 1_000_000);
    assert_eq!(MAX_REQUEST_BYTES, 1_048_576);
    assert_eq!(BrokerPolicy::default().max_effect(), EffectClass::Workspace);
}

#[test]
fn arming_requires_the_runtime_to_report_exactly_the_planned_tools() {
    let tools = vec![
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(1)),
        ),
        ("search", EffectClass::None, ok_with(serde_json::json!(2))),
    ];
    let extra = vec![
        "read_file".to_string(),
        "search".to_string(),
        "bash".to_string(),
    ];
    let (broker, parts) = build_with(
        &Card::new("\"*\"", "workspace"),
        tools,
        BrokerPolicy::default(),
    );
    let err = broker.arm(&parts.plan, &extra).err().unwrap();
    assert!(matches!(err, ArmError::Handshake(_)));
    assert!(err.to_string().contains("bash"));
    let tools = vec![
        (
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(1)),
        ),
        ("search", EffectClass::None, ok_with(serde_json::json!(2))),
    ];
    let (broker, parts) = build_with(
        &Card::new("\"*\"", "workspace"),
        tools,
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
    let tools = || {
        vec![
            (
                "read_file",
                EffectClass::None,
                ok_with(serde_json::json!(1)),
            ),
            ("search", EffectClass::None, ok_with(serde_json::json!(2))),
        ]
    };
    let base = Card::new("\"*\"", "workspace");
    let plan_for = |card: &Card,
                    repo: &str,
                    socket: &str,
                    image: &str,
                    tools: Vec<(&str, EffectClass, Behavior)>| {
        let identity = card.identity();
        let allowed = allowed_for_repo(&identity, repo);
        let grant = scoped(allowed.identity(), &specs_of(&tools));
        let agent =
            ResolvedAgent::resolve(&allowed, &grant, gateway(), image, socket.into()).unwrap();
        launch_plan(&PiAdapter, &agent).unwrap()
    };
    let repo = "github.com/example/repo";
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
    let narrower = Card::new("\"read_file\"", "workspace");
    let cases: Vec<(&str, CheckedPlan)> = vec![
        (
            "agent",
            plan_for(&other_agent, repo, SOCKET, "nanna-agent:pi", tools()),
        ),
        (
            "audited repository",
            plan_for(
                &base,
                "github.com/example/other",
                SOCKET,
                "nanna-agent:pi",
                tools(),
            ),
        ),
        (
            "model",
            plan_for(&other_model, repo, SOCKET, "nanna-agent:pi", tools()),
        ),
        (
            "image",
            plan_for(&base, repo, SOCKET, "nanna-agent:other", tools()),
        ),
        (
            "limits",
            plan_for(&other_limits, repo, SOCKET, "nanna-agent:pi", tools()),
        ),
        (
            "scope",
            plan_for(&other_scope, repo, SOCKET, "nanna-agent:pi", tools()),
        ),
        (
            "broker socket",
            plan_for(
                &base,
                repo,
                "/run/nanna/other.sock",
                "nanna-agent:pi",
                tools(),
            ),
        ),
        (
            "capabilities",
            plan_for(&narrower, repo, SOCKET, "nanna-agent:pi", tools()),
        ),
    ];
    for (field, plan) in cases {
        let (broker, parts) = build_with(&base, tools(), BrokerPolicy::default());
        let err = broker.arm(&plan, &parts.reported()).err().unwrap();
        assert!(
            matches!(err, ArmError::PlanMismatch { field: f } if f == field),
            "{field}: {err}"
        );
        assert_eq!(
            err.to_string(),
            format!("plan does not belong to this broker: {field} differs")
        );
    }
    let (broker, parts) = build_with(&base, tools(), BrokerPolicy::default());
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
        match broker.arm(&parts.plan, &parts.reported()).err().unwrap() {
            ArmError::ExposedNotServed { names } => assert_eq!(names, unserved, "{ceiling}"),
            other => panic!("{ceiling}: {other:?}"),
        }
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
    ];
    for (tool, kind) in expect {
        match broker.handle(req(tool)).await {
            BrokerOutcome::ToolError { kind: got, message } => {
                assert_eq!(got, kind, "{tool}");
                assert!(!message.is_empty());
            }
            other => panic!("{tool}: {other:?}"),
        }
    }
    assert!(!broker.is_halted());
    let held = broker.handle(req("t_held")).await;
    assert_eq!(
        held,
        BrokerOutcome::Halt(HaltReason::ProductionHeld {
            repo: "r".into(),
            escalation_id: "e1".into(),
            summary: "down".into()
        })
    );
    assert!(broker.is_halted());
    assert_eq!(
        parts.sink.entries().pop().unwrap().outcome,
        AuditOutcome::Halt
    );
    let reply = held.to_response();
    assert!(reply.halt && !reply.ok && reply.kind.is_none());
    let text = reply.output.as_str().unwrap();
    assert_eq!(text, "the task was halted by the host");
    for secret in ["incident", "e1", "down", "r"] {
        if secret.len() > 1 {
            assert!(!text.contains(secret), "{secret}");
        }
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

#[test]
fn building_refuses_an_agent_that_differs_from_the_audit() {
    let tools = || {
        vec![(
            "read_file",
            EffectClass::None,
            ok_with(serde_json::json!(0)),
        )]
    };
    let (_, parts) = build_with(
        &Card::new("\"*\"", "workspace"),
        tools(),
        BrokerPolicy::default(),
    );
    let attempts: Vec<(&str, Card, &str)> = vec![
        (
            "name",
            Card {
                name: "other",
                ..Card::new("\"*\"", "workspace")
            },
            "github.com/example/repo",
        ),
        (
            "scope",
            Card {
                paths: "[\"api/**\"]",
                ..Card::new("\"*\"", "workspace")
            },
            "github.com/example/repo",
        ),
        (
            "read paths",
            Card {
                read_paths: "read_paths = [\"**\"]",
                ..Card::new("\"*\"", "workspace")
            },
            "github.com/example/repo",
        ),
        (
            "repos",
            Card {
                repos: "[\"r\", \"s\"]",
                ..Card::new("\"*\"", "workspace")
            },
            "github.com/example/repo",
        ),
        (
            "ceiling",
            Card::new("\"*\"", "none"),
            "github.com/example/repo",
        ),
        (
            "repo",
            Card::new("\"*\"", "workspace"),
            "github.com/example/other",
        ),
    ];
    for (label, card, repo) in attempts {
        let other_allowed = allowed_for_repo(&card.identity(), repo);
        let (registry, _) = base_registry(tools());
        let bound = BoundWorkspace::for_test(registry.scoped_for(other_allowed.identity()));
        let result = Broker::for_agent(
            &other_allowed,
            &parts.agent,
            bound,
            BrokerPolicy::default(),
            Arc::new(RingAuditSink::default()),
        );
        let err = result.err().unwrap_or_else(|| panic!("{label} accepted"));
        assert!(
            matches!(err, BrokerError::IdentityMismatch { .. }),
            "{label}"
        );
        assert!(err
            .to_string()
            .contains("does not match the resolved agent"));
    }
    let _ = &parts.allowed;
}

#[test]
fn a_registry_must_offer_exactly_the_agents_capabilities_and_effects() {
    use super::bind::registry_matches;
    let card = Card::new("\"*\"", "workspace");
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
    let (_, parts) = build_with(&card, tools(), BrokerPolicy::default());
    let build = |tools: Vec<(&str, EffectClass, Behavior)>| base_registry(tools).0;
    assert!(registry_matches(&parts.agent, &build(tools())).is_ok());
    let extra = vec![
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
        ("bash", EffectClass::None, ok_with(serde_json::json!(0))),
    ];
    let err = registry_matches(&parts.agent, &build(extra)).unwrap_err();
    assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "bash"));
    let missing = vec![(
        "read_file",
        EffectClass::None,
        ok_with(serde_json::json!(0)),
    )];
    let err = registry_matches(&parts.agent, &build(missing)).unwrap_err();
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
    let err = registry_matches(&parts.agent, &build(effect)).unwrap_err();
    assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "write_file"));
    assert_eq!(
        err.to_string(),
        "workspace registry differs from the agent's capabilities at `write_file`"
    );
}

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn repo_with_origin(url: &str) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    git(dir.path(), &["init"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(dir.path().join("api")).unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("README.md"), "# r").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-m", "init"]);
    git(dir.path(), &["remote", "add", "origin", url]);
    dir
}

#[tokio::test]
async fn bound_to_a_real_workspace_an_out_of_path_write_is_refused_end_to_end() {
    use crate::workspace::TaskWorkspace;
    let _path = crate::test_support::hold_path_async().await;
    let repo = repo_with_origin("https://github.com/example/repo.git");
    let mut workspace = TaskWorkspace::create(repo.path(), "t-e2e", "HEAD").unwrap();

    let local = Card {
        paths: "[\"api/**\"]",
        repos: "[\"github.com/example/repo\"]",
        ..Card::new("\"write_file\"", "workspace")
    };
    let identity = local.identity();
    let allowed = allowed_for(&identity);
    let grant = ScopedCapabilities::scope(
        workspace
            .build_tool_registry_for(allowed.identity())
            .unwrap(),
        allowed.identity(),
    );
    let agent =
        ResolvedAgent::resolve(&allowed, &grant, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    assert_eq!(agent.capability_names(), vec!["write_file"]);

    let global = Card {
        repos: "[\"github.com/example/repo\"]",
        ..Card::new("\"*\"", "workspace")
    };
    let global_allowed = allowed_for(&global.identity());
    let err = agent
        .bind_workspace(&global_allowed, &workspace)
        .err()
        .unwrap();
    assert!(matches!(err, BindError::AgentMismatch));

    let empty = scoped(allowed.identity(), &[]);
    let hollow =
        ResolvedAgent::resolve(&allowed, &empty, gateway(), "nanna-agent:pi", SOCKET.into())
            .unwrap();
    let err = hollow.bind_workspace(&allowed, &workspace).err().unwrap();
    assert!(matches!(err, BindError::CapabilityMismatch { ref tool } if tool == "write_file"));

    let bound = agent.bind_workspace(&allowed, &workspace).unwrap();
    let sink = Arc::new(RingAuditSink::default());
    let broker = Broker::for_agent(&allowed, &agent, bound, BrokerPolicy::default(), sink).unwrap();
    let plan = launch_plan(&PiAdapter, &agent).unwrap();
    let armed = broker
        .arm(&plan, &plan.exposed_capabilities.clone())
        .unwrap();

    let denied = armed
        .handle(BrokerRequest {
            tool: "write_file".into(),
            args: serde_json::json!({"path": "src/x.rs", "content": "x"}),
        })
        .await;
    assert_eq!(kind_of(&denied), Some(ToolErrorKind::ScopeDenied));
    assert!(!workspace.workspace_path.join("src/x.rs").exists());
    let written = armed
        .handle(BrokerRequest {
            tool: "write_file".into(),
            args: serde_json::json!({"path": "api/x.rs", "content": "x"}),
        })
        .await;
    assert!(matches!(written, BrokerOutcome::Output(_)));
    assert!(workspace.workspace_path.join("api/x.rs").exists());

    let other = repo_with_origin("https://github.com/example/other.git");
    let other_workspace = TaskWorkspace::create(other.path(), "t-other", "HEAD").unwrap();
    let err = agent
        .bind_workspace(&allowed, &other_workspace)
        .err()
        .unwrap();
    assert!(matches!(err, BindError::RepoMismatch { .. }));
    assert!(err.to_string().contains("is not the audited repository"));

    let outside = Card::new("\"write_file\"", "workspace");
    let outside_allowed = allowed_for(&outside.identity());
    let outside_grant = ScopedCapabilities::scope(
        workspace
            .build_tool_registry_for(outside_allowed.identity())
            .unwrap(),
        outside_allowed.identity(),
    );
    let outside_agent = ResolvedAgent::resolve(
        &outside_allowed,
        &outside_grant,
        gateway(),
        "nanna-agent:pi",
        SOCKET.into(),
    )
    .unwrap();
    let err = outside_agent
        .bind_workspace(&outside_allowed, &workspace)
        .err()
        .unwrap();
    assert!(matches!(err, BindError::Scope(_)), "{err}");

    let mut other_workspace = other_workspace;
    other_workspace.cleanup().unwrap();
    workspace.cleanup().unwrap();
}

#[test]
fn a_plan_naming_another_agent_or_model_is_rejected_by_the_policy() {
    let (a, base) = checked_plan();
    let mut plan = base.clone();
    plan.agent = "someone-else".into();
    assert_eq!(
        first_violation(&plan, &a),
        Some(IsolationViolation::AgentChanged {
            plan: "someone-else".into(),
            agent: "a".into()
        })
    );
    assert_eq!(
        IsolationViolation::AgentChanged {
            plan: "x".into(),
            agent: "y".into()
        }
        .to_string(),
        "plan agent `x` differs from the agent `y`"
    );
    let mut plan = base;
    plan.model = "other".into();
    assert_eq!(
        first_violation(&plan, &a),
        Some(IsolationViolation::ModelChanged {
            plan: "other".into(),
            agent: "gemma4:e4b".into()
        })
    );
    assert_eq!(
        IsolationViolation::ModelChanged {
            plan: "x".into(),
            agent: "y".into()
        }
        .to_string(),
        "plan model `x` differs from the agent model `y`"
    );
}
