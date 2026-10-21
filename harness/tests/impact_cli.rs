use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/fullstack")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "target" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn committed_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixture_root(), dir.path());
    let repo = git2::Repository::init(dir.path()).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("t", "t@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .unwrap();
    dir
}

fn nanna(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args(args)
        .output()
        .unwrap()
}

fn touch_greeting_handler(root: &Path) {
    let lib = root.join("api/src/lib.rs");
    let edited = std::fs::read_to_string(&lib).unwrap().replace(
        "HttpResponse::Ok().json(Greeting::default())",
        "HttpResponse::Ok().json(Greeting::default()) // changed",
    );
    std::fs::write(lib, edited).unwrap();
}

#[test]
fn impact_prints_the_blast_radius_of_a_diff() {
    let dir = committed_fixture();
    touch_greeting_handler(dir.path());
    let out = nanna(&[
        "impact",
        "--diff",
        "HEAD",
        "--repo-path",
        dir.path().to_str().unwrap(),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("score: "));
    assert!(text.contains("touched: "));
    assert!(text.contains("http.GET /api/v1/greeting"));
    assert!(text.contains("content (3)"));
    assert!(text.contains("ui.greeting_page"));
}

#[test]
fn impact_json_matches_the_blast_radius_shape() {
    let dir = committed_fixture();
    touch_greeting_handler(dir.path());
    let out = nanna(&[
        "impact",
        "--diff",
        "HEAD",
        "--repo-path",
        dir.path().to_str().unwrap(),
        "--json",
    ]);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json["score"].as_u64().unwrap() > 0);
    assert!(json["touched"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "http.GET /api/v1/greeting"));
    assert!(json["concerns"]["availability"].is_number());
    assert!(json["evidence"].is_array());
}

#[test]
fn impact_of_a_clean_tree_is_empty() {
    let dir = committed_fixture();
    let out = nanna(&[
        "impact",
        "--diff",
        "HEAD",
        "--repo-path",
        dir.path().to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("score: 0\n"));
    assert!(text.contains("touched: (none)"));
}

#[test]
fn impact_reports_unknown_revisions_and_missing_manifests() {
    let dir = committed_fixture();
    let bad_rev = nanna(&[
        "impact",
        "--diff",
        "no-such-rev",
        "--repo-path",
        dir.path().to_str().unwrap(),
    ]);
    assert!(!bad_rev.status.success());
    assert!(String::from_utf8_lossy(&bad_rev.stderr).starts_with("error: "));

    let bare = tempfile::tempdir().unwrap();
    git2::Repository::init(bare.path()).unwrap();
    let no_manifest = nanna(&[
        "impact",
        "--diff",
        "HEAD",
        "--repo-path",
        bare.path().to_str().unwrap(),
    ]);
    assert!(!no_manifest.status.success());
    assert!(String::from_utf8_lossy(&no_manifest.stderr).contains("effects.toml"));
}

#[test]
fn impact_scores_a_new_migration_that_was_never_staged() {
    let dir = committed_fixture();
    std::fs::write(
        dir.path().join("migrations/20260925000000_note.sql"),
        "ALTER TABLE greetings ADD COLUMN note TEXT;\n",
    )
    .unwrap();
    let out = nanna(&[
        "impact",
        "--diff",
        "HEAD",
        "--repo-path",
        dir.path().to_str().unwrap(),
        "--json",
    ]);
    assert!(out.status.success());
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["touched"], serde_json::json!(["db.greetings"]));
    assert!(json["downstream"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "http.GET /api/v1/greeting"));
    assert!(json["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["extractor"] == "sql" && e["detail"] == "ALTER greetings"));
}

#[test]
fn impact_defaults_to_the_repository_containing_the_current_directory() {
    let dir = committed_fixture();
    touch_greeting_handler(dir.path());
    let out = Command::new(env!("CARGO_BIN_EXE_nanna"))
        .args(["impact", "--diff", "HEAD", "--json"])
        .current_dir(dir.path().join("api/src"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(json["touched"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a == "http.GET /api/v1/greeting"));
}

#[test]
fn impact_rejects_a_malformed_deploy_template_instead_of_ignoring_it() {
    let dir = committed_fixture();
    std::fs::write(dir.path().join(".nanna/deploy.toml"), "[target\n").unwrap();
    let out = nanna(&[
        "impact",
        "--diff",
        "HEAD",
        "--repo-path",
        dir.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("deploy.toml"), "{stderr}");
}

#[test]
fn impact_rejects_a_malformed_manifest() {
    let dir = committed_fixture();
    std::fs::write(dir.path().join(".nanna/effects.toml"), "[asset\n").unwrap();
    let out = nanna(&[
        "impact",
        "--diff",
        "HEAD",
        "--repo-path",
        dir.path().to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("effects.toml"));
}
