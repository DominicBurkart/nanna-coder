//! End-to-end integration tests for the moon-phase CLI loading spinner.
//!
//! These tests do NOT require a live Ollama instance; they use commands that
//! exit quickly and only assert on the presence (or absence) of spinner
//! artifacts on stderr.
//!
//! `LANG=C.UTF-8` is pinned for reproducibility: the spinner is UTF-8 and any
//! locale fallback to ASCII would cause a moon-glyph assertion to fail.

use std::io::Write;
use std::process::{Command, Stdio};

/// Path to the freshly built harness binary.
fn harness_bin() -> &'static str {
    env!("CARGO_BIN_EXE_harness")
}

/// The first three bytes of every moon-phase glyph's UTF-8 encoding.
const MOON_PREFIX: &[u8] = &[0xF0, 0x9F, 0x8C];

#[test]
fn no_animation_flag_suppresses_all_spinner_artifacts() {
    // `harness tools` exits immediately — no network needed — and the
    // --no-animation flag should prevent any spinner bytes on stderr.
    let output = Command::new(harness_bin())
        .args(["--no-animation", "tools"])
        .env("LANG", "C.UTF-8")
        .env("RUST_LOG", "off")
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
fn mcp_serve_produces_no_spinner_bytes_on_stderr() {
    // Owner feedback (PR #228): a test that only checks `mcp-serve --help` is
    // meaningless because clap short-circuits before policy resolution.
    // Instead, actually boot `mcp-serve` with a closed stdin (EOF on read)
    // so the server loop exits immediately, and assert stderr is moon-free.
    //
    // Capturing stderr makes `is_terminal()` false, which by itself already
    // forces policy=Off — but that's also the real-world behavior when a
    // parent MCP client captures the child's streams.
    let mut child = Command::new(harness_bin())
        .args(["mcp-serve"])
        .env("LANG", "C.UTF-8")
        .env("RUST_LOG", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn mcp-serve");

    // Close stdin so the child's stdin read returns EOF and the loop exits.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"");
    }

    let output = child
        .wait_with_output()
        .expect("failed to wait on mcp-serve");
    let stderr = output.stderr;
    assert!(
        !stderr.windows(MOON_PREFIX.len()).any(|w| w == MOON_PREFIX),
        "mcp-serve leaked moon glyphs on stderr: {stderr:?}"
    );
}
