//! Integration tests for AGENTS.md discovery at session/task start (issue #231).
//!
//! These tests live in `tests/` (not `src/**`) so they exercise the public
//! crate surface the same way downstream callers do: a temp directory plays
//! the role of an onboarded repo, and the test invokes the public `load` /
//! `format_system_prompt_fragment` functions just like `main.rs` and
//! `task.rs` do. Criterion patch coverage stays at 100% because every branch
//! in `agents_md.rs` is already exercised by the unit tests; these integration
//! tests add realistic end-to-end scenarios on top.

use harness::agent::agents_md::{
    format_system_prompt_fragment, load, AgentsMdSource, AGENTS_MD_FILENAME, CLAUDE_MD_FILENAME,
    MAX_AGENTS_MD_BYTES,
};
use std::fs;
use tempfile::tempdir;

/// Happy-path: a realistic AGENTS.md in an onboarded-repo-like directory is
/// loaded and can be formatted into a system-prompt fragment suitable for
/// splicing into the model context.
#[test]
fn present_agents_md_is_loaded_and_formatted_into_context() {
    let dir = tempdir().unwrap();
    // Minimal realistic layout: the repo root has an AGENTS.md alongside a
    // Cargo.toml and a flake.nix, matching a Nix-onboarded Rust workspace.
    fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(dir.path().join("flake.nix"), "{}").unwrap();
    let body = "# AGENTS.md\n\nRun `cargo nextest run --workspace` before committing.\n";
    fs::write(dir.path().join(AGENTS_MD_FILENAME), body).unwrap();

    let doc = load(dir.path())
        .expect("load must not error on valid AGENTS.md")
        .expect("AGENTS.md should have been discovered");

    assert_eq!(doc.source, AgentsMdSource::AgentsMd);
    assert!(!doc.truncated);
    assert_eq!(doc.body, body);

    let fragment = format_system_prompt_fragment(&doc);
    assert!(fragment.contains("<repo-guidance source=\"AGENTS.md\">"));
    assert!(fragment.contains("cargo nextest run --workspace"));
    assert!(fragment.ends_with("</repo-guidance>"));
}

/// Missing both files in a clean repo is silent: no error, no injection.
#[test]
fn missing_files_produce_no_injection_and_no_error() {
    let dir = tempdir().unwrap();
    // Typical fresh checkout with only source metadata; no agent guidance.
    fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();

    let result = load(dir.path()).expect("missing files must not error");
    assert!(result.is_none(), "no guidance should be surfaced");
}

/// Oversize AGENTS.md is truncated; the caller receives usable content with
/// `truncated = true` so it can mark the entity or emit its own log.
#[test]
fn oversize_agents_md_is_truncated_to_cap() {
    let dir = tempdir().unwrap();
    let mut content = String::from("# AGENTS.md\n\n");
    // Pad with printable ASCII past the cap.
    content.push_str(&"x".repeat((MAX_AGENTS_MD_BYTES as usize) + 4096));
    fs::write(dir.path().join(AGENTS_MD_FILENAME), &content).unwrap();

    let doc = load(dir.path()).unwrap().unwrap();
    assert_eq!(doc.source, AgentsMdSource::AgentsMd);
    assert!(doc.truncated, "oversize file must be marked as truncated");
    assert_eq!(doc.body.len(), MAX_AGENTS_MD_BYTES as usize);
    // Truncation preserves the prefix so the heading is intact.
    assert!(doc.body.starts_with("# AGENTS.md"));
}

/// Both files present: AGENTS.md wins, matching the cross-tool convention.
#[test]
fn agents_md_preferred_over_claude_md_when_both_present() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(AGENTS_MD_FILENAME), "AGENTS guidance\n").unwrap();
    fs::write(dir.path().join(CLAUDE_MD_FILENAME), "CLAUDE guidance\n").unwrap();

    let doc = load(dir.path()).unwrap().unwrap();
    assert_eq!(doc.source, AgentsMdSource::AgentsMd);
    assert_eq!(doc.body, "AGENTS guidance\n");

    let fragment = format_system_prompt_fragment(&doc);
    assert!(fragment.contains("AGENTS guidance"));
    assert!(!fragment.contains("CLAUDE guidance"));
}

/// CLAUDE.md fallback path is exercised when only the legacy file exists.
#[test]
fn claude_md_fallback_used_when_agents_md_absent() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join(CLAUDE_MD_FILENAME), "legacy guidance").unwrap();

    let doc = load(dir.path()).unwrap().unwrap();
    assert_eq!(doc.source, AgentsMdSource::ClaudeMd);
    assert_eq!(doc.body, "legacy guidance");

    let fragment = format_system_prompt_fragment(&doc);
    assert!(fragment.contains("source=\"CLAUDE.md\""));
}
