//! Language, framework, and CI-expectation detectors for the workspace root.
//!
//! Each detector is a pure function that reads a single file (or directory)
//! under `workspace_root` and emits zero or more [`Signal`]s. [`detect`]
//! composes them into a [`DetectedProfile`] that downstream phases will feed
//! into the system-prompt assembler (issue #194, Phase B).
//!
//! # Relationship to the existing hardcoded system prompt
//!
//! This module only produces data. It does **not** modify the two current
//! hardcoded system-prompt call sites:
//!
//! - `harness/src/main.rs:425`
//! - `harness/src/task.rs:369`
//!
//! Those are rewritten in Phase D. Phase B (this module) is deliberately
//! limited to detector logic + tests so it can land in parallel with the
//! Phase A loader.
//!
//! # Design notes
//!
//! - All detectors tolerate missing or malformed files and return an empty
//!   [`Vec<Signal>`] in those cases. Parse errors are silently dropped: a
//!   project with a broken `Cargo.toml` should still produce an agent prompt.
//! - Detectors do not walk outside `workspace_root` except for the
//!   single-level glob in [`detect_github_workflows`].
//! - The enums are deliberately closed - new languages or frameworks require
//!   a code change (and a test) so typos cannot sneak in via config.

use regex::Regex;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Detected languages in a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    Rust,
    Node,
    Python,
    Go,
    Nix,
}

/// Detected testing / tooling frameworks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Framework {
    Cargo,
    Nextest,
    Tokio,
    Proptest,
    Criterion,
    Jest,
    Vitest,
    Mocha,
    Playwright,
    Pytest,
    Poetry,
    Uv,
    Ruff,
    Mypy,
    Tox,
    Flake,
}

/// Aggregated detection result for the whole workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectedProfile {
    pub languages: Vec<Language>,
    pub frameworks: Vec<Framework>,
    pub ci_expectations: Vec<String>,
}

/// A single detector observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    Language(Language),
    Framework(Framework),
    /// Free-form CI or tooling expectation, e.g. "use `cargo nextest run`".
    CiExpectation(String),
}

/// Run every detector against `workspace_root` and compose a
/// [`DetectedProfile`]. Duplicate signals are de-duplicated while preserving
/// first-seen order.
pub fn detect(workspace_root: &Path) -> DetectedProfile {
    let mut signals = Vec::new();
    signals.extend(detect_cargo_toml(workspace_root));
    signals.extend(detect_package_json(workspace_root));
    signals.extend(detect_pyproject_toml(workspace_root));
    signals.extend(detect_requirements_txt(workspace_root));
    signals.extend(detect_go_mod(workspace_root));
    signals.extend(detect_flake_nix(workspace_root));
    signals.extend(detect_github_workflows(workspace_root));

    let mut profile = DetectedProfile::default();
    for signal in signals {
        match signal {
            Signal::Language(l) => {
                if !profile.languages.contains(&l) {
                    profile.languages.push(l);
                }
            }
            Signal::Framework(f) => {
                if !profile.frameworks.contains(&f) {
                    profile.frameworks.push(f);
                }
            }
            Signal::CiExpectation(s) => {
                if !profile.ci_expectations.contains(&s) {
                    profile.ci_expectations.push(s);
                }
            }
        }
    }
    profile
}

/// Read `workspace_root/Cargo.toml` if present and emit Rust + detected
/// dev-dependency frameworks.
pub fn detect_cargo_toml(workspace_root: &Path) -> Vec<Signal> {
    let path = workspace_root.join("Cargo.toml");
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(parsed) = toml::from_str::<CargoManifest>(&contents) else {
        // Tolerate malformed manifests - still at least flag Rust.
        return vec![
            Signal::Language(Language::Rust),
            Signal::Framework(Framework::Cargo),
        ];
    };

    let mut out = vec![
        Signal::Language(Language::Rust),
        Signal::Framework(Framework::Cargo),
    ];

    for (name, _) in parsed.dev_dependencies.iter() {
        match name.as_str() {
            "nextest" | "cargo-nextest" => out.push(Signal::Framework(Framework::Nextest)),
            "tokio" => out.push(Signal::Framework(Framework::Tokio)),
            "proptest" => out.push(Signal::Framework(Framework::Proptest)),
            "criterion" => out.push(Signal::Framework(Framework::Criterion)),
            _ => {}
        }
    }
    // tokio often appears in regular [dependencies] too for async projects.
    for (name, _) in parsed.dependencies.iter() {
        if name == "tokio" {
            out.push(Signal::Framework(Framework::Tokio));
        }
    }

    out
}

#[derive(Debug, Default, Deserialize)]
struct CargoManifest {
    #[serde(default, rename = "dev-dependencies")]
    dev_dependencies: toml::Table,
    #[serde(default)]
    dependencies: toml::Table,
}

/// Read `workspace_root/package.json` if present and emit Node + JS
/// test-framework signals from `devDependencies` and `scripts`.
pub fn detect_package_json(workspace_root: &Path) -> Vec<Signal> {
    let path = workspace_root.join("package.json");
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return vec![Signal::Language(Language::Node)];
    };

    let mut out = vec![Signal::Language(Language::Node)];

    let mut names: Vec<String> = Vec::new();
    if let Some(dd) = parsed.get("devDependencies").and_then(|v| v.as_object()) {
        names.extend(dd.keys().cloned());
    }
    if let Some(d) = parsed.get("dependencies").and_then(|v| v.as_object()) {
        names.extend(d.keys().cloned());
    }
    let scripts_blob = parsed
        .get("scripts")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.values()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default();

    let push_if = |condition: bool, fw: Framework, out: &mut Vec<Signal>| {
        if condition {
            out.push(Signal::Framework(fw));
        }
    };

    let has = |needle: &str| {
        names.iter().any(|n| n == needle) || scripts_blob.split_whitespace().any(|w| w == needle)
    };

    push_if(has("jest"), Framework::Jest, &mut out);
    push_if(has("vitest"), Framework::Vitest, &mut out);
    push_if(has("mocha"), Framework::Mocha, &mut out);
    push_if(
        has("playwright") || names.iter().any(|n| n == "@playwright/test"),
        Framework::Playwright,
        &mut out,
    );

    out
}

/// Read `workspace_root/pyproject.toml` and emit Python + Python-tooling
/// framework signals based on `[tool.*]` tables.
pub fn detect_pyproject_toml(workspace_root: &Path) -> Vec<Signal> {
    let path = workspace_root.join("pyproject.toml");
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&contents) else {
        return vec![Signal::Language(Language::Python)];
    };

    let mut out = vec![Signal::Language(Language::Python)];
    let tool = parsed.get("tool").and_then(|v| v.as_table());

    if let Some(tool) = tool {
        if tool
            .get("pytest")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("ini_options"))
            .is_some()
            || tool.contains_key("pytest")
        {
            out.push(Signal::Framework(Framework::Pytest));
        }
        if tool.contains_key("poetry") {
            out.push(Signal::Framework(Framework::Poetry));
        }
        if tool.contains_key("uv") {
            out.push(Signal::Framework(Framework::Uv));
        }
        if tool.contains_key("ruff") {
            out.push(Signal::Framework(Framework::Ruff));
        }
        if tool.contains_key("mypy") {
            out.push(Signal::Framework(Framework::Mypy));
        }
    }

    out
}

/// Read `workspace_root/requirements.txt` and regex-scan for pytest / tox
/// entries.
pub fn detect_requirements_txt(workspace_root: &Path) -> Vec<Signal> {
    let path = workspace_root.join("requirements.txt");
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };

    let mut out = vec![Signal::Language(Language::Python)];

    // Match package name at line start (allowing leading whitespace) followed
    // by end-of-name punctuation. Case-insensitive.
    let pytest_re = Regex::new(r"(?im)^\s*pytest(\s*$|[\[=<>!~])").unwrap();
    let tox_re = Regex::new(r"(?im)^\s*tox(\s*$|[\[=<>!~])").unwrap();

    if pytest_re.is_match(&contents) {
        out.push(Signal::Framework(Framework::Pytest));
    }
    if tox_re.is_match(&contents) {
        out.push(Signal::Framework(Framework::Tox));
    }

    out
}

/// Detect Go via `go.mod`. Requires a `go <version>` directive.
pub fn detect_go_mod(workspace_root: &Path) -> Vec<Signal> {
    let path = workspace_root.join("go.mod");
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let re = Regex::new(r"(?m)^go\s+\d+\.\d+").unwrap();
    if re.is_match(&contents) {
        vec![Signal::Language(Language::Go)]
    } else {
        Vec::new()
    }
}

/// Detect Nix flakes by file existence.
pub fn detect_flake_nix(workspace_root: &Path) -> Vec<Signal> {
    let path = workspace_root.join("flake.nix");
    if path.is_file() {
        vec![
            Signal::Language(Language::Nix),
            Signal::Framework(Framework::Flake),
            Signal::CiExpectation(
                "use `nix develop --command ...` for hermetic builds".to_string(),
            ),
        ]
    } else {
        Vec::new()
    }
}

/// Walk `.github/workflows/*.yml` (and `.yaml`) and regex-scan `run:` lines
/// for CI expectations that the agent should respect.
pub fn detect_github_workflows(workspace_root: &Path) -> Vec<Signal> {
    let dir = workspace_root.join(".github").join("workflows");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|r| r.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && matches!(
                    p.extension().and_then(|e| e.to_str()),
                    Some("yml") | Some("yaml")
                )
        })
        .collect();
    paths.sort();

    // Match a YAML run-step: lines like `- run: cargo fmt` or `run: |` blocks.
    let run_re = Regex::new(r"(?m)^\s*-?\s*run:\s*(.*)$").unwrap();

    let mut out = Vec::new();
    let seen = |needle: &str, expectation: &str, out: &mut Vec<Signal>| {
        let exp = expectation.to_string();
        if !out
            .iter()
            .any(|s| matches!(s, Signal::CiExpectation(e) if e == &exp))
        {
            out.push(Signal::CiExpectation(exp));
        }
        let _ = needle;
    };

    for path in paths {
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        // Collect each `run:` tail plus any indented continuation for `run: |`.
        // For simplicity we scan the entire file body against each needle; the
        // YAML structure is only used to skip obvious non-run lines.
        let mut run_bodies = String::new();
        for cap in run_re.captures_iter(&contents) {
            run_bodies.push_str(cap.get(1).map_or("", |m| m.as_str()));
            run_bodies.push('\n');
        }
        // Also include the whole file so multi-line `run: |` blocks are caught
        // (cheap and safe - false positives here would only be other YAML
        // strings that literally mention these commands).
        let haystack = format!("{run_bodies}\n{contents}");

        if haystack.contains("cargo fmt") {
            seen("cargo fmt", "CI runs `cargo fmt -- --check`", &mut out);
        }
        if haystack.contains("cargo clippy") {
            seen(
                "cargo clippy",
                "CI runs `cargo clippy -- -D warnings`",
                &mut out,
            );
        }
        if haystack.contains("cargo deny") {
            seen("cargo deny", "CI runs `cargo deny check`", &mut out);
        }
        if haystack.contains("nextest") {
            seen("nextest", "CI runs tests via `cargo nextest`", &mut out);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn cargo_toml_rust_plus_frameworks() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"

[dependencies]
tokio = "1"

[dev-dependencies]
nextest = "0.9"
proptest = "1"
criterion = "0.5"
"#,
        );
        let signals = detect_cargo_toml(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Rust)));
        assert!(signals.contains(&Signal::Framework(Framework::Cargo)));
        assert!(signals.contains(&Signal::Framework(Framework::Nextest)));
        assert!(signals.contains(&Signal::Framework(Framework::Tokio)));
        assert!(signals.contains(&Signal::Framework(Framework::Proptest)));
        assert!(signals.contains(&Signal::Framework(Framework::Criterion)));
    }

    #[test]
    fn cargo_toml_missing_returns_empty() {
        let dir = tempdir().unwrap();
        assert!(detect_cargo_toml(dir.path()).is_empty());
    }

    #[test]
    fn cargo_toml_malformed_still_flags_rust() {
        let dir = tempdir().unwrap();
        write(dir.path(), "Cargo.toml", "this is = = not toml");
        let signals = detect_cargo_toml(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Rust)));
    }

    #[test]
    fn package_json_node_plus_frameworks() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{
  "name": "x",
  "devDependencies": {
    "jest": "^29",
    "@playwright/test": "^1"
  },
  "scripts": {
    "test": "vitest run",
    "e2e": "playwright test"
  }
}"#,
        );
        let signals = detect_package_json(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Node)));
        assert!(signals.contains(&Signal::Framework(Framework::Jest)));
        assert!(signals.contains(&Signal::Framework(Framework::Vitest)));
        assert!(signals.contains(&Signal::Framework(Framework::Playwright)));
        assert!(!signals.contains(&Signal::Framework(Framework::Mocha)));
    }

    #[test]
    fn package_json_missing_returns_empty() {
        let dir = tempdir().unwrap();
        assert!(detect_package_json(dir.path()).is_empty());
    }

    #[test]
    fn pyproject_toml_tool_tables() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "pyproject.toml",
            r#"
[tool.poetry]
name = "x"

[tool.pytest.ini_options]
minversion = "7"

[tool.ruff]
line-length = 100

[tool.mypy]
strict = true

[tool.uv]
"#,
        );
        let signals = detect_pyproject_toml(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Python)));
        assert!(signals.contains(&Signal::Framework(Framework::Pytest)));
        assert!(signals.contains(&Signal::Framework(Framework::Poetry)));
        assert!(signals.contains(&Signal::Framework(Framework::Ruff)));
        assert!(signals.contains(&Signal::Framework(Framework::Mypy)));
        assert!(signals.contains(&Signal::Framework(Framework::Uv)));
    }

    #[test]
    fn requirements_txt_scans_pytest_and_tox() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "requirements.txt",
            "requests==2.31.0\npytest>=7.0\ntox\n",
        );
        let signals = detect_requirements_txt(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Python)));
        assert!(signals.contains(&Signal::Framework(Framework::Pytest)));
        assert!(signals.contains(&Signal::Framework(Framework::Tox)));
    }

    #[test]
    fn requirements_txt_no_matching_deps() {
        let dir = tempdir().unwrap();
        write(dir.path(), "requirements.txt", "requests==2.31.0\n");
        let signals = detect_requirements_txt(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Python)));
        assert!(!signals.contains(&Signal::Framework(Framework::Pytest)));
    }

    #[test]
    fn go_mod_version_detected() {
        let dir = tempdir().unwrap();
        write(dir.path(), "go.mod", "module x\n\ngo 1.22\n");
        let signals = detect_go_mod(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Go)));
    }

    #[test]
    fn go_mod_missing_version_not_detected() {
        let dir = tempdir().unwrap();
        write(dir.path(), "go.mod", "module x\n");
        assert!(detect_go_mod(dir.path()).is_empty());
    }

    #[test]
    fn flake_nix_present_emits_expectation() {
        let dir = tempdir().unwrap();
        write(dir.path(), "flake.nix", "{ outputs = _: {}; }");
        let signals = detect_flake_nix(dir.path());
        assert!(signals.contains(&Signal::Language(Language::Nix)));
        assert!(signals.contains(&Signal::Framework(Framework::Flake)));
        assert!(signals.iter().any(|s| matches!(
            s,
            Signal::CiExpectation(msg) if msg.contains("nix develop")
        )));
    }

    #[test]
    fn flake_nix_missing_emits_nothing() {
        let dir = tempdir().unwrap();
        assert!(detect_flake_nix(dir.path()).is_empty());
    }

    #[test]
    fn github_workflows_finds_cargo_commands() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            ".github/workflows/ci.yml",
            r#"
name: ci
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: cargo fmt --all -- --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo deny check
      - run: cargo nextest run
"#,
        );
        let signals = detect_github_workflows(dir.path());
        assert!(signals
            .iter()
            .any(|s| matches!(s, Signal::CiExpectation(m) if m.contains("cargo fmt"))));
        assert!(signals
            .iter()
            .any(|s| matches!(s, Signal::CiExpectation(m) if m.contains("clippy"))));
        assert!(signals
            .iter()
            .any(|s| matches!(s, Signal::CiExpectation(m) if m.contains("deny"))));
        assert!(signals
            .iter()
            .any(|s| matches!(s, Signal::CiExpectation(m) if m.contains("nextest"))));
    }

    #[test]
    fn github_workflows_directory_absent() {
        let dir = tempdir().unwrap();
        assert!(detect_github_workflows(dir.path()).is_empty());
    }

    #[test]
    fn composite_rust_nix_ci_profile() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "Cargo.toml",
            r#"
[package]
name = "x"
version = "0.1.0"

[dev-dependencies]
nextest = "0.9"
tokio = "1"
"#,
        );
        write(dir.path(), "flake.nix", "{ outputs = _: {}; }");
        write(
            dir.path(),
            ".github/workflows/ci.yml",
            "jobs:\n  t:\n    steps:\n      - run: cargo fmt -- --check\n      - run: cargo nextest run\n",
        );

        let profile = detect(dir.path());

        assert!(profile.languages.contains(&Language::Rust));
        assert!(profile.languages.contains(&Language::Nix));
        assert!(profile.frameworks.contains(&Framework::Cargo));
        assert!(profile.frameworks.contains(&Framework::Nextest));
        assert!(profile.frameworks.contains(&Framework::Tokio));
        assert!(profile.frameworks.contains(&Framework::Flake));

        // CI expectations should cover both the nix-develop hint and the
        // cargo-fmt / nextest lines from workflows.
        assert!(profile
            .ci_expectations
            .iter()
            .any(|m| m.contains("nix develop")));
        assert!(profile
            .ci_expectations
            .iter()
            .any(|m| m.contains("cargo fmt")));
        assert!(profile
            .ci_expectations
            .iter()
            .any(|m| m.contains("nextest")));

        // Deduplication: each language / framework appears exactly once.
        let rust_count = profile
            .languages
            .iter()
            .filter(|l| **l == Language::Rust)
            .count();
        assert_eq!(rust_count, 1);
    }

    #[test]
    fn empty_workspace_yields_empty_profile() {
        let dir = tempdir().unwrap();
        let profile = detect(dir.path());
        assert!(profile.languages.is_empty());
        assert!(profile.frameworks.is_empty());
        assert!(profile.ci_expectations.is_empty());
    }
}
