//! Tracing writer that cooperates with [`crate::ui::spinner`].
//!
//! The default `tracing_subscriber::fmt` writer emits to stderr.  When the
//! moon-phase spinner is active, its in-progress frame lives on the last line
//! of stderr — any log record splatted in on top leaves an orphan glyph above
//! the shell prompt.
//!
//! [`SpinnerAwareMakeWriter`] fixes this by, for each log record:
//!
//! 1. Checking the shared [`spinner::spinner_active`] flag.
//! 2. If set, prepending `\r\x1b[2K` (carriage-return + erase-to-end-of-line)
//!    so the glyph is wiped before the log bytes land.
//! 3. Writing the log line itself to stderr.
//!
//! The next spinner tick will repaint the frame on the now-empty line.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing_subscriber::fmt::MakeWriter;

use super::spinner;

/// A [`MakeWriter`] that wraps stderr and knows how to erase a spinner frame
/// before each log line.
#[derive(Clone)]
pub struct SpinnerAwareMakeWriter {
    active: Arc<AtomicBool>,
}

impl SpinnerAwareMakeWriter {
    /// Construct a writer bound to the global spinner-active flag so the
    /// writer tracks whichever spinner is currently live.
    pub fn new() -> Self {
        Self {
            active: spinner::spinner_active(),
        }
    }
}

impl Default for SpinnerAwareMakeWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> MakeWriter<'a> for SpinnerAwareMakeWriter {
    type Writer = SpinnerAwareWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SpinnerAwareWriter {
            active: Arc::clone(&self.active),
            stderr: io::stderr(),
        }
    }
}

/// The actual writer used by `tracing`; holds a locked handle semantics via
/// `io::Stderr`, which serializes writes across threads.
pub struct SpinnerAwareWriter {
    active: Arc<AtomicBool>,
    stderr: io::Stderr,
}

impl Write for SpinnerAwareWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut handle = self.stderr.lock();
        if self.active.load(Ordering::Acquire) {
            // Erase the current spinner frame before the log line so they do
            // not visually collide.  The next spinner tick will repaint.
            handle.write_all(b"\r\x1b[2K")?;
        }
        handle.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stderr.lock().flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SpinnerAwareMakeWriter` is cheaply cloneable — it's a single
    /// `Arc<AtomicBool>`.  This is a smoke test to guard against accidentally
    /// pulling in non-Clone fields.
    #[test]
    fn make_writer_is_clone_and_send_and_sync() {
        fn assert_traits<T: Clone + Send + Sync>() {}
        assert_traits::<SpinnerAwareMakeWriter>();
    }

    /// Two writers minted from the same `MakeWriter` share the same active
    /// flag so all log emissions see a consistent view of whether a spinner
    /// is currently running.
    #[test]
    fn writers_share_the_global_spinner_flag() {
        let make = SpinnerAwareMakeWriter::new();
        let a = make.make_writer();
        let b = make.make_writer();
        assert!(Arc::ptr_eq(&a.active, &b.active));
    }
}
