use harness::action_auditor::MESSAGE_TOOLS;
use harness::effects::EffectClass;
use harness::hub::message::MessageReach;
use harness::identity::{librarian_identity, AgentIdentity, IdentityCatalog, IdentityError};
use harness::identity::{SystemPrompt, LIBRARIAN_CARD, LIBRARIAN_IDENTITY, REPO_AGENTS_DIR};
use std::fs;
use std::path::{Path, PathBuf};

const HUB_TOOLS: [&str; 6] = [
    "msg_send",
    "msg_recv",
    "session_set_note",
    "sessions_list",
    "session_describe",
    "sessions_tree",
];

const OTHER_TOOLS: [&str; 4] = ["run_command", "write_file", "read_file", "assign_task"];

const FIXTURE_CARD: &str = r#"
[identity]
name = "@NAME@"
description = "A fixture card with an inline prompt."
loop = "inner"
model = "gemma4:e4b"
system_prompt = { inline = "Fixture prompt." }

[scope]
repos = ["github.com/example/repo"]
paths = []
max_effect = "none"
tools = ["index_search", "index_read"]

[limits]
max_iterations = 8
max_wall_clock_secs = 60
max_concurrent = 2
"#;

const CARD_DIRS: [(&str, usize); 4] = [
    (".nanna/catalog", 5),
    (".nanna/agents", 4),
    ("evals/cases/auditor_heldout/catalog", 4),
    ("harness/tests/fixtures/identities/global", 5),
];

const LOOKALIKES: [&str; 5] = [
    "Session-Librarian",
    "SESSION-LIBRARIAN",
    "session_librarian",
    "Session_Librarian",
    "session-LIBRARIAN",
];

const INNOCENT: [&str; 4] = [
    "session-librarian-helper",
    "librarian",
    "session-librarian2",
    "my_session-librarian",
];

const TOOLS_LINE: &str = "tools = [\"index_search\", \"index_read\"]";
const EFFECT_LINE: &str = "max_effect = \"none\"";

const WIDENINGS: [(&str, &str, &str); 9] = [
    (
        TOOLS_LINE,
        "tools = [\"index_search\", \"index_read\", \"sessions_query\"]",
        "scope.tools",
    ),
    (
        TOOLS_LINE,
        "tools = [\"index_search\", \"index_read\", \"msg_send\"]",
        "scope.tools",
    ),
    (
        TOOLS_LINE,
        "tools = [\"index_search\", \"index_read\", \"write_file\"]",
        "scope.tools",
    ),
    (
        EFFECT_LINE,
        "max_effect = \"workspace\"",
        "scope.max_effect",
    ),
    ("paths = []", "paths = [\"**\"]", "scope.paths"),
    (
        "max_iterations = 8",
        "max_iterations = 9",
        "limits.max_iterations",
    ),
    (
        "max_concurrent = 2",
        "max_concurrent = 3",
        "limits.max_concurrent",
    ),
    (
        EFFECT_LINE,
        "max_effect = \"none\"\nmessage_reach = \"lineage\"",
        "scope.message_reach",
    ),
    (
        EFFECT_LINE,
        "max_effect = \"none\"\nmessage_reach = \"root\"",
        "scope.message_reach",
    ),
];

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn card() -> AgentIdentity {
    librarian_identity().expect("the built-in card parses")
}

fn raw_card() -> toml::Table {
    LIBRARIAN_CARD.parse().expect("the card is valid TOML")
}

fn variant(replace: &str, by: &str) -> AgentIdentity {
    let present = LIBRARIAN_CARD.contains(replace);
    assert!(present, "{replace} not in the card");
    let text = LIBRARIAN_CARD.replace(replace, by);
    AgentIdentity::from_toml_str(&text, "variant.toml").expect("the variant parses")
}

fn text_of(value: &toml::Value) -> String {
    value.as_str().expect("a string").to_string()
}

fn strings(value: &toml::Value) -> Vec<String> {
    let items = value.as_array().expect("an array");
    items.iter().map(text_of).collect()
}

fn put(dir: &Path, file: &str, text: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(file);
    fs::write(&path, text).unwrap();
    path
}

fn card_named(name: &str) -> String {
    FIXTURE_CARD.replace("@NAME@", name)
}

fn base_catalog() -> IdentityCatalog {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "other.toml", &card_named("other"));
    IdentityCatalog::load(dir.path()).unwrap()
}

fn kind(err: &IdentityError) -> String {
    let debug = format!("{err:?}");
    debug.split(' ').next().unwrap().to_string()
}

fn rejected_as(err: &IdentityError, expected: &str, path: &Path) {
    assert_eq!(kind(err), expected, "{err:?}");
    let text = err.to_string();
    assert!(text.starts_with(&path.display().to_string()), "{text}");
    assert!(text.contains(LIBRARIAN_IDENTITY), "{text}");
}

fn global_error(file: &str, text: &str) -> (IdentityError, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = put(dir.path(), file, text);
    (IdentityCatalog::load(dir.path()).unwrap_err(), path)
}

fn repo_error(base: IdentityCatalog, file: &str, text: &str) -> (IdentityError, PathBuf) {
    let repo = tempfile::tempdir().unwrap();
    let path = put(&repo.path().join(REPO_AGENTS_DIR), file, text);
    (base.with_repo_overrides(repo.path()).unwrap_err(), path)
}

#[test]
fn the_card_has_exactly_the_least_privilege_shape_in_raw_toml() {
    let raw = raw_card();
    assert_eq!(raw["identity"]["name"].as_str(), Some(LIBRARIAN_IDENTITY));
    let scope = &raw["scope"];
    assert_eq!(scope["max_effect"].as_str(), Some("none"));
    assert_eq!(strings(&scope["tools"]), vec!["index_search", "index_read"]);
    assert!(strings(&scope["paths"]).is_empty());
    assert_eq!(strings(&scope["repos"]), vec!["builtin"]);
    assert!(scope.get("read_paths").is_none());
    let reach = scope.get("message_reach");
    assert!(reach.is_none_or(|value| value.as_str() == Some("none")));
    assert!(raw["identity"]["system_prompt"]["inline"].is_str());
    let limits = &raw["limits"];
    assert_eq!(limits["max_iterations"].as_integer(), Some(8));
    assert_eq!(limits["max_wall_clock_secs"].as_integer(), Some(60));
    assert_eq!(limits["max_concurrent"].as_integer(), Some(2));
}

#[test]
fn the_typed_card_agrees_with_the_raw_toml() {
    let raw = raw_card();
    let typed = card();
    assert_eq!(typed.name(), LIBRARIAN_IDENTITY);
    assert_eq!(typed.scope.max_effect, EffectClass::None);
    let names: Vec<String> = typed.scope.tools.iter().map(|t| t.to_string()).collect();
    assert_eq!(names, strings(&raw["scope"]["tools"]));
    assert_eq!(typed.scope.paths, strings(&raw["scope"]["paths"]));
    assert_eq!(typed.scope.read_paths, None);
    assert_eq!(typed.scope.repos, vec!["builtin"]);
    assert_eq!(typed.limits.max_iterations, 8);
    assert_eq!(typed.limits.max_wall_clock_secs, 60);
    assert_eq!(typed.limits.max_concurrent, 2);
}

#[test]
fn g2_the_card_can_never_message_and_can_never_write() {
    let typed = card();
    assert_eq!(typed.scope.message_reach, MessageReach::None);
    assert!(typed.scope.paths.is_empty());
    assert!(typed.scope.read_paths.is_none());
    assert_eq!(typed.scope.max_effect, EffectClass::None);
    assert!(typed.restricts_paths());
}

#[test]
fn g2_the_card_names_no_hub_tool_and_no_pattern() {
    let typed = card();
    for tool in HUB_TOOLS.iter().chain(&MESSAGE_TOOLS).chain(&OTHER_TOOLS) {
        assert!(!typed.allows_tool(tool), "{tool} is allowed");
    }
    assert!(typed.allows_tool("index_search"));
    assert!(typed.allows_tool("index_read"));
    for pattern in &typed.scope.tools {
        let text = pattern.to_string();
        assert!(!text.contains('*'), "{text} is a glob");
        assert!(!text.starts_with("msg_"), "{text}");
        assert!(!text.starts_with("session"), "{text}");
    }
    assert_eq!(typed.scope.tools.len(), 2);
}

#[test]
fn the_prompt_is_inline_and_needs_no_file() {
    let typed = card();
    let inline = matches!(typed.identity.system_prompt, SystemPrompt::Inline(_));
    assert!(inline);
    let text = typed.system_prompt_text().expect("the prompt reads");
    assert!(text.contains("index_search"));
    assert!(text.contains("JSON"));
    assert!(!typed.source().exists());
}

#[test]
fn the_card_narrows_itself() {
    assert!(card().narrows(&card()).is_ok());
}

#[test]
fn every_widening_of_the_card_fails_narrows_with_the_widened_field() {
    let base = card();
    for (replace, by, field) in WIDENINGS {
        let widened = variant(replace, by);
        let err = widened.narrows(&base).expect_err("a widening fails");
        assert_eq!(kind(&err), "WidensScope", "{err:?}");
        assert!(err.to_string().contains(&format!("`{field}`")), "{err}");
    }
}

#[test]
fn a_global_file_named_session_librarian_is_rejected() {
    let (err, path) = global_error("session-librarian.toml", &card_named("other"));
    rejected_as(&err, "ReservedIdentity", &path);
}

#[test]
fn a_global_file_named_session_librarian_is_rejected_even_when_malformed() {
    let (err, path) = global_error("session-librarian.toml", "not toml at all [");
    rejected_as(&err, "ReservedIdentity", &path);
}

#[test]
fn a_global_card_declaring_the_name_in_another_file_is_rejected() {
    let (err, path) = global_error("innocent.toml", &card_named(LIBRARIAN_IDENTITY));
    rejected_as(&err, "ReservedIdentity", &path);
}

#[test]
fn a_repo_file_named_session_librarian_is_protected_even_when_it_narrows() {
    let base = base_catalog().with_builtins().unwrap();
    let narrower = LIBRARIAN_CARD.replace("max_iterations = 8", "max_iterations = 4");
    let (err, path) = repo_error(base, "session-librarian.toml", &narrower);
    rejected_as(&err, "ProtectedIdentity", &path);
}

#[test]
fn a_repo_file_named_session_librarian_is_protected_not_a_missing_base() {
    let text = card_named("nobody");
    let (err, path) = repo_error(base_catalog(), "session-librarian.toml", &text);
    rejected_as(&err, "ProtectedIdentity", &path);
}

#[test]
fn a_repo_card_declaring_the_name_in_another_file_is_protected() {
    let base = base_catalog().with_builtins().unwrap();
    let (err, path) = repo_error(base, "innocent.toml", &card_named(LIBRARIAN_IDENTITY));
    rejected_as(&err, "ProtectedIdentity", &path);
}

#[test]
fn the_rejection_text_carries_the_name_and_no_card_text() {
    let (err, _) = global_error("session-librarian.toml", LIBRARIAN_CARD);
    let text = err.to_string();
    assert!(text.contains(LIBRARIAN_IDENTITY));
    assert!(text.contains("built in"));
    let prompt = card().system_prompt_text().unwrap();
    assert!(!text.contains(prompt.lines().next().unwrap()));
    assert!(!text.contains("index_search"));
}

#[test]
fn load_never_injects_the_card() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), "other.toml", &card_named("other"));
    let catalog = IdentityCatalog::load(dir.path()).unwrap();
    assert_eq!(catalog.names().collect::<Vec<_>>(), vec!["other"]);
    assert!(catalog.get(LIBRARIAN_IDENTITY).is_none());
    let empty = tempfile::tempdir().unwrap();
    assert!(IdentityCatalog::load(empty.path()).unwrap().is_empty());
}

#[test]
fn the_fixture_catalog_loads_without_the_card() {
    let fixtures = manifest().join("tests/fixtures/identities/global");
    let catalog = IdentityCatalog::load(fixtures).unwrap();
    assert!(catalog.len() >= 5);
    assert!(catalog.get(LIBRARIAN_IDENTITY).is_none());
}

#[test]
fn no_shipped_or_frozen_card_file_declares_the_name() {
    let root = manifest().join("..");
    for (relative, floor) in CARD_DIRS {
        let dir = root.join(relative);
        let mut seen = 0;
        for entry in fs::read_dir(&dir).expect("directory reads") {
            let path = entry.expect("entry reads").path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                let text = fs::read_to_string(&path).expect("card reads");
                let parsed: toml::Table = text.parse().expect("card parses");
                let name = parsed["identity"]["name"].as_str();
                assert_ne!(name, Some(LIBRARIAN_IDENTITY), "{}", path.display());
                assert_ne!(path.file_stem().unwrap(), LIBRARIAN_IDENTITY);
                seen += 1;
            }
        }
        assert!(seen >= floor, "{}: only {seen} cards read", dir.display());
    }
}

#[test]
fn with_builtins_adds_exactly_the_card() {
    let base = base_catalog();
    let before = base.len();
    let with = base.with_builtins().unwrap();
    assert_eq!(with.len(), before + 1);
    assert_eq!(with.get(LIBRARIAN_IDENTITY), Some(&card()));
    assert!(with.get("other").is_some());
}

#[test]
fn with_builtins_on_an_empty_catalog_holds_only_the_card() {
    let with = IdentityCatalog::default().with_builtins().unwrap();
    assert_eq!(with.names().collect::<Vec<_>>(), vec![LIBRARIAN_IDENTITY]);
}

#[test]
fn with_builtins_twice_is_a_duplicate_error() {
    let once = IdentityCatalog::default().with_builtins().unwrap();
    let err = once.with_builtins().unwrap_err();
    assert_eq!(kind(&err), "DuplicateName", "{err:?}");
}

fn identity_sources() -> Vec<(PathBuf, String)> {
    let dir = manifest().join("src/identity");
    let mut sources = Vec::new();
    for entry in fs::read_dir(dir).expect("directory reads") {
        let path = entry.expect("entry reads").path();
        let text = fs::read_to_string(&path).expect("source reads");
        sources.push((path, text));
    }
    sources
}

#[test]
fn every_lookalike_file_stem_is_rejected_in_both_dirs() {
    for name in LOOKALIKES {
        let file = format!("{name}.toml");
        let (err, path) = global_error(&file, &card_named("other"));
        rejected_as(&err, "ReservedIdentity", &path);
        let base = base_catalog().with_builtins().unwrap();
        let (err, path) = repo_error(base, &file, &card_named("other"));
        rejected_as(&err, "ProtectedIdentity", &path);
    }
}

#[test]
fn every_lookalike_declared_name_is_rejected_in_both_dirs() {
    for name in LOOKALIKES {
        let text = card_named(name);
        let (err, path) = global_error("innocent.toml", &text);
        rejected_as(&err, "ReservedIdentity", &path);
        assert!(!err.to_string().contains(name), "{err}");
        let base = base_catalog().with_builtins().unwrap();
        let (err, path) = repo_error(base, "innocent.toml", &text);
        rejected_as(&err, "ProtectedIdentity", &path);
        assert!(!err.to_string().contains(name), "{err}");
    }
}

#[test]
fn innocent_names_are_still_accepted_as_files_and_as_declared_names() {
    for name in INNOCENT {
        let dir = tempfile::tempdir().unwrap();
        let file = format!("{name}.toml");
        let declared = card_named(&format!("x-{name}"));
        put(dir.path(), &file, &card_named(name));
        put(dir.path(), "declared.toml", &declared);
        let catalog = IdentityCatalog::load(dir.path()).unwrap();
        assert!(catalog.get(name).is_some(), "{name}");
        let repo = tempfile::tempdir().unwrap();
        let agents = repo.path().join(REPO_AGENTS_DIR);
        put(&agents, &file, &card_named(name));
        let layered = catalog.with_repo_overrides(repo.path());
        assert!(layered.is_ok(), "{name}");
    }
}

#[test]
fn the_reserved_name_comparison_exists_once_in_the_identity_sources() {
    let mut folding = 0;
    for (path, text) in identity_sources() {
        assert!(
            !text.contains("== LIBRARIAN_IDENTITY"),
            "{}",
            path.display()
        );
        assert!(
            !text.contains("== \"session-librarian\""),
            "{}",
            path.display()
        );
        assert!(
            !text.contains("!= LIBRARIAN_IDENTITY"),
            "{}",
            path.display()
        );
        folding += text
            .matches("eq_ignore_ascii_case(LIBRARIAN_IDENTITY)")
            .count();
    }
    assert_eq!(folding, 1);
}
