use super::testing::{allowed_for, allowed_from_dir, scoped};
use super::*;
use crate::identity::AgentIdentity;
use proptest::prelude::*;
use std::path::Path;

const SOCKET: &str = "/run/nanna/broker.sock";

struct Card<'a> {
    name: &'a str,
    model: &'a str,
    paths: &'a str,
    read_paths: &'a str,
    effect: &'a str,
    tools: &'a str,
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
repos = ["r"]
paths = {}
{}
max_effect = "{}"
tools = [{}]
[limits]
max_iterations = 7
max_wall_clock_secs = 9
max_concurrent = 2
"#,
            self.name, self.model, self.paths, self.read_paths, self.effect, self.tools
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
