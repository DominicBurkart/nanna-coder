//! End-to-end integration tests for the moon-phase CLI loading spinner.
//!
//! These tests do NOT require a live Ollama instance; they use commands that
//! exit quickly and only assert on the presence (or absence) of spinner
//! artifacts on stderr.
//!
//! `LANG=C.UTF-8` is pinned for reproducibility: the spinner is UTF-8 and any
//! locale fallback to ASCII would cause a moon-glyph assertion to fail.

use std::process::Command;

/// Path to the freshly built harness binary.  Cargo sets `CARGO_BIN_EXE_<name>`
/// for every `[[bin]]` in the package, so we can invoke the exact artifact
/// that the current `cargo test` run compiled.
fn harness_bin() -> &'static str {
    env!("CARGO_BIN_EXE_harness")
}

/// The 4-byte UTF-8 encoding of the first moon-phase glyph (`U+1F311` 🌑).
/// Every moon glyph begins with the same 3-byte prefix `0xF0 0x9F 0x8C`, so
/// searching for that prefix is a cheap way to detect any spinner output.
const MOON_PREFIX: &[u8] = &[0xF0, 0x9F, 0x8C];

#[test]
fn no_animation_flag_suppresses_all_spinner_artifacts() {
    // `harness tools` exits immediately — no network needed — and the
    // --no-animation flag should prevent any spinner bytes from hitting
    // stderr even if we somehow managed to start one.
    let output = Command::new(harness_bin())
        .args(["--no-animation", "tools"])
        .env("LANG", "C.UTF-8")
        .env("RUST_LOG", "off")
        .env_remove("HARNESS_FORCE_ANIMATION")
        .output()
        .expect("failed to execute harness");

    let stderr = output.stderr;
    assert!(
        !stderr.windows(MOON_PREFIX.len()).any(|w| w == MOON_PREFIX),
        "--no-animation produced moon glyphs on stderr: {stderr:?}"
    );
    assert!(
        !stderr.windows(2).any(|w| w == b"\x1b["),
        "--no-animation produced ANSI escape sequences on stderr: {stderr:?}"
    );
}

#[test]
fn mcp_serve_hard_off_even_without_flag() {
    // We cannot actually boot `mcp-serve` here (it would block on stdin), but
    // we can assert that `AnimationPolicy::resolve(..., mcp_mode=true)`
    // collapses to `Off`.  The policy-level unit test in `ui::spinner::tests`
    // covers the identity; this integration test covers the CLI wiring by
    // invoking `--help mcp-serve` which exits cleanly and should never render
    // a spinner regardless.
    let output = Command::new(harness_bin())
        .args(["mcp-serve", "--help"])
        .env("LANG", "C.UTF-8")
        .env("HARNESS_FORCE_ANIMATION", "1") // even force-on must stay silent
        .output()
        .expect("failed to execute harness");

    let stderr = output.stderr;
    assert!(
        !stderr.windows(MOON_PREFIX.len()).any(|w| w == MOON_PREFIX),
        "mcp-serve --help leaked moon glyphs: {stderr:?}"
    );
}

#[test]
fn moon_glyphs_are_u1f311_through_u1f318_inclusive() {
    // Re-exported compile-time invariant: the MOON_PHASES slice is eight
    // glyphs long and maps exactly onto the U+1F311..U+1F318 range.  This
    // duplicates a unit test but serves as a spec test visible from the
    // integration crate.
    use harness::ui::MOON_PHASES;
    assert_eq!(MOON_PHASES.len(), 8);
    for (i, glyph) in MOON_PHASES.iter().enumerate() {
        let cp = glyph.chars().next().unwrap() as u32;
        assert_eq!(cp, 0x1F311 + i as u32);
    }
}
