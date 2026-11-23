//! Loads the fixture identity catalog under `tests/fixtures/identities` and
//! layers the fixture repository's `.nanna/agents/` overrides on top.

use harness::effects::EffectClass;
use harness::identity::{DevLoop, IdentityCatalog, IdentityError, SystemPrompt};
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/identities")
}

#[test]
fn global_catalog_loads_all_fixture_identities() {
    let catalog = IdentityCatalog::load(fixtures().join("global")).unwrap();
    assert_eq!(
        catalog.names().collect::<Vec<_>>(),
        vec![
            "auditor",
            "deployer",
            "incident-responder",
            "pr-shepherd",
            "rust-implementer"
        ]
    );

    let auditor = catalog.get("auditor").unwrap();
    assert_eq!(auditor.scope.max_effect, EffectClass::None);
    assert!(auditor.scope.tools.is_empty());
    assert!(auditor
        .system_prompt_text()
        .unwrap()
        .starts_with("# auditor"));

    let implementer = catalog.get("rust-implementer").unwrap();
    assert_eq!(implementer.identity.dev_loop, DevLoop::Inner);
    assert_eq!(implementer.scope.max_effect, EffectClass::Repository);
    assert!(implementer
        .system_prompt_text()
        .unwrap()
        .starts_with("# rust-implementer"));
    assert!(implementer.allows_tool("cargo_check"));
    assert!(!implementer.allows_tool("run_command"));

    let shepherd = catalog.get("pr-shepherd").unwrap();
    assert_eq!(shepherd.identity.dev_loop, DevLoop::Middle);
    assert_eq!(
        shepherd.scope.read_paths.as_deref(),
        Some(&["**".to_string()][..])
    );
    assert!(shepherd.allows_tool("github_pr_status"));
    assert!(shepherd.allows_effect(EffectClass::Ci));
    assert!(!shepherd.allows_effect(EffectClass::Sandbox));

    let deployer = catalog.get("deployer").unwrap();
    assert_eq!(deployer.identity.dev_loop, DevLoop::Outer);
    assert!(matches!(
        deployer.identity.system_prompt,
        SystemPrompt::Inline(_)
    ));
    assert!(deployer.scope.paths.is_empty());
    assert_eq!(deployer.limits.max_concurrent, 1);
}

#[test]
fn repo_overrides_narrow_the_global_implementer() {
    let global = IdentityCatalog::load(fixtures().join("global")).unwrap();
    let catalog = global
        .clone()
        .with_repo_overrides(fixtures().join("repo"))
        .unwrap();
    assert_eq!(catalog.len(), global.len());

    let implementer = catalog.get("rust-implementer").unwrap();
    assert_eq!(
        implementer.source(),
        fixtures().join("repo/.nanna/agents/rust-implementer.toml")
    );
    assert_eq!(implementer.scope.max_effect, EffectClass::Workspace);
    assert_eq!(implementer.scope.paths, vec!["api/**"]);
    assert!(!implementer.allows_tool("git_status"));
    assert!(implementer
        .system_prompt_text()
        .unwrap()
        .contains("Never edit shared/"));
    assert!(implementer
        .narrows(global.get("rust-implementer").unwrap())
        .is_ok());

    assert_eq!(catalog.get("deployer"), global.get("deployer"));
    assert_eq!(catalog.get("pr-shepherd"), global.get("pr-shepherd"));
}

#[test]
fn every_fixture_identity_round_trips_through_toml() {
    let catalog = IdentityCatalog::load(fixtures().join("global")).unwrap();
    for identity in catalog.iter() {
        let toml = identity.to_toml_string().unwrap();
        let back =
            harness::identity::AgentIdentity::from_toml_str(&toml, identity.source()).unwrap();
        assert_eq!(&back, identity, "{}", identity.name());
    }
}

#[test]
fn fixture_catalog_renders_a_table() {
    let catalog = IdentityCatalog::load(fixtures().join("global")).unwrap();
    let table = catalog.render_table();
    let lines: Vec<&str> = table.lines().collect();
    assert_eq!(
        lines[0].split_whitespace().collect::<Vec<_>>(),
        vec!["NAME", "LOOP", "MODEL", "MAX_EFFECT"]
    );
    assert_eq!(
        lines[1].split_whitespace().collect::<Vec<_>>(),
        vec!["auditor", "inner", "gemma4:e4b", "none"]
    );
    assert_eq!(
        lines[2].split_whitespace().collect::<Vec<_>>(),
        vec!["deployer", "outer", "gemma4:e4b", "sandbox"]
    );
    assert_eq!(lines.len(), 6);
}

#[test]
fn a_global_catalog_used_as_repo_root_has_no_overrides() {
    let global = IdentityCatalog::load(fixtures().join("global")).unwrap();
    let same = global
        .clone()
        .with_repo_overrides(fixtures().join("global"))
        .unwrap();
    assert_eq!(same, global);
    let err = IdentityCatalog::load(fixtures().join("does-not-exist")).unwrap_err();
    assert!(matches!(err, IdentityError::Io { .. }), "{err}");
}

const HUB_TOOLS: [&str; 6] = [
    "msg_send",
    "msg_recv",
    "session_set_note",
    "sessions_list",
    "session_describe",
    "sessions_tree",
];

fn read_cards(dir: &std::path::Path) -> Vec<(String, toml::Table)> {
    let mut cards = Vec::new();
    for entry in std::fs::read_dir(dir).expect("directory reads") {
        let path = entry.expect("entry reads").path();
        let named = path
            .file_stem()
            .expect("card has a name")
            .to_string_lossy()
            .to_string();
        if path.extension().is_some_and(|ext| ext == "toml") {
            let text = std::fs::read_to_string(&path).expect("card reads");
            cards.push((named, text.parse().expect("card parses")));
        }
    }
    cards
}

fn reach_of(card: &toml::Table) -> String {
    let reach = card["scope"].get("message_reach");
    let named = reach.and_then(|value| value.as_str());
    named.unwrap_or("none").to_string()
}

fn tools_of(card: &toml::Table) -> Vec<String> {
    let listed = card["scope"]["tools"].as_array().expect("tools is a list");
    listed
        .iter()
        .map(|tool| tool.as_str().expect("tool is text").to_string())
        .collect()
}

#[test]
fn shipped_cards_reach_the_root_except_the_auditor_and_fixtures_and_the_frozen_catalog_stay_none() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let shipped = [
        (root.join(".nanna/catalog"), 5),
        (root.join(".nanna/agents"), 4),
    ];
    for (dir, floor) in shipped {
        let cards = read_cards(&dir);
        assert!(
            cards.len() >= floor,
            "{}: only {} cards read",
            dir.display(),
            cards.len()
        );
        for (name, card) in cards {
            let reaches = !matches!(name.as_str(), "auditor" | "session-librarian");
            let want = if reaches { "root" } else { "none" };
            assert_eq!(reach_of(&card), want, "{name} in {}", dir.display());
            let tools = tools_of(&card);
            for hub_tool in HUB_TOOLS {
                let listed = tools.iter().any(|tool| tool == hub_tool);
                assert_eq!(listed, reaches, "{name}: {hub_tool}");
            }
        }
    }
    let quiet = [
        (fixtures().join("global"), 5),
        (root.join("evals/cases/auditor_heldout/catalog"), 4),
    ];
    for (dir, floor) in quiet {
        let cards = read_cards(&dir);
        assert!(
            cards.len() >= floor,
            "{}: only {} cards read",
            dir.display(),
            cards.len()
        );
        for (name, card) in cards {
            assert_eq!(reach_of(&card), "none", "{name} in {}", dir.display());
        }
    }
}

#[test]
fn every_repo_card_narrows_its_shipped_twin_in_reach_and_hub_tools() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let base = read_cards(&root.join(".nanna/catalog"));
    let local = read_cards(&root.join(".nanna/agents"));
    assert!(local.len() >= 4);
    for (name, card) in local {
        let twin = base.iter().find(|(other, _)| *other == name);
        let (_, twin) = twin.expect("every repo card has a shipped twin");
        assert_eq!(reach_of(&card), reach_of(twin), "{name}");
        let allowed = tools_of(twin);
        for tool in tools_of(&card) {
            assert!(
                allowed.contains(&tool),
                "{name}: {tool} is not in the base card"
            );
        }
    }
}
