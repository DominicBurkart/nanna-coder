use harness::effects::EffectClass;
use harness::plugins::curated::curated_set;
use harness::plugins::load::{build_catalog, PluginSet, REPO_PLUGINS_DIR};
use harness::plugins::policy::apply_policy;
use harness::plugins::render::{render_capabilities, PluginError, PluginSource};
use harness::plugins::types::HostPolicy;
use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go_stack")
}

fn merged() -> PluginSet {
    let mut set = curated_set();
    let mut repo = PluginSet::default();
    repo.load_dir(&fixture().join(REPO_PLUGINS_DIR), PluginSource::Repo)
        .unwrap();
    set.extend_definitions(&repo).unwrap();
    set.agents.extend(repo.agents);
    set
}

fn allow_go() -> HostPolicy {
    HostPolicy {
        max_effect: EffectClass::Workspace,
        network: false,
        allowed_verbs: vec!["check".into(), "test".into(), "format".into()],
    }
}

#[test]
fn the_go_stack_is_detected_from_go_mod() {
    let set = merged();
    assert_eq!(set.detect(&fixture()), ["go"]);
}

#[test]
fn the_go_implementer_renders_from_the_go_stack() {
    let set = merged();
    let agent = set.render("go-implementer").unwrap();

    assert_eq!(agent.source, PluginSource::Repo);
    assert_eq!(agent.max_effect, EffectClass::Workspace);
    assert_eq!(agent.write_paths, ["**/*.go", "go.mod", "**/*_test.go"]);
    assert!(agent.description.contains("go repository"));
    assert!(agent.system_prompt.contains("go vet ./..."));
    assert!(agent.system_prompt.contains("go test ./..."));
    assert!(agent.system_prompt.contains("gofmt -l -w {{files}}"));
    assert!(!agent.system_prompt.contains("cargo"));

    let names: Vec<&str> = agent.tools.iter().map(|t| t.name.as_str()).collect();
    for tool in [
        "check",
        "test",
        "format",
        "read_file",
        "write_file",
        "finish",
    ] {
        assert!(names.contains(&tool), "{tool} missing from {names:?}");
    }
    assert!(!names.iter().any(|n| n.starts_with("cargo")));

    let block = render_capabilities(&agent);
    assert!(block.contains("argv=`go vet ./...`"), "{block}");
    assert!(block.contains("argv=`go test ./...`"), "{block}");
    assert!(block.contains("argv=`gofmt -l -w {{files}}`"), "{block}");
    assert!(block.contains("**/*.go, go.mod, **/*_test.go"), "{block}");
}

#[test]
fn the_default_host_policy_refuses_the_repo_go_agent() {
    let set = merged();
    let agent = set.render("go-implementer").unwrap();
    assert!(apply_policy(&agent, &HostPolicy::default()).is_err());
    assert!(apply_policy(&agent, &allow_go()).is_ok());
}

#[test]
fn the_same_implementer_role_renders_for_rust_cargo_unchanged() {
    let set = merged();
    let go = set.render("go-implementer").unwrap();
    let rust = set.render("rust-implementer").unwrap();

    assert_eq!(rust.source, PluginSource::Curated);
    assert!(rust.system_prompt.contains("cargo check"));
    assert!(rust.system_prompt.contains("cargo test"));
    let rust_names: Vec<&str> = rust.tools.iter().map(|t| t.name.as_str()).collect();
    for tool in ["cargo_check", "cargo_test", "cargo_format"] {
        assert!(rust_names.contains(&tool), "{tool}");
    }
    assert_eq!(rust.max_effect, go.max_effect);
    assert_eq!(rust.limits, go.limits);
    assert_eq!(set.roles["implementer"].source, PluginSource::Curated);
}

#[test]
fn the_catalog_includes_the_go_agent_as_a_repo_agent_and_gates_it_on_policy() {
    let repos = vec!["github.com/example/gostack".to_string()];
    let built = build_catalog(None, Some(&fixture()), &repos, None, &allow_go()).unwrap();
    let entry = built.get("go-implementer").unwrap();
    assert_eq!(entry.source, PluginSource::Repo);
    assert!(entry.identity.allows_tool("check"));
    assert!(entry.identity.allows_tool("format"));
    assert!(!entry.identity.allows_tool("cargo_check"));
    assert_eq!(entry.identity.scope.repos, repos);

    let err =
        build_catalog(None, Some(&fixture()), &repos, None, &HostPolicy::default()).unwrap_err();
    assert!(matches!(err, PluginError::Policy(_)), "{err}");
}
