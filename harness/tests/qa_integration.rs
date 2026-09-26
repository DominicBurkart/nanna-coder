//! Container integration tests for `qa_endpoints` and `qa_browser` on the
//! full-stack fixture. All tests are `#[ignore]` and skip when no runtime is
//! available; run them with
//! `cargo test -p harness --test qa_integration -- --ignored --test-threads=1`.
//!
//! The dev image is the [`apprun_integration`] stand-in plus headless
//! Chromium (the package the generated flake adds for this profile).
//! `qa_browser`'s console-error evidence comes from Chromium's own
//! automatic `GET /favicon.ico` request, which the fixture's static file
//! service 404s (no file exists under `ui/dist`); no fixture change was
//! needed to produce it.

use harness::apprun::{Limits, APP_START_TOOL};
use harness::container::{detect_runtime, ContainerRuntime};
use harness::qa::{QA_BROWSER_TOOL, QA_ENDPOINTS_TOOL};
use harness::tools::ToolRegistry;
use harness::workspace::TaskWorkspace;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const DEV_IMAGE_TAG: &str = "localhost/nanna-qa-test-dev:latest";
const TRUNK_URL: &str =
    "https://github.com/trunk-rs/trunk/releases/download/v0.21.14/trunk-x86_64-unknown-linux-gnu.tar.gz";
const COLD_BUILD_LIMIT: Limits = Limits {
    max_wall_clock_secs: 1800,
};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("tests/fixtures/fullstack")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git must be installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@test.invalid"]);
    git(dir, &["config", "user.name", "test"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if name == "target" || name == "dist" || name == ".git" {
            continue;
        }
        let target = dst.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn build_dev_image(runtime: &ContainerRuntime) {
    let dir = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), &dir.path().join("fixture"));
    let containerfile = format!(
        "FROM docker.io/library/rust:1-bookworm\n\
         RUN rustup target add wasm32-unknown-unknown \\\n\
         \x20&& curl -fsSL {TRUNK_URL} | tar -xz -C /usr/local/bin trunk \\\n\
         \x20&& mkdir -p /home/dev /cache \\\n\
         \x20&& apt-get update \\\n\
         \x20&& apt-get install -y --no-install-recommends chromium \\\n\
         \x20&& rm -rf /var/lib/apt/lists/*\n\
         ENV HOME=/home/dev CARGO_TARGET_DIR=/cache/target\n\
         COPY fixture /src\n\
         RUN cd /src/ui && trunk build && cd /src && cargo build --package api \\\n\
         \x20&& rm -rf /src && chmod -R a+w /cache /home/dev /usr/local/cargo\n\
         CMD [\"sleep\", \"infinity\"]\n"
    );
    std::fs::write(dir.path().join("Containerfile"), containerfile).unwrap();
    let out = Command::new(runtime.command())
        .args(["build", "-q", "-t", DEV_IMAGE_TAG])
        .arg(dir.path())
        .output()
        .expect("runtime must be runnable");
    assert!(
        out.status.success(),
        "image build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

async fn tool(registry: &ToolRegistry, name: &str, args: Value) -> Value {
    registry
        .execute(name, args)
        .await
        .unwrap_or_else(|e| panic!("{name} failed: {e}"))
}

async fn start_workspace(
    source: &Path,
    task_prefix: &str,
    env: Vec<(String, String)>,
) -> TaskWorkspace {
    let task_id = format!("{task_prefix}-{}", uuid::Uuid::new_v4());
    let mut ws = TaskWorkspace::create_with_container(source, &task_id, "HEAD", DEV_IMAGE_TAG)
        .await
        .expect("dev container must start");
    ws.set_app_limits(Some(COLD_BUILD_LIMIT));
    if !env.is_empty() {
        ws.set_app_env(env);
    }
    ws
}

#[tokio::test]
#[ignore]
async fn qa_tools_reject_calls_before_app_start_then_check_and_mount_the_fixture() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    build_dev_image(&runtime);

    let source = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), source.path());
    init_repo(source.path());
    let mut ws = start_workspace(source.path(), "qa-precheck", vec![]).await;
    let registry = ws.build_tool_registry();

    for (name, args) in [
        (QA_ENDPOINTS_TOOL, json!({})),
        (
            QA_BROWSER_TOOL,
            json!({ "scenario": { "steps": [{ "step": "goto", "path": "/" }] } }),
        ),
    ] {
        let err = registry.execute(name, args).await.unwrap_err();
        assert!(
            err.to_string().contains("no application is running"),
            "{name}: {err}"
        );
    }

    tool(&registry, APP_START_TOOL, Value::Null).await;

    let endpoints = tool(&registry, QA_ENDPOINTS_TOOL, json!({})).await;
    assert_eq!(endpoints["manifest"], "CHECKS", "{endpoints}");
    assert_eq!(endpoints["all_passed"], true, "{endpoints}");
    assert_eq!(endpoints["passed"], 3, "{endpoints}");
    let paths: Vec<&str> = endpoints["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["/", "/health/v1", "/api/v1/greeting"]);
    assert!(
        Path::new(endpoints["artifact"].as_str().unwrap()).is_file(),
        "{endpoints}"
    );

    let scenario = json!({ "steps": [
        { "step": "goto", "path": "/" },
        { "step": "expect_text", "selector": "#greeting", "text": "Hello from the full-stack fixture" },
        { "step": "screenshot", "name": "home" }
    ] });
    let browser = tool(&registry, QA_BROWSER_TOOL, json!({ "scenario": scenario })).await;
    assert_eq!(browser["passed"], true, "{browser}");
    let screenshots = browser["screenshots"].as_array().unwrap();
    assert_eq!(screenshots.len(), 1, "{browser}");
    let screenshot = PathBuf::from(screenshots[0].as_str().unwrap());
    assert!(screenshot.is_file(), "{screenshot:?}");
    assert!(
        std::fs::metadata(&screenshot).unwrap().len() > 0,
        "screenshot must not be empty"
    );
    let console_errors = browser["console_errors"].as_array().unwrap();
    assert!(
        console_errors.iter().any(|e| e["url"]
            .as_str()
            .is_some_and(|url| url.ends_with("/favicon.ico"))
            && e["text"].as_str().is_some_and(|t| t.contains("404"))),
        "the browser's automatic favicon.ico request must 404 and be reported: {browser}"
    );

    let summary = ws.qa_summary();
    assert_eq!(summary.endpoint_runs, 1);
    assert_eq!(summary.browser_runs, 1);
    assert!(!summary.artifacts.is_empty());

    ws.cleanup().expect("cleanup must succeed");
}

#[tokio::test]
#[ignore]
async fn qa_endpoints_reports_the_broken_route_with_its_response_snippet() {
    let runtime = detect_runtime();
    if !runtime.is_available() {
        eprintln!("No container runtime available, skipping test");
        return;
    }
    build_dev_image(&runtime);

    let source = tempfile::tempdir().unwrap();
    copy_dir_all(&fixture_root(), source.path());
    init_repo(source.path());
    let mut ws = start_workspace(
        source.path(),
        "qa-break",
        vec![("FIXTURE_BREAK_ROUTE".to_string(), "1".to_string())],
    )
    .await;
    let registry = ws.build_tool_registry();

    tool(&registry, APP_START_TOOL, Value::Null).await;
    let endpoints = tool(&registry, QA_ENDPOINTS_TOOL, json!({})).await;
    assert_eq!(endpoints["all_passed"], false, "{endpoints}");
    assert_eq!(endpoints["passed"], 2, "{endpoints}");
    assert_eq!(endpoints["failed"], 1, "{endpoints}");
    let checks = endpoints["checks"].as_array().unwrap();
    let health = checks.iter().find(|c| c["path"] == "/health/v1").unwrap();
    assert_eq!(health["passed"], true, "{health}");
    let greeting = checks
        .iter()
        .find(|c| c["path"] == "/api/v1/greeting")
        .unwrap();
    assert_eq!(greeting["passed"], false, "{greeting}");
    assert_eq!(greeting["status"], 500, "{greeting}");
    assert!(
        greeting["snippet"]
            .as_str()
            .unwrap()
            .contains("route broken by FIXTURE_BREAK_ROUTE=1"),
        "{greeting}"
    );

    ws.cleanup().expect("cleanup must succeed");
}
