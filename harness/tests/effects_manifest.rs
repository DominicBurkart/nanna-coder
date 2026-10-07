use glob::Pattern;
use harness::assets::{manifest_path_in, AssetGraph, MANIFEST_REL_PATH, PROTECTED_CONFIG_GLOB};
use std::path::PathBuf;
use std::process::Command;

mod orders_module {
    nanna_effects::touches!("db.greetings");
}

mod greeting_module {
    nanna_effects::touches!("http.GET /api/v1/greeting");
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/fullstack")
}

fn nanna(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn fixture_ships_a_manifest_that_loads_without_warnings() {
    let root = fixture_root();
    let graph = AssetGraph::load_from_repo(&root).unwrap();
    assert!(graph.assets().count() >= 3);
    assert!(graph.warnings(Some(&root)).is_empty());
}

#[test]
fn fixture_blast_radius_follows_reads() {
    let graph = AssetGraph::load_from_repo(&fixture_root()).unwrap();
    let dependents = graph.dependents_of("db.greetings");
    assert!(dependents.contains("http.GET /api/v1/greeting"));
    assert!(dependents.contains("ui.greeting_page"));
    assert!(!dependents.contains("http.GET /health/v1"));
    let concerns = graph.concerns_of(dependents.iter().chain(["db.greetings".to_string()].iter()));
    assert_eq!(concerns[0].name, "availability");
}

#[test]
fn graph_cli_renders_text_for_the_fixture() {
    let root = fixture_root();
    let out = nanna(&["effects", "graph", "--repo-path", root.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("db.greetings [table]"));
    assert!(text.contains("read by: http.GET /api/v1/greeting"));
}

#[test]
fn graph_cli_renders_mermaid_for_the_fixture() {
    let root = fixture_root();
    let out = nanna(&[
        "effects",
        "graph",
        "--repo-path",
        root.to_str().unwrap(),
        "--format",
        "mermaid",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("graph LR\n"));
    assert!(text.contains("-->"));
    assert!(text.contains("db.greetings (table)"));
}

#[test]
fn graph_cli_fails_clearly_without_a_manifest() {
    let repo = tempfile::tempdir().unwrap();
    let out = nanna(&[
        "effects",
        "graph",
        "--repo-path",
        repo.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("effects.toml"));
}

#[test]
fn graph_cli_reports_field_errors_with_the_file() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join(".nanna")).unwrap();
    std::fs::write(
        manifest_path_in(repo.path()),
        "[asset.a]\nkind = \"table\"\nconcerns = [\"ghost\"]\n",
    )
    .unwrap();
    let out = nanna(&[
        "effects",
        "graph",
        "--repo-path",
        repo.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("effects.toml"));
    assert!(stderr.contains("asset.\"a\".concerns"));
}

#[test]
fn propose_cli_writes_a_proposal_and_never_adopts_it() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(repo.path().join("migrations")).unwrap();
    std::fs::write(
        repo.path().join("migrations/1.sql"),
        "CREATE TABLE t (id int);",
    )
    .unwrap();
    let out = nanna(&[
        "effects",
        "propose",
        "--repo-path",
        repo.path().to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(repo.path().join(".nanna/effects.proposed.toml").exists());
    assert!(!manifest_path_in(repo.path()).exists());
    let graph_out = nanna(&[
        "effects",
        "graph",
        "--repo-path",
        repo.path().to_str().unwrap(),
    ]);
    assert!(!graph_out.status.success());
}

#[test]
fn touches_declarations_round_trip_into_the_graph() {
    let mut graph = AssetGraph::load_from_repo(&fixture_root()).unwrap();
    graph.merge_declarations(nanna_effects::declared()).unwrap();

    let here = file!();
    let sites: Vec<_> = graph.sites_of("db.greetings").collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].file, here);
    assert_eq!(
        sites[0].module_path,
        concat!(module_path!(), "::orders_module")
    );
    assert!(sites[0].line > 0);

    let owners: Vec<_> = graph
        .owners_matching(here)
        .iter()
        .map(|a| a.name.clone())
        .collect();
    assert!(owners.contains(&"db.greetings".to_string()));
    assert!(owners.contains(&"http.GET /api/v1/greeting".to_string()));
}

#[test]
fn touches_of_an_undeclared_asset_is_a_validation_error() {
    let mut graph = AssetGraph::parse("[asset.\"db.other\"]\nkind = \"table\"\n").unwrap();
    let err = graph
        .merge_declarations(nanna_effects::declared())
        .unwrap_err();
    assert!(err.to_string().contains("effects_manifest.rs"));
}

#[test]
fn manifest_path_is_covered_by_the_protected_config_glob() {
    let glob = Pattern::new(PROTECTED_CONFIG_GLOB).unwrap();
    assert!(glob.matches(MANIFEST_REL_PATH));
    let repo = PathBuf::from("/repo");
    assert_eq!(
        manifest_path_in(&repo)
            .strip_prefix(&repo)
            .unwrap()
            .to_str()
            .unwrap(),
        MANIFEST_REL_PATH
    );
}
