use harness::capabilities::detect_capabilities;
use harness::onboarding::detect::scan_project;
use harness::onboarding::flake_template::generate_flake;
use harness::onboarding::fullstack::DatabaseUsage;
use harness::onboarding::profile::BuildSystem;
use std::fs;
use std::path::PathBuf;

const EXPECTED_MEMBERS: [&str; 3] = ["api", "shared", "ui"];
const EXPECTED_CHECKS: [&str; 3] = ["/", "/health/v1", "/api/v1/greeting"];
const EXPECTED_STACK_DEPS: [&str; 4] = ["actix-web", "dioxus", "sqlx", "shared"];

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/fullstack")
}

#[test]
fn fixture_is_excluded_from_root_workspace() {
    let root_manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("Cargo.toml");
    let doc: toml::Value = fs::read_to_string(root_manifest).unwrap().parse().unwrap();
    let excluded: Vec<&str> = doc["workspace"]["exclude"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(excluded.contains(&"tests/fixtures/fullstack"));
}

#[test]
fn fixture_is_detected_as_cargo_workspace_with_expected_members() {
    let signals = scan_project(&fixture_root()).unwrap();
    assert!(!signals.has_flake_nix);
    assert!(!signals.has_build_file);
    assert!(!signals.has_makefile);

    let manifest = signals.cargo_toml.as_ref().unwrap();
    assert!(manifest.is_workspace);
    assert_eq!(manifest.name, "fullstack");
    assert_eq!(manifest.edition.as_deref(), Some("2021"));

    let mut members = manifest.members.clone();
    members.sort();
    assert_eq!(members, EXPECTED_MEMBERS);

    for dep in EXPECTED_STACK_DEPS {
        assert!(
            manifest.dependencies.iter().any(|d| d == dep),
            "fixture should depend on {dep}, got {:?}",
            manifest.dependencies
        );
    }
}

#[test]
fn fixture_members_are_crates_with_sources() {
    let root = fixture_root();
    for member in EXPECTED_MEMBERS {
        let crate_dir = root.join(member);
        assert!(
            crate_dir.join("Cargo.toml").is_file(),
            "{member}/Cargo.toml"
        );
        assert!(crate_dir.join("src").is_dir(), "{member}/src");
    }
    assert!(root.join("api/src/main.rs").is_file());
    assert!(root.join("ui/src/main.rs").is_file());
    assert!(root.join("shared/src/lib.rs").is_file());
    assert!(root.join("ui/Trunk.toml").is_file());
    assert!(root.join("ui/index.html").is_file());
    assert!(root.join("Containerfile").is_file());
    assert!(root.join("README.md").is_file());
}

#[test]
fn fixture_profile_uses_cargo_build_system() {
    let signals = scan_project(&fixture_root()).unwrap();
    let profile = signals.to_cargo_profile().unwrap();
    assert_eq!(profile.project_name, "fullstack");
    assert_eq!(profile.build_system, BuildSystem::Cargo);
    assert!(profile.tools.iter().any(|t| t.name == "build"));
    assert!(profile.tools.iter().any(|t| t.name == "test"));
}

#[test]
fn fixture_is_detected_as_full_stack_rust() {
    let signals = scan_project(&fixture_root()).unwrap();
    let profile = signals
        .full_stack
        .expect("fixture matches the full-stack profile");
    assert_eq!(profile.api.name, "api");
    assert_eq!(profile.frontend.name, "ui");
    assert_eq!(profile.shared.len(), 1);
    assert_eq!(profile.shared[0].name, "shared");
    assert_eq!(
        profile.database,
        Some(DatabaseUsage {
            sqlx_postgres: true,
            migrations_dir: Some(PathBuf::from("migrations")),
        })
    );
    assert_eq!(profile.health_path(), "/health/v1");
    assert_eq!(profile.proxy_backends.len(), 2);
}

#[test]
fn plain_library_crate_is_not_full_stack_rust() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"plain\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    let signals = scan_project(dir.path()).unwrap();
    assert!(signals.full_stack.is_none());
}

const EXPECTED_FLAKE_TOOLCHAIN: &str = r#"        rustToolchain = pkgs.rust-bin.stable."1.84.0".default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];
          targets = [ "wasm32-unknown-unknown" ];
        };
"#;

const EXPECTED_FLAKE_PACKAGES: &str = r#"        devContainerPackages = [
          rustToolchain
          pkgs.cargo-nextest
          pkgs.bash
          pkgs.coreutils
          pkgs.git
          pkgs.cacert
          pkgs.trunk
          pkgs.wasm-bindgen-cli
          pkgs.sqlx-cli
          pkgs.postgresql
        ];
"#;

#[test]
fn fixture_flake_provisions_full_stack_toolchain_and_packages() {
    let profile = scan_project(&fixture_root())
        .unwrap()
        .to_cargo_profile()
        .unwrap();
    let flake = generate_flake(&profile).unwrap();
    assert!(flake.contains(EXPECTED_FLAKE_TOOLCHAIN), "{flake}");
    assert!(flake.contains(EXPECTED_FLAKE_PACKAGES), "{flake}");
    assert!(flake.contains(r#"name = "fullstack-dev";"#));
}

#[test]
fn fixture_exposes_trunk_build_and_sqlx_migrate_capabilities() {
    let ids: Vec<&str> = detect_capabilities(&fixture_root())
        .iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids, vec!["trunk_build", "sqlx_migrate"]);
}

#[test]
fn fixture_ships_exactly_one_sqlx_migration() {
    let migrations: Vec<PathBuf> = fs::read_dir(fixture_root().join("migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    assert_eq!(migrations.len(), 1, "{migrations:?}");
    let sql = fs::read_to_string(&migrations[0]).unwrap();
    assert!(sql.contains("CREATE TABLE"));
}

#[test]
fn checks_manifest_lists_every_public_endpoint() {
    let checks = fs::read_to_string(fixture_root().join("CHECKS")).unwrap();
    let paths: Vec<&str> = checks.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(paths, EXPECTED_CHECKS);
    for path in &paths {
        assert!(path.starts_with('/'), "{path} must be an absolute path");
    }
}

#[test]
fn deploy_template_describes_fake_container_registry_target() {
    let template = fs::read_to_string(fixture_root().join(".nanna/deploy.toml")).unwrap();
    let doc: toml::Value = template.parse().unwrap();

    assert_eq!(
        doc["target"]["kind"].as_str(),
        Some("container-registry+serverless")
    );
    let registry = doc["target"]["registry"].as_str().unwrap();
    assert!(
        registry.contains(".invalid"),
        "registry must be a reserved placeholder, got {registry}"
    );
    let environments: Vec<&str> = doc["target"]["environments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(environments.contains(&"production"));
    assert!(doc["rollout"]["windows"].as_str().is_some());

    let checks = fs::read_to_string(fixture_root().join("CHECKS")).unwrap();
    for endpoint in doc["health"]["endpoints"].as_array().unwrap() {
        let endpoint = endpoint.as_str().unwrap();
        assert!(
            checks.lines().any(|l| l == endpoint),
            "health endpoint {endpoint} must appear in CHECKS"
        );
    }

    let steps: Vec<i64> = doc["rollout"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_integer().unwrap())
        .collect();
    assert!(steps.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(steps.last(), Some(&100));
}

#[test]
fn readme_documents_every_env_hook() {
    let readme = fs::read_to_string(fixture_root().join("README.md")).unwrap();
    for hook in ["FIXTURE_BREAK_ROUTE", "FIXTURE_FAIL_TEST", "DATABASE_URL"] {
        assert!(readme.contains(hook), "README must document {hook}");
    }
}
