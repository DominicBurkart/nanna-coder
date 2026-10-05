use harness::assets::AssetGraph;
use harness::impact::{Access, Action, Change, ChangedFile, ImpactAnalyzer};
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/fullstack")
}

fn graph() -> AssetGraph {
    AssetGraph::load_from_repo(&fixture_root()).unwrap()
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(fixture_root().join(rel)).unwrap()
}

fn line_containing(source: &str, needle: &str) -> u32 {
    source
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} not in source")) as u32
        + 1
}

fn edit_line(path: &str, needle: &str) -> Change {
    let source = read(path);
    let number = line_containing(&source, needle);
    let mut file = ChangedFile::new(path);
    file.added.push(harness::impact::AddedLine {
        number,
        text: source.lines().nth(number as usize - 1).unwrap().to_string(),
    });
    file.removed.push("old line".to_string());
    file.content = Some(source);
    Change {
        files: vec![file],
        actions: vec![],
    }
}

#[test]
fn greeting_handler_diff_yields_endpoint_and_its_concerns() {
    let graph = graph();
    let change = edit_line(
        "api/src/lib.rs",
        "HttpResponse::Ok().json(Greeting::default())",
    );
    let radius = ImpactAnalyzer::new(&graph).analyze(&change);
    assert!(radius
        .touched
        .contains(&"http.GET /api/v1/greeting".to_string()));
    assert!(radius.downstream.contains(&"ui.greeting_page".to_string()));
    assert_eq!(radius.concerns["availability"], 6);
    assert_eq!(radius.concerns["content"], 3);
    assert!(radius.score > 0);
    assert!(radius.evidence.iter().any(|e| e.extractor == "routes"
        && e.asset == "http.GET /api/v1/greeting"
        && e.detail.contains("greeting")));
}

#[test]
fn handler_edit_is_attributed_to_its_own_endpoint_by_the_route_extractor() {
    let graph = graph();
    let change = edit_line(
        "api/src/lib.rs",
        "HttpResponse::Ok().json(Greeting::default())",
    );
    let radius = ImpactAnalyzer::new(&graph).analyze(&change);
    let routed: Vec<&str> = radius
        .evidence
        .iter()
        .filter(|e| e.extractor == "routes")
        .map(|e| e.asset.as_str())
        .collect();
    assert_eq!(routed, ["http.GET /api/v1/greeting"]);
}

#[test]
fn migration_diff_yields_table_plus_downstream_endpoint() {
    let graph = graph();
    let rel = "migrations/20260924000000_create_greetings.sql";
    let change = Change {
        files: vec![ChangedFile::whole(rel, read(rel))],
        actions: vec![],
    };
    let radius = ImpactAnalyzer::new(&graph).analyze(&change);
    assert_eq!(radius.touched, ["db.greetings"]);
    assert_eq!(
        radius.downstream,
        ["http.GET /api/v1/greeting", "ui.greeting_page"]
    );
    assert!(radius
        .evidence
        .iter()
        .any(|e| e.extractor == "sql" && e.access == Access::Write));
    assert_eq!(radius.concerns["content"], 3);
}

#[test]
fn migration_sql_extractor_finds_create_and_insert() {
    let sql = read("migrations/20260924000000_create_greetings.sql");
    let found = harness::impact::sql_accesses(&sql);
    assert!(found
        .iter()
        .all(|a| a.table == "greetings" && a.access == Access::Write));
    assert_eq!(found.len(), 2);
}

#[test]
fn query_with_subselect_reads_inner_table_and_writes_outer() {
    let graph = graph();
    let source = "pub async fn bump(pool: &PgPool) {\n    sqlx::query!(\n        r#\"UPDATE greetings SET message = (SELECT message FROM greetings WHERE id = 1)\"#\n    );\n}\n";
    let change = Change {
        files: vec![ChangedFile::whole("api/src/store.rs", source)],
        actions: vec![],
    };
    let radius = ImpactAnalyzer::new(&graph).analyze(&change);
    let sql: Vec<_> = radius
        .evidence
        .iter()
        .filter(|e| e.extractor == "sql")
        .collect();
    assert!(sql
        .iter()
        .any(|e| e.access == Access::Write && e.asset == "db.greetings"));
    assert!(sql
        .iter()
        .any(|e| e.access == Access::Read && e.asset == "db.greetings"));
    assert!(radius.touched.contains(&"db.greetings".to_string()));
}

#[test]
fn actions_follow_the_deploy_template_environments() {
    let graph = graph();
    let template = harness::deploy::DeployTemplate::load_from_repo(&fixture_root()).unwrap();
    let analyzer =
        ImpactAnalyzer::new(&graph).with_environments(template.target.environments.clone());
    let deploy = Change::from_actions([Action::SandboxDeploy {
        environment: "sandbox".into(),
    }]);
    let radius = analyzer.analyze(&deploy);
    assert!(radius.touched.contains(&"http.GET /health/v1".to_string()));
    assert!(radius.touched.contains(&"ui.greeting_page".to_string()));
    assert!(analyzer
        .analyze(&Change::from_actions([Action::CiTrigger]))
        .is_empty());
}
