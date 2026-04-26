//! Edge-case tests for `harness::agent::project_detect` and
//! `harness::mcp`.
//!
//! The existing inline `#[cfg(test)]` block in `project_detect.rs` covers the
//! happy paths for each detector in isolation plus one composite scenario.
//! These tests fill the remaining gaps:
//!
//! * `detect_package_json` — mocha via `devDependencies`, and the fallback when
//!   the JSON is malformed.
//! * `detect_requirements_txt` — `pytest[cov]>=7` (bracket-qualified version
//!   spec, exercises the `[\[` arm of the regex).
//! * `detect_pyproject_toml` — bare `[tool.pytest]` table without an
//!   `ini_options` sub-table (exercises the `tool.contains_key("pytest")` branch
//!   that the existing test never reaches with only the `ini_options` path).
//! * `detect_github_workflows` — `.yaml` extension accepted; non-yml/yaml files
//!   skipped; duplicate CI expectations across two workflow files are emitted
//!   only once.
//! * `detect` (composite) — Python detected exactly once when both
//!   `requirements.txt` and `pyproject.toml` are present.
//! * `mcp::NannaMcpServer` — invalid JSON-RPC version rejected with -32600;
//!   `tools/call` with a missing `name` field returns -32602.

use harness::agent::project_detect::{
    detect, detect_github_workflows, detect_package_json, detect_pyproject_toml,
    detect_requirements_txt, Framework, Language, Signal,
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

// ── helpers ──────────────────────────────────────────────────────────────────

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

// ── detect_package_json ───────────────────────────────────────────────────────

/// Mocha listed as a `devDependency` must emit `Framework::Mocha`.
#[test]
fn package_json_detects_mocha_in_dev_dependencies() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "package.json",
        r#"{"name":"x","devDependencies":{"mocha":"^10"}}"#,
    );
    let signals = detect_package_json(dir.path());
    assert!(
        signals.contains(&Signal::Framework(Framework::Mocha)),
        "expected Mocha framework signal, got: {signals:?}"
    );
    assert!(signals.contains(&Signal::Language(Language::Node)));
}

/// Malformed JSON: parser should return `[Node]` without panicking.
#[test]
fn package_json_malformed_returns_node_only() {
    let dir = tempdir().unwrap();
    write(dir.path(), "package.json", "not { valid json");
    let signals = detect_package_json(dir.path());
    assert_eq!(signals, vec![Signal::Language(Language::Node)]);
}

// ── detect_requirements_txt ───────────────────────────────────────────────────

/// `pytest[cov]>=7` uses the `[` character immediately after the package name.
/// The regex character class `[\[=<>!~]` must match the opening bracket so that
/// extras-qualified specifiers are still recognised as pytest.
#[test]
fn requirements_txt_detects_pytest_with_extras_bracket() {
    let dir = tempdir().unwrap();
    write(dir.path(), "requirements.txt", "pytest[cov]>=7\n");
    let signals = detect_requirements_txt(dir.path());
    assert!(
        signals.contains(&Signal::Framework(Framework::Pytest)),
        "pytest[extras] must still be recognised as pytest: {signals:?}"
    );
}

// ── detect_pyproject_toml ─────────────────────────────────────────────────────

/// A bare `[tool.pytest]` table (no `ini_options` sub-table) must still emit
/// `Framework::Pytest` via the `tool.contains_key("pytest")` branch.
#[test]
fn pyproject_toml_bare_pytest_table_detected() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        "pyproject.toml",
        "[tool.pytest]\naddopts = \"-v\"\n",
    );
    let signals = detect_pyproject_toml(dir.path());
    assert!(
        signals.contains(&Signal::Framework(Framework::Pytest)),
        "bare [tool.pytest] table must emit Pytest: {signals:?}"
    );
}

// ── detect_github_workflows ───────────────────────────────────────────────────

/// `.yaml` files (not just `.yml`) must be scanned.
#[test]
fn github_workflows_yaml_extension_is_scanned() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        ".github/workflows/ci.yaml",
        "jobs:\n  t:\n    steps:\n      - run: cargo nextest run\n",
    );
    let signals = detect_github_workflows(dir.path());
    assert!(
        signals
            .iter()
            .any(|s| matches!(s, Signal::CiExpectation(m) if m.contains("nextest"))),
        "expected nextest expectation from .yaml file: {signals:?}"
    );
}

/// Files with extensions other than `.yml` / `.yaml` must be silently skipped
/// and must not produce signals.
#[test]
fn github_workflows_non_yaml_files_are_ignored() {
    let dir = tempdir().unwrap();
    // These files mention cargo commands but must be ignored.
    write(
        dir.path(),
        ".github/workflows/config.json",
        r#"{"run":"cargo fmt && cargo nextest run"}"#,
    );
    write(
        dir.path(),
        ".github/workflows/config.toml",
        "run = \"cargo clippy\"\n",
    );
    let signals = detect_github_workflows(dir.path());
    assert!(
        signals.is_empty(),
        "non-yml/yaml files must not produce signals: {signals:?}"
    );
}

/// When two workflow files both mention the same CI command the resulting
/// `CiExpectation` signal must appear exactly once (deduplication).
#[test]
fn github_workflows_duplicate_expectations_deduplicated() {
    let dir = tempdir().unwrap();
    write(
        dir.path(),
        ".github/workflows/lint.yml",
        "jobs:\n  l:\n    steps:\n      - run: cargo fmt -- --check\n",
    );
    write(
        dir.path(),
        ".github/workflows/test.yml",
        "jobs:\n  t:\n    steps:\n      - run: cargo fmt -- --check\n",
    );
    let signals = detect_github_workflows(dir.path());
    let fmt_count = signals
        .iter()
        .filter(|s| matches!(s, Signal::CiExpectation(m) if m.contains("cargo fmt")))
        .count();
    assert_eq!(
        fmt_count, 1,
        "cargo fmt expectation must appear exactly once, got {fmt_count}: {signals:?}"
    );
}

// ── detect (composite) ────────────────────────────────────────────────────────

/// When both `requirements.txt` and `pyproject.toml` are present, Python must
/// appear exactly once in the profile (deduplication invariant in `detect`).
#[test]
fn detect_deduplicates_python_from_multiple_sources() {
    let dir = tempdir().unwrap();
    write(dir.path(), "requirements.txt", "requests\n");
    write(dir.path(), "pyproject.toml", "[tool.ruff]\n");
    let profile = detect(dir.path());
    let python_count = profile
        .languages
        .iter()
        .filter(|l| **l == Language::Python)
        .count();
    assert_eq!(
        python_count, 1,
        "Python must appear exactly once when detected by multiple sources: {profile:?}"
    );
}

// ── mcp server ────────────────────────────────────────────────────────────────
//
// Tests here exercise the `serve()` public API end-to-end. `process_line` is
// intentionally private; routing through `serve` is the correct external
// interface.

mod mcp_edge_cases {
    use async_trait::async_trait;
    use harness::mcp::NannaMcpServer;
    use harness::task::TaskManager;
    use model::provider::{ModelProvider, ModelResult};
    use model::types::{ChatRequest, ChatResponse, ModelInfo};
    use std::sync::Arc;

    struct NoopProvider;

    #[async_trait]
    impl ModelProvider for NoopProvider {
        async fn chat(&self, _: ChatRequest) -> ModelResult<ChatResponse> {
            unimplemented!()
        }
        async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn health_check(&self) -> ModelResult<()> {
            Ok(())
        }
        fn provider_name(&self) -> &'static str {
            "noop"
        }
    }

    fn make_server() -> NannaMcpServer {
        NannaMcpServer::new(
            Arc::new(TaskManager::default()),
            Arc::new(NoopProvider),
            "qwen3:0.6b".to_string(),
            100,
        )
    }

    /// Drive a single newline-terminated JSON-RPC request through `serve` and
    /// return the parsed response. Panics if `serve` errors or if the output
    /// is not valid JSON.
    async fn round_trip(request_line: &str) -> serde_json::Value {
        let input = request_line.as_bytes().to_vec();
        let mut output: Vec<u8> = Vec::new();
        make_server()
            .serve(std::io::Cursor::new(input), &mut output)
            .await
            .expect("serve must not error");
        let text = std::str::from_utf8(&output).expect("output must be utf-8");
        // `serve` writes one newline-terminated JSON object per response.
        let line = text
            .lines()
            .next()
            .expect("serve must produce at least one response line");
        serde_json::from_str(line).expect("response must be valid JSON")
    }

    /// A JSON-RPC request with `"jsonrpc": "1.0"` must be rejected with
    /// error code -32600 (Invalid Request).
    #[tokio::test]
    async fn invalid_jsonrpc_version_returns_32600() {
        let v = round_trip(
            "{\"jsonrpc\":\"1.0\",\"id\":99,\"method\":\"tools/list\",\"params\":{}}\n",
        )
        .await;
        assert_eq!(
            v["error"]["code"], -32600,
            "expected -32600 for bad JSON-RPC version: {v}"
        );
        assert_eq!(v["id"], 99);
    }

    /// A `tools/call` request with no `name` field must return error code
    /// -32602 (Invalid Params) rather than panicking.
    #[tokio::test]
    async fn tools_call_missing_name_returns_32602() {
        let v = round_trip(
            "{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"arguments\":{}}}\n",
        )
        .await;
        assert_eq!(
            v["error"]["code"], -32602,
            "expected -32602 for missing tool name: {v}"
        );
    }
}
