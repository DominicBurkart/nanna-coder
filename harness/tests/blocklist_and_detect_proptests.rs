//! Property-based invariant tests for two pure, security-relevant modules:
//!
//! 1. [`harness::onboarding::profile::ToolSpec::new`] — the constructor is the
//!    sole guard against onboarding profiles that name destructive shell
//!    commands (publish/deploy/push/rm -rf/drop/delete/destroy). Existing
//!    example-based tests in `profile.rs` cover a handful of literal cases;
//!    these proptests assert the underlying invariants hold across a much
//!    broader input space.
//!
//! 2. [`harness::agent::project_detect::detect`] — the public composer is
//!    documented to deduplicate signals while preserving first-seen order.
//!    Existing tests check dedup for one variant (Rust). These proptests
//!    exercise dedup over arbitrary file combinations.
//!
//! Strategy rationale
//! -------------------
//! - Proptest is appropriate here because both surfaces have a small number of
//!   load-bearing invariants that are awkward to enumerate exhaustively as
//!   examples. Both surfaces are pure (no I/O beyond a tempdir) so shrinking
//!   yields tight failure cases.
//! - Inputs are restricted to printable ASCII (and a curated tail of
//!   destructive verbs) so failures are legible in shrunk form.

use std::fs;
use std::path::Path;

use proptest::prelude::*;
use tempfile::tempdir;

use harness::agent::project_detect::{detect, DetectedProfile, Framework, Language};
use harness::onboarding::profile::{ToolCategory, ToolSpec};

// ---------------------------------------------------------------------------
// ToolSpec blocklist invariants
// ---------------------------------------------------------------------------

/// Tokens the constructor must reject when they appear as standalone
/// whitespace-separated tokens (or, for `rm -rf`, as the adjacent token pair).
const SINGLE_TOKEN_BLOCKED: &[&str] = &["publish", "deploy", "push", "drop", "delete", "destroy"];

/// Generate strings that contain only whitespace-safe printable ASCII so that
/// `split_whitespace` tokenization is well-defined and shrunk failure cases
/// are readable.
fn safe_word() -> impl Strategy<Value = String> {
    // Letters, digits, and common shell-safe punctuation. No whitespace.
    "[A-Za-z0-9_./=:+@-]{1,12}".prop_filter("non-empty, no whitespace", |s| {
        !s.is_empty() && !s.chars().any(|c| c.is_whitespace())
    })
}

fn safe_command() -> impl Strategy<Value = String> {
    proptest::collection::vec(safe_word(), 1..6).prop_map(|words| words.join(" "))
}

fn category_strategy() -> impl Strategy<Value = ToolCategory> {
    prop_oneof![
        Just(ToolCategory::Build),
        Just(ToolCategory::Test),
        Just(ToolCategory::Lint),
        Just(ToolCategory::Format),
        Just(ToolCategory::Check),
    ]
}

proptest! {
    /// Invariant: a command containing a blocked token as a *standalone
    /// whitespace-separated token* is always rejected, regardless of where it
    /// sits in the command line.
    #[test]
    fn blocked_token_anywhere_is_rejected(
        prefix in proptest::collection::vec(safe_word(), 0..3),
        suffix in proptest::collection::vec(safe_word(), 0..3),
        blocked_idx in 0usize..SINGLE_TOKEN_BLOCKED.len(),
        category in category_strategy(),
    ) {
        let blocked = SINGLE_TOKEN_BLOCKED[blocked_idx];
        let mut tokens = prefix;
        tokens.push(blocked.to_string());
        tokens.extend(suffix);
        let command = tokens.join(" ");

        let result = ToolSpec::new("tool", command.clone(), "desc", category);
        prop_assert!(
            result.is_err(),
            "blocked token {:?} in command {:?} must be rejected",
            blocked,
            command,
        );
    }

    /// Invariant: substring occurrences of a blocked token *within* a longer
    /// word (e.g. "publisher", "redeploy") must NOT be rejected. The blocklist
    /// is whitespace-token-aware, not substring-based.
    #[test]
    fn blocked_substring_within_word_is_accepted(
        prefix in "[a-z]{1,5}",
        suffix in "[a-z]{1,5}",
        blocked_idx in 0usize..SINGLE_TOKEN_BLOCKED.len(),
        category in category_strategy(),
    ) {
        // Glue prefix + blocked + suffix into a single non-whitespace word so
        // the blocked verb is a strict substring of a longer identifier.
        let blocked = SINGLE_TOKEN_BLOCKED[blocked_idx];
        let glued = format!("{prefix}{blocked}{suffix}");
        // Sanity: ensure the blocked verb is not its own token in `glued`.
        // (The strategy guarantees both prefix and suffix are non-empty
        // alphabetic, so this holds — but assert defensively.)
        prop_assume!(
            !glued.split_whitespace().any(|t| t == blocked)
        );

        let result = ToolSpec::new("tool", glued.as_str(), "desc", category);
        prop_assert!(
            result.is_ok(),
            "substring occurrence {:?} of blocked {:?} must NOT be rejected",
            glued,
            blocked,
        );
    }

    /// Invariant: the multi-token blocked phrase "rm -rf" is detected iff its
    /// two tokens appear *adjacent* in the command. A bare "rm" or a stray
    /// "-rf" is fine.
    #[test]
    fn rm_rf_phrase_is_window_matched(
        head in proptest::collection::vec(safe_word(), 0..3),
        tail in proptest::collection::vec(safe_word(), 0..3),
        category in category_strategy(),
    ) {
        // Adjacent "rm -rf" must be rejected.
        let mut adj = head.clone();
        adj.push("rm".to_string());
        adj.push("-rf".to_string());
        adj.extend(tail.clone());
        let adj_cmd = adj.join(" ");
        prop_assert!(
            ToolSpec::new("t", adj_cmd.as_str(), "d", category.clone()).is_err(),
            "adjacent 'rm -rf' must be rejected: {:?}",
            adj_cmd,
        );

        // Bare "rm" without "-rf" is allowed (it's not on the single-token
        // blocklist; only the multi-word window matters).
        let mut bare = head.clone();
        bare.push("rm".to_string());
        bare.extend(tail.clone());
        let bare_cmd = bare.join(" ");
        // Skip if random fillers happen to contain a blocked single token.
        prop_assume!(!bare_cmd
            .split_whitespace()
            .any(|t| SINGLE_TOKEN_BLOCKED.contains(&t)));
        prop_assert!(
            ToolSpec::new("t", bare_cmd.as_str(), "d", category).is_ok(),
            "bare 'rm' (no adjacent '-rf') must be accepted: {:?}",
            bare_cmd,
        );
    }

    /// Invariant: when `ToolSpec::new` succeeds, every input field is
    /// preserved verbatim.
    #[test]
    fn accepted_spec_preserves_fields(
        name in "[A-Za-z][A-Za-z0-9_-]{0,15}",
        command in safe_command(),
        description in "[A-Za-z0-9 .,!?-]{0,40}",
        category in category_strategy(),
    ) {
        // Discard any randomly-generated command that would be blocked; we are
        // asserting the success branch's behaviour here.
        prop_assume!(!command
            .split_whitespace()
            .any(|t| SINGLE_TOKEN_BLOCKED.contains(&t)));
        let tokens: Vec<&str> = command.split_whitespace().collect();
        prop_assume!(!tokens.windows(2).any(|w| w == ["rm", "-rf"]));

        let spec = ToolSpec::new(
            name.as_str(),
            command.as_str(),
            description.as_str(),
            category.clone(),
        )
        .expect("non-blocked command should construct");

        prop_assert_eq!(spec.name, name);
        prop_assert_eq!(spec.command, command);
        prop_assert_eq!(spec.description, description);
        prop_assert_eq!(spec.category, category);
    }
}

// ---------------------------------------------------------------------------
// project_detect::detect dedup invariants
// ---------------------------------------------------------------------------

fn write(dir: &Path, rel: &str, contents: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn assert_no_duplicates(profile: &DetectedProfile) {
    // Languages
    let mut langs: Vec<Language> = profile.languages.clone();
    langs.sort();
    let original_len = langs.len();
    langs.dedup();
    assert_eq!(
        langs.len(),
        original_len,
        "languages must be deduplicated, got {:?}",
        profile.languages,
    );

    // Frameworks
    let mut fws: Vec<Framework> = profile.frameworks.clone();
    fws.sort();
    let original_len = fws.len();
    fws.dedup();
    assert_eq!(
        fws.len(),
        original_len,
        "frameworks must be deduplicated, got {:?}",
        profile.frameworks,
    );

    // CI expectations
    let mut ci: Vec<String> = profile.ci_expectations.clone();
    ci.sort();
    let original_len = ci.len();
    ci.dedup();
    assert_eq!(
        ci.len(),
        original_len,
        "ci_expectations must be deduplicated, got {:?}",
        profile.ci_expectations,
    );
}

proptest! {
    /// Invariant: regardless of which subset of recognized manifest files is
    /// present, [`detect`] never returns duplicate entries in any of its
    /// three result vectors. This complements the existing single-case
    /// `composite_rust_nix_ci_profile` test by sweeping the full power-set of
    /// manifest combinations.
    #[test]
    fn detect_dedups_across_arbitrary_manifest_combinations(
        // Each bool toggles whether a particular manifest file is present.
        has_cargo in any::<bool>(),
        has_package_json in any::<bool>(),
        has_pyproject in any::<bool>(),
        has_requirements in any::<bool>(),
        has_go_mod in any::<bool>(),
        has_flake in any::<bool>(),
        has_workflow in any::<bool>(),
    ) {
        let dir = tempdir().unwrap();

        if has_cargo {
            // Cargo manifest that triggers Rust + Cargo + Tokio + Nextest +
            // Proptest + Criterion. Tokio appears in BOTH [dependencies] and
            // [dev-dependencies] to provoke a dedup challenge.
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
tokio = "1"
nextest = "0.9"
proptest = "1"
criterion = "0.5"
"#,
            );
        }
        if has_package_json {
            // Trigger Jest + Vitest + Mocha + Playwright via both deps AND
            // scripts (deliberate overlap).
            write(
                dir.path(),
                "package.json",
                r#"{
  "name": "x",
  "devDependencies": {
    "jest": "^29",
    "vitest": "^1",
    "mocha": "^10",
    "@playwright/test": "^1"
  },
  "scripts": {
    "test": "jest && vitest run && mocha",
    "e2e": "playwright test"
  }
}"#,
            );
        }
        if has_pyproject {
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
        }
        if has_requirements {
            // Overlap with pyproject.toml on the Python language signal and
            // the Pytest framework signal — dedup must collapse them.
            write(dir.path(), "requirements.txt", "pytest>=7\ntox\n");
        }
        if has_go_mod {
            write(dir.path(), "go.mod", "module x\n\ngo 1.22\n");
        }
        if has_flake {
            write(dir.path(), "flake.nix", "{ outputs = _: {}; }");
        }
        if has_workflow {
            write(
                dir.path(),
                ".github/workflows/ci.yml",
                "jobs:\n  t:\n    steps:\n      - run: cargo fmt -- --check\n      - run: cargo nextest run\n      - run: cargo fmt -- --check\n",
            );
        }

        let profile = detect(dir.path());
        assert_no_duplicates(&profile);

        // Empty workspace must yield a fully empty profile — sanity check.
        if !(has_cargo
            || has_package_json
            || has_pyproject
            || has_requirements
            || has_go_mod
            || has_flake
            || has_workflow)
        {
            prop_assert!(profile.languages.is_empty());
            prop_assert!(profile.frameworks.is_empty());
            prop_assert!(profile.ci_expectations.is_empty());
        }
    }

    /// Invariant: [`detect`] is deterministic — calling it twice on the same
    /// workspace yields equal profiles (including ordering).
    #[test]
    fn detect_is_deterministic(
        has_cargo in any::<bool>(),
        has_flake in any::<bool>(),
        has_workflow in any::<bool>(),
    ) {
        let dir = tempdir().unwrap();
        if has_cargo {
            write(
                dir.path(),
                "Cargo.toml",
                "[package]\nname=\"x\"\nversion=\"0.1.0\"\n\n[dev-dependencies]\nnextest=\"0.9\"\n",
            );
        }
        if has_flake {
            write(dir.path(), "flake.nix", "{ outputs = _: {}; }");
        }
        if has_workflow {
            write(
                dir.path(),
                ".github/workflows/ci.yml",
                "jobs:\n  t:\n    steps:\n      - run: cargo nextest run\n",
            );
        }

        let a = detect(dir.path());
        let b = detect(dir.path());
        prop_assert_eq!(a, b);
    }
}
