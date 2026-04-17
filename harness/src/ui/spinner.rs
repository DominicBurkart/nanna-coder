//! Moon-phase loading animation for the harness CLI.
//!
//! The spinner cycles through the eight Unicode moon-phase emoji
//! (U+1F311..U+1F318) once every 8 * tick = 1 s and renders to stderr while a
//! long-running future is awaited.  A [`SpinnerGuard`] returned from
//! [`MoonSpinner::start`] owns the background tick task and clears the line on
//! drop, guaranteeing the cursor is restored even on panic or early return.
//!
//! ## Coordination with `tracing`
//!
//! A globally-shared [`Arc<AtomicBool>`] tracks whether a spinner frame is
//! currently visible on stderr.  The [`crate::ui::log_writer`] module reads
//! that flag and erases the active frame before each log line, so structured
//! log output does not interleave with spinner frames.
//!
//! ## Test determinism
//!
//! The render pipeline is split in two halves:
//!
//! - [`next_glyph`] is a pure function from `counter -> &'static str`; it is
//!   trivially unit-testable.
//! - [`MoonSpinner::render_once`] writes exactly one frame to an arbitrary
//!   [`tokio::io::AsyncWrite`] implementor, so golden byte tests can feed an
//!   in-memory buffer and assert on the exact byte sequence without timers.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// The eight Unicode moon-phase glyphs rendered by the spinner, in order.
///
/// Must match `U+1F311..U+1F318` inclusive.
pub const MOON_PHASES: [&str; 8] = [
    "\u{1F311}", // 🌑 new moon
    "\u{1F312}", // 🌒 waxing crescent
    "\u{1F313}", // 🌓 first quarter
    "\u{1F314}", // 🌔 waxing gibbous
    "\u{1F315}", // 🌕 full moon
    "\u{1F316}", // 🌖 waning gibbous
    "\u{1F317}", // 🌗 last quarter
    "\u{1F318}", // 🌘 waning crescent
];

/// Default tick interval between frames.
pub const TICK_INTERVAL: Duration = Duration::from_millis(125);

/// Controls whether the spinner actually renders.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AnimationPolicy {
    /// Detect at runtime: render only if stderr is a TTY and the environment
    /// does not disable animation (`NO_COLOR`, `TERM=dumb`, `--no-animation`).
    #[default]
    Auto,
    /// Never render — spinner [`start`](MoonSpinner::start) returns a no-op
    /// guard.  Used for `mcp-serve`, non-interactive pipelines, and when
    /// `--no-animation` is passed.
    Off,
    /// Always render regardless of TTY status.  Used by integration tests via
    /// `HARNESS_FORCE_ANIMATION=1`; suppresses cursor-toggle escapes when
    /// stderr is not a TTY so captured bytes stay clean.
    ForceOn,
}

impl AnimationPolicy {
    /// Resolve the effective policy from environment and CLI flags.
    ///
    /// Precedence (highest first):
    /// 1. `HARNESS_FORCE_ANIMATION=1` → [`AnimationPolicy::ForceOn`].
    /// 2. Explicit `--no-animation`, `NO_COLOR`, `TERM=dumb`, or piped stderr
    ///    → [`AnimationPolicy::Off`].
    /// 3. Otherwise → [`AnimationPolicy::Auto`].
    ///
    /// `mcp_mode` forces [`AnimationPolicy::Off`] regardless of any force
    /// flag, because the parent may capture stderr as well as stdout.
    pub fn resolve(no_animation: bool, mcp_mode: bool) -> Self {
        if mcp_mode {
            return AnimationPolicy::Off;
        }
        if std::env::var_os("HARNESS_FORCE_ANIMATION").as_deref() == Some("1".as_ref()) {
            return AnimationPolicy::ForceOn;
        }
        if no_animation {
            return AnimationPolicy::Off;
        }
        if std::env::var_os("TERM").as_deref() == Some("dumb".as_ref()) {
            return AnimationPolicy::Off;
        }
        if std::env::var_os("NO_COLOR").is_some() {
            return AnimationPolicy::Off;
        }
        if !std::io::stderr().is_terminal() {
            return AnimationPolicy::Off;
        }
        AnimationPolicy::Auto
    }

    /// True if this policy should produce visible frames.
    pub fn is_active(self) -> bool {
        matches!(self, AnimationPolicy::Auto | AnimationPolicy::ForceOn)
    }
}

/// Pure function used by the tick loop and by unit tests: maps a monotonic
/// counter to a glyph from [`MOON_PHASES`] with wrap-around at 8.
#[inline]
pub fn next_glyph(counter: usize) -> &'static str {
    MOON_PHASES[counter % MOON_PHASES.len()]
}

/// Globally-visible flag: `true` while a spinner frame is visible on stderr.
///
/// [`crate::ui::log_writer::SpinnerAwareMakeWriter`] reads this to decide
/// whether to erase the current line before emitting a log record.
fn spinner_active_flag() -> &'static Arc<AtomicBool> {
    use std::sync::OnceLock;
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

/// Read-only handle to the spinner-active flag for downstream consumers
/// (namely the log writer).
pub fn spinner_active() -> Arc<AtomicBool> {
    Arc::clone(spinner_active_flag())
}

/// A running moon-phase spinner.  Held internally by [`SpinnerGuard`]; drop
/// the guard to stop the spinner.
pub struct MoonSpinner {
    handle: Option<JoinHandle<()>>,
    stop_tx: Option<oneshot::Sender<()>>,
    flag: Arc<AtomicBool>,
}

impl MoonSpinner {
    /// Spawn a spinner tied to the current Tokio runtime and return an RAII
    /// guard.  If `policy` resolves to [`AnimationPolicy::Off`] or no runtime
    /// is available, returns a no-op guard.
    pub fn start(label: impl Into<String>, policy: AnimationPolicy) -> SpinnerGuard {
        if !policy.is_active() {
            return SpinnerGuard { inner: None };
        }

        // Guard against nested spinners: only one frame may be live at a time.
        let flag = Arc::clone(spinner_active_flag());
        if flag.swap(true, Ordering::AcqRel) {
            return SpinnerGuard { inner: None };
        }

        let label = label.into();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let flag_clone = Arc::clone(&flag);

        // Suppress cursor-toggle escapes when stderr is not a TTY even under
        // ForceOn, so captured test output does not get polluted.
        let emit_cursor_escapes =
            policy == AnimationPolicy::Auto || std::io::stderr().is_terminal();

        let spawn_result = tokio::runtime::Handle::try_current().map(|rt| {
            rt.spawn(async move {
                let mut stderr = tokio::io::stderr();
                let mut interval = tokio::time::interval(TICK_INTERVAL);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let counter = AtomicUsize::new(0);

                if emit_cursor_escapes {
                    // Hide the cursor while animating.
                    let _ = stderr.write_all(b"\x1b[?25l").await;
                    let _ = stderr.flush().await;
                }

                tokio::pin!(stop_rx);
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut stop_rx => break,
                        _ = interval.tick() => {
                            let c = counter.fetch_add(1, Ordering::Relaxed);
                            let _ = render_once(&mut stderr, &label, c).await;
                        }
                    }
                }

                // Clear frame and restore cursor.
                let _ = stderr.write_all(b"\r\x1b[2K").await;
                if emit_cursor_escapes {
                    let _ = stderr.write_all(b"\x1b[?25h").await;
                }
                let _ = stderr.flush().await;
                flag_clone.store(false, Ordering::Release);
            })
        });

        match spawn_result {
            Ok(handle) => SpinnerGuard {
                inner: Some(MoonSpinner {
                    handle: Some(handle),
                    stop_tx: Some(stop_tx),
                    flag,
                }),
            },
            Err(_) => {
                // No current runtime — nothing to do.  Release the flag we
                // just acquired.
                flag.store(false, Ordering::Release);
                SpinnerGuard { inner: None }
            }
        }
    }
}

/// Render exactly one frame `\r{glyph} {label}\x1b[K` to the given writer.
///
/// Exposed at the crate level so golden byte tests can drive it with an
/// in-memory buffer; see `tests` below.
pub async fn render_once<W>(writer: &mut W, label: &str, counter: usize) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let glyph = next_glyph(counter);
    writer.write_all(b"\r").await?;
    writer.write_all(glyph.as_bytes()).await?;
    writer.write_all(b" ").await?;
    writer.write_all(label.as_bytes()).await?;
    // Erase to end-of-line so shorter labels don't leave residue.
    writer.write_all(b"\x1b[K").await?;
    writer.flush().await?;
    Ok(())
}

/// RAII guard owning a running [`MoonSpinner`].  Dropping the guard signals
/// the tick loop to stop and clears the spinner frame.
pub struct SpinnerGuard {
    inner: Option<MoonSpinner>,
}

impl SpinnerGuard {
    /// A guard that does nothing on drop.  Equivalent to starting with
    /// [`AnimationPolicy::Off`].
    pub fn noop() -> Self {
        SpinnerGuard { inner: None }
    }

    /// True if this guard is actively driving a spinner task.
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }
}

impl Drop for SpinnerGuard {
    fn drop(&mut self) {
        if let Some(mut inner) = self.inner.take() {
            if let Some(tx) = inner.stop_tx.take() {
                let _ = tx.send(());
            }
            if let Some(handle) = inner.handle.take() {
                // Best-effort wait for the tick task to finish clearing the
                // line.  Abort if it is stuck so Drop never blocks forever.
                let rt = tokio::runtime::Handle::try_current();
                if let Ok(rt) = rt {
                    // Block briefly to let the clear complete.  We cannot
                    // await in Drop, so spawn a detached task that aborts
                    // after a short timeout if the clear never lands.  The
                    // JoinHandle returned by `spawn` is intentionally dropped
                    // — we do not need to observe the detached task.
                    drop(rt.spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_millis(500), handle).await;
                    }));
                } else {
                    handle.abort();
                }
            }
            inner.flag.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_sequence_is_1f311_through_1f318_in_order() {
        let expected: Vec<u32> = (0x1F311u32..=0x1F318u32).collect();
        let actual: Vec<u32> = MOON_PHASES
            .iter()
            .map(|g| g.chars().next().unwrap() as u32)
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn each_glyph_is_exactly_four_utf8_bytes() {
        for glyph in MOON_PHASES {
            assert_eq!(glyph.len(), 4, "{glyph:?} not 4 UTF-8 bytes");
        }
    }

    #[test]
    fn cycle_wraps_after_eight_ticks() {
        assert_eq!(next_glyph(0), MOON_PHASES[0]);
        assert_eq!(next_glyph(7), MOON_PHASES[7]);
        assert_eq!(next_glyph(8), MOON_PHASES[0]);
        assert_eq!(next_glyph(9), MOON_PHASES[1]);
        assert_eq!(next_glyph(16), MOON_PHASES[0]);
        // Many cycles in.
        assert_eq!(next_glyph(8_000_003), MOON_PHASES[3]);
    }

    #[test]
    fn policy_defaults_to_auto() {
        assert_eq!(AnimationPolicy::default(), AnimationPolicy::Auto);
    }

    #[test]
    fn policy_mcp_mode_forces_off() {
        // mcp_mode=true is a hard-off even if no_animation=false.
        assert_eq!(AnimationPolicy::resolve(false, true), AnimationPolicy::Off);
    }

    #[test]
    fn policy_no_animation_is_off() {
        // Clearing HARNESS_FORCE_ANIMATION makes the test deterministic.
        let _saved = EnvGuard::clear("HARNESS_FORCE_ANIMATION");
        assert_eq!(AnimationPolicy::resolve(true, false), AnimationPolicy::Off);
    }

    #[test]
    fn policy_is_active_matches_auto_and_force_on() {
        assert!(AnimationPolicy::Auto.is_active());
        assert!(AnimationPolicy::ForceOn.is_active());
        assert!(!AnimationPolicy::Off.is_active());
    }

    // Golden byte test — deterministic because render_once is a pure function
    // over (writer, label, counter).  No timer involved.
    #[tokio::test]
    async fn render_once_emits_exact_bytes_for_one_cycle() {
        let label = "calling llama3.1:8b";
        let mut buf: Vec<u8> = Vec::new();
        for c in 0..MOON_PHASES.len() {
            render_once(&mut buf, label, c).await.unwrap();
        }

        // Build the expected byte sequence from the same primitives; this
        // catches reorderings and accidental ANSI-escape mutations.
        let mut expected: Vec<u8> = Vec::new();
        for glyph in MOON_PHASES {
            expected.extend_from_slice(b"\r");
            expected.extend_from_slice(glyph.as_bytes());
            expected.extend_from_slice(b" ");
            expected.extend_from_slice(label.as_bytes());
            expected.extend_from_slice(b"\x1b[K");
        }
        assert_eq!(buf, expected);

        // Sanity: every glyph's 4 bytes should appear somewhere.
        for glyph in MOON_PHASES {
            assert!(
                buf.windows(4).any(|w| w == glyph.as_bytes()),
                "glyph {glyph:?} missing from golden buffer"
            );
        }
    }

    #[tokio::test]
    async fn spinner_guard_noop_when_policy_off() {
        let guard = MoonSpinner::start("x", AnimationPolicy::Off);
        assert!(!guard.is_active());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn spinner_guard_clears_flag_on_drop() {
        // Confirm the shared active flag is released after the guard drops,
        // so nested starts can later succeed.
        assert!(!spinner_active().load(Ordering::Acquire));

        let guard = MoonSpinner::start("work", AnimationPolicy::ForceOn);
        // Advance virtual time past a few ticks so the background task has
        // rendered at least one frame before we stop it.
        tokio::time::advance(TICK_INTERVAL * 3).await;
        drop(guard);

        // Yield so the drop-spawned timeout task can run.
        for _ in 0..4 {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(10)).await;
        }
        // The flag must be cleared so a fresh spinner can acquire it again.
        assert!(!spinner_active().load(Ordering::Acquire));
    }

    // Helper: scoped env-var mutation.  Cargo test threads share process env,
    // so this is intentionally narrow: set on construction, restore on drop.
    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn clear(key: &'static str) -> Self {
            let prev = std::env::var_os(key);
            // SAFETY: tests in this file are not run concurrently with code
            // reading these vars except via AnimationPolicy::resolve, which
            // only reads during the single test under test.
            unsafe {
                std::env::remove_var(key);
            }
            EnvGuard { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: see EnvGuard::clear.
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }
}
