//! Moon-phase loading animation for the harness CLI.
//!
//! Cycles through the eight Unicode moon-phase glyphs (U+1F311..U+1F318) at
//! 125 ms per tick and renders to stderr while a long-running future is
//! awaited.  A [`SpinnerGuard`] returned from [`MoonSpinner::start`] owns the
//! background tick task and synchronously restores the cursor on drop so the
//! terminal is never left with the cursor hidden, even on panic/shutdown.

use std::io::{IsTerminal, Write as _};
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
    "\u{1F311}", // new moon
    "\u{1F312}", // waxing crescent
    "\u{1F313}", // first quarter
    "\u{1F314}", // waxing gibbous
    "\u{1F315}", // full moon
    "\u{1F316}", // waning gibbous
    "\u{1F317}", // last quarter
    "\u{1F318}", // waning crescent
];

/// Default tick interval between frames.
pub const TICK_INTERVAL: Duration = Duration::from_millis(125);

/// Bytes written synchronously from `Drop` to guarantee the terminal is
/// restored: carriage return, erase-to-end-of-line, show cursor.
const CURSOR_RESTORE: &[u8] = b"\r\x1b[2K\x1b[?25h";

/// Controls whether the spinner actually renders.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AnimationPolicy {
    /// Render frames to stderr.
    #[default]
    On,
    /// Never render — [`MoonSpinner::start`] returns a no-op guard.
    Off,
}

/// A minimal indirection over the environment.  The production impl reads
/// from `std::env`; tests inject a closed map so we never mutate process env.
pub trait EnvReader {
    fn get(&self, key: &str) -> Option<String>;
}

/// Default reader: queries `std::env::var_os` and converts to `String`.
pub struct SystemEnv;

impl EnvReader for SystemEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var_os(key).and_then(|v| v.into_string().ok())
    }
}

impl AnimationPolicy {
    /// Resolve the effective policy from environment and CLI flags using the
    /// real system environment.  Thin wrapper over [`Self::resolve_with`].
    pub fn resolve(no_animation: bool, mcp_mode: bool) -> Self {
        Self::resolve_with(
            no_animation,
            mcp_mode,
            &SystemEnv,
            std::io::stderr().is_terminal(),
        )
    }

    /// Pure function from inputs to policy.  Used by tests with an injected
    /// [`EnvReader`] so we never touch process env from the test suite.
    ///
    /// Precedence (highest first):
    /// 1. `mcp_mode == true` → [`Self::Off`] (parent may capture stderr).
    /// 2. Explicit `--no-animation` → [`Self::Off`].
    /// 3. `TERM=dumb` or `NO_COLOR=*` → [`Self::Off`].
    /// 4. stderr is not a TTY → [`Self::Off`].
    /// 5. Otherwise → [`Self::On`].
    pub fn resolve_with<E: EnvReader>(
        no_animation: bool,
        mcp_mode: bool,
        env: &E,
        stderr_is_tty: bool,
    ) -> Self {
        if mcp_mode || no_animation {
            return AnimationPolicy::Off;
        }
        if env.get("TERM").as_deref() == Some("dumb") || env.get("NO_COLOR").is_some() {
            return AnimationPolicy::Off;
        }
        if !stderr_is_tty {
            return AnimationPolicy::Off;
        }
        AnimationPolicy::On
    }

    /// True if this policy should produce visible frames.
    pub fn is_active(self) -> bool {
        matches!(self, AnimationPolicy::On)
    }
}

/// Pure function: maps a monotonic counter to a glyph with wrap-around at 8.
#[inline]
pub fn next_glyph(counter: usize) -> &'static str {
    MOON_PHASES[counter % MOON_PHASES.len()]
}

/// Globally-visible flag: `true` while a spinner frame is visible on stderr.
///
/// The log writer reads this to decide whether to erase the current line
/// before emitting a log record.
fn spinner_active_flag() -> &'static Arc<AtomicBool> {
    use std::sync::OnceLock;
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
}

/// Read-only handle to the spinner-active flag.
pub fn spinner_active() -> Arc<AtomicBool> {
    Arc::clone(spinner_active_flag())
}

/// A running moon-phase spinner.  Held internally by [`SpinnerGuard`]; drop
/// the guard to stop the spinner and synchronously restore the cursor.
pub struct MoonSpinner {
    handle: Option<JoinHandle<()>>,
    stop_tx: Option<oneshot::Sender<()>>,
    flag: Arc<AtomicBool>,
}

impl MoonSpinner {
    /// Spawn a spinner tied to the current Tokio runtime and return an RAII
    /// guard.  If `policy` is [`AnimationPolicy::Off`] or no runtime is
    /// available, returns a no-op guard.
    pub fn start(label: impl Into<String>, policy: AnimationPolicy) -> SpinnerGuard {
        if !policy.is_active() {
            return SpinnerGuard { inner: None };
        }

        let flag = Arc::clone(spinner_active_flag());
        flag.store(true, Ordering::Release);

        let label = label.into();
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let flag_clone = Arc::clone(&flag);

        let spawn_result = tokio::runtime::Handle::try_current().map(|rt| {
            rt.spawn(async move {
                let mut stderr = tokio::io::stderr();
                let mut interval = tokio::time::interval(TICK_INTERVAL);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let counter = AtomicUsize::new(0);

                // Hide the cursor while animating.
                let _ = stderr.write_all(b"\x1b[?25l").await;
                let _ = stderr.flush().await;

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

                // Async cleanup best-effort: Drop also writes CURSOR_RESTORE
                // synchronously so we are covered even if this never runs.
                let _ = stderr.write_all(CURSOR_RESTORE).await;
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
                flag.store(false, Ordering::Release);
                SpinnerGuard { inner: None }
            }
        }
    }
}

/// Render exactly one frame as a single atomic `write_all` + `flush`.
///
/// All frame bytes (`\r`, glyph, space, label, `\x1b[K`) are concatenated into
/// a single `Vec<u8>` and written in one syscall so a concurrent tracing event
/// cannot interleave mid-frame.
pub async fn render_once<W>(writer: &mut W, label: &str, counter: usize) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let glyph = next_glyph(counter);
    let mut frame = Vec::with_capacity(1 + glyph.len() + 1 + label.len() + 3);
    frame.push(b'\r');
    frame.extend_from_slice(glyph.as_bytes());
    frame.push(b' ');
    frame.extend_from_slice(label.as_bytes());
    frame.extend_from_slice(b"\x1b[K");
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

/// Synchronously restore the cursor on the real stderr.  Called from `Drop`
/// so even if the async tick task never runs (e.g. runtime is mid-shutdown)
/// the user's terminal does not keep `\x1b[?25l` hiding the cursor.
fn sync_restore_cursor() {
    let stderr = std::io::stderr();
    let mut handle = stderr.lock();
    let _ = handle.write_all(CURSOR_RESTORE);
    let _ = handle.flush();
}

/// RAII guard owning a running [`MoonSpinner`].  Dropping the guard signals
/// the tick loop to stop, synchronously restores the cursor, and clears the
/// shared active flag.
pub struct SpinnerGuard {
    inner: Option<MoonSpinner>,
}

impl SpinnerGuard {
    /// A guard that does nothing on drop.
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
                // Best-effort: abort the tick task so it does not keep
                // emitting frames after this guard is gone.  We do NOT rely on
                // its async cleanup; the sync path below handles restoration.
                handle.abort();
            }
            // Clear the shared flag FIRST so the log writer stops erasing.
            inner.flag.store(false, Ordering::Release);
            // Synchronously restore the cursor.  This always runs, even if
            // the runtime is mid-shutdown and no task can be spawned.
            sync_restore_cursor();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic test-only [`EnvReader`] backed by a fixed map.
    struct MapEnv<'a>(&'a [(&'a str, &'a str)]);

    impl<'a> EnvReader for MapEnv<'a> {
        fn get(&self, key: &str) -> Option<String> {
            self.0
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

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
    fn cycle_wraps_after_eight_ticks() {
        assert_eq!(next_glyph(0), MOON_PHASES[0]);
        assert_eq!(next_glyph(7), MOON_PHASES[7]);
        assert_eq!(next_glyph(8), MOON_PHASES[0]);
        assert_eq!(next_glyph(16), MOON_PHASES[0]);
    }

    #[test]
    fn policy_defaults_to_on() {
        assert_eq!(AnimationPolicy::default(), AnimationPolicy::On);
    }

    #[test]
    fn policy_mcp_mode_forces_off() {
        let env = MapEnv(&[]);
        assert_eq!(
            AnimationPolicy::resolve_with(false, true, &env, true),
            AnimationPolicy::Off
        );
    }

    #[test]
    fn policy_no_animation_is_off() {
        let env = MapEnv(&[]);
        assert_eq!(
            AnimationPolicy::resolve_with(true, false, &env, true),
            AnimationPolicy::Off
        );
    }

    #[test]
    fn policy_term_dumb_is_off() {
        let env = MapEnv(&[("TERM", "dumb")]);
        assert_eq!(
            AnimationPolicy::resolve_with(false, false, &env, true),
            AnimationPolicy::Off
        );
    }

    #[test]
    fn policy_no_color_is_off() {
        let env = MapEnv(&[("NO_COLOR", "1")]);
        assert_eq!(
            AnimationPolicy::resolve_with(false, false, &env, true),
            AnimationPolicy::Off
        );
    }

    #[test]
    fn policy_piped_stderr_is_off() {
        let env = MapEnv(&[]);
        assert_eq!(
            AnimationPolicy::resolve_with(false, false, &env, false),
            AnimationPolicy::Off
        );
    }

    #[test]
    fn policy_tty_no_env_is_on() {
        let env = MapEnv(&[]);
        assert_eq!(
            AnimationPolicy::resolve_with(false, false, &env, true),
            AnimationPolicy::On
        );
    }

    #[test]
    fn policy_is_active_matches_on_only() {
        assert!(AnimationPolicy::On.is_active());
        assert!(!AnimationPolicy::Off.is_active());
    }

    #[test]
    fn system_env_reader_reads_from_process_env() {
        // PATH is reliably set in every sane test environment.
        let sys = SystemEnv;
        assert!(sys.get("PATH").is_some());
        assert!(sys.get("DEFINITELY_NOT_A_REAL_ENV_VAR_12345").is_none());
    }

    #[tokio::test]
    async fn spinner_guard_noop_when_policy_off() {
        let guard = MoonSpinner::start("x", AnimationPolicy::Off);
        assert!(!guard.is_active());
    }

    #[tokio::test]
    async fn render_once_writes_single_atomic_frame() {
        // Verify render_once produces ONE write_all-able blob containing all
        // frame bytes in order: \r, glyph, space, label, \x1b[K.
        let mut buf: Vec<u8> = Vec::new();
        render_once(&mut buf, "loading", 0).await.unwrap();

        let mut expected = Vec::new();
        expected.push(b'\r');
        expected.extend_from_slice(MOON_PHASES[0].as_bytes());
        expected.push(b' ');
        expected.extend_from_slice(b"loading");
        expected.extend_from_slice(b"\x1b[K");
        assert_eq!(buf, expected);
    }

    #[tokio::test]
    async fn render_once_wraps_counter_across_cycle() {
        // Counter 8 should wrap back to glyph 0.
        let mut a = Vec::new();
        let mut b = Vec::new();
        render_once(&mut a, "x", 0).await.unwrap();
        render_once(&mut b, "x", 8).await.unwrap();
        assert_eq!(a, b);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn spinner_guard_clears_flag_on_drop() {
        assert!(!spinner_active().load(Ordering::Acquire));

        let guard = MoonSpinner::start("work", AnimationPolicy::On);
        tokio::time::advance(TICK_INTERVAL * 3).await;
        drop(guard);

        // The flag is cleared synchronously from Drop, not via the async task.
        assert!(!spinner_active().load(Ordering::Acquire));
    }

    #[test]
    fn sync_restore_cursor_bytes_include_show_cursor_escape() {
        // Sanity: the bytes we write synchronously must include the show-
        // cursor escape so Drop can never leave the terminal with the cursor
        // hidden, even when the async cleanup never runs.
        assert!(CURSOR_RESTORE.windows(5).any(|w| w == b"\x1b[?25"));
        assert_eq!(CURSOR_RESTORE, b"\r\x1b[2K\x1b[?25h");
    }

    #[test]
    fn drop_without_runtime_does_not_panic() {
        // SpinnerGuard::noop() drops cleanly with no runtime attached.
        let g = SpinnerGuard::noop();
        assert!(!g.is_active());
        drop(g);
    }
}
