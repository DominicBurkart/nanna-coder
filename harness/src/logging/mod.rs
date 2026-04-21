//! Structured-logging helpers with best-effort secret redaction.
//!
//! This module is a precursor to the full logging pipeline described in issue
//! #219 / PR #229. Its present job is narrow: provide a
//! [`redaction::redact_secrets`] primitive and a [`RedactingMakeWriter`]
//! `tracing_subscriber::fmt::MakeWriter` adapter so any logs emitted through
//! `init_with_redaction` pass through a redaction pipeline before hitting the
//! sink.
//!
//! # Threat model
//!
//! This is **defense in depth**, not a security boundary. Operators and
//! contributors should still assume that anything they hand to `tracing::info!`
//! can leak. In particular, this module does **not** attempt to redact:
//!
//! - structured secrets that do not match any of the supported patterns
//!   (for example, a password that happens to be `"hunter2"` with no
//!   surrounding `password=` marker),
//! - truncated or line-split tokens (split across two log events, or past a
//!   `fmt::Layer` truncation boundary),
//!   partial tokens (e.g. first 6 characters of a PAT logged on purpose as a
//!   fingerprint),
//! - secrets emitted through sinks that bypass the configured
//!   `tracing_subscriber` (direct `println!`, `eprintln!`, `std::process`
//!   output, panic messages).
//!
//! See [`redaction`] for the enumerated field-name allowlist and regex set.
pub mod redaction;

use std::io::{self, Write};
use tracing_subscriber::fmt::MakeWriter;

/// A [`MakeWriter`] adapter that buffers each write, runs the bytes through
/// [`redaction::redact_secrets`], and forwards the redacted bytes to the inner
/// writer.
///
/// This is intentionally a post-serialization redaction pass. It runs after
/// `fmt::Layer` has rendered the event (fields + message) to bytes, which
/// means:
///
/// - every field value is covered (including free-form `Display`-rendered
///   values such as `user_prompt`),
/// - the implementation does not need to thread a custom `Visit` through
///   `tracing`'s private APIs,
/// - cost is paid per-event rather than per-field.
///
/// The adapter is UTF-8 tolerant: if a buffered write is not valid UTF-8 it is
/// forwarded unmodified rather than being dropped. Redaction only operates on
/// the UTF-8 prefix / interpretation of the bytes.
#[derive(Clone, Debug)]
pub struct RedactingMakeWriter<M> {
    inner: M,
}

impl<M> RedactingMakeWriter<M> {
    /// Wrap an existing [`MakeWriter`] so that every emitted event is run
    /// through [`redaction::redact_secrets`] before it reaches the inner
    /// writer.
    pub fn new(inner: M) -> Self {
        Self { inner }
    }
}

/// Per-write handle returned by [`RedactingMakeWriter::make_writer`].
pub struct RedactingWriter<W> {
    inner: W,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match std::str::from_utf8(buf) {
            Ok(s) => {
                let redacted = redaction::redact_secrets(s);
                self.inner.write_all(redacted.as_bytes())?;
                Ok(buf.len())
            }
            Err(_) => self.inner.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<'a, M> MakeWriter<'a> for RedactingMakeWriter<M>
where
    M: MakeWriter<'a>,
{
    type Writer = RedactingWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            inner: self.inner.make_writer(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::info;
    use tracing_subscriber::fmt::MakeWriter;

    /// Thread-safe `MakeWriter` that captures everything written into a
    /// shared `Vec<u8>`, used by the unit tests below to assert on the final
    /// serialized log bytes.
    #[derive(Clone)]
    struct VecSink(Arc<Mutex<Vec<u8>>>);

    impl VecSink {
        fn new() -> (Self, Arc<Mutex<Vec<u8>>>) {
            let buf = Arc::new(Mutex::new(Vec::new()));
            (Self(buf.clone()), buf)
        }
    }

    struct VecSinkHandle(Arc<Mutex<Vec<u8>>>);

    impl Write for VecSinkHandle {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for VecSink {
        type Writer = VecSinkHandle;
        fn make_writer(&'a self) -> Self::Writer {
            VecSinkHandle(self.0.clone())
        }
    }

    fn with_subscriber<F: FnOnce()>(sink: VecSink, f: F) {
        let redacting = RedactingMakeWriter::new(sink);
        let subscriber = tracing_subscriber::fmt()
            .with_writer(redacting)
            .with_ansi(false)
            .with_target(false)
            .with_level(false)
            .without_time()
            .finish();
        tracing::subscriber::with_default(subscriber, f);
    }

    fn captured(buf: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(buf.lock().unwrap().clone()).expect("utf8 log bytes")
    }

    #[test]
    fn redacts_openai_style_key_in_message() {
        let (sink, buf) = VecSink::new();
        let secret = "sk-ABCDEFabcdef0123456789TOPSECRETxyz";
        with_subscriber(sink, || {
            info!(message = %format!("oops {secret}"));
        });
        let out = captured(&buf);
        assert!(!out.contains(secret), "expected redaction, got: {out}");
        assert!(out.contains("***REDACTED***"), "expected marker: {out}");
    }

    #[test]
    fn redacts_bearer_token() {
        let (sink, buf) = VecSink::new();
        let token = "Bearer abc.def-ghi_jkl0123456789";
        with_subscriber(sink, || {
            info!(message = %format!("auth: {token}"));
        });
        let out = captured(&buf);
        assert!(!out.contains(token), "bearer not redacted: {out}");
        assert!(out.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_jwt_like_string() {
        let (sink, buf) = VecSink::new();
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        with_subscriber(sink, || {
            info!(message = %format!("token={jwt}"));
        });
        let out = captured(&buf);
        assert!(!out.contains(jwt), "jwt not redacted: {out}");
        assert!(out.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_long_hex_string() {
        let (sink, buf) = VecSink::new();
        let hex = "deadbeefcafebabe0123456789abcdef0123456789abcdef";
        with_subscriber(sink, || {
            info!(message = %format!("hash={hex}"));
        });
        let out = captured(&buf);
        assert!(!out.contains(hex), "hex not redacted: {out}");
        assert!(out.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_field_named_password() {
        let (sink, buf) = VecSink::new();
        let pw = "hunter2NotReallyASecret";
        with_subscriber(sink, || {
            info!(password = pw, "login attempt");
        });
        let out = captured(&buf);
        assert!(!out.contains(pw), "password field not redacted: {out}");
        assert!(out.contains("***REDACTED***"));
    }

    #[test]
    fn redacts_field_named_api_key() {
        let (sink, buf) = VecSink::new();
        let key = "plainbutsensitivevalue-XYZ";
        with_subscriber(sink, || {
            info!(api_key = key, "call");
        });
        let out = captured(&buf);
        assert!(!out.contains(key), "api_key not redacted: {out}");
    }

    #[test]
    fn leaves_non_secret_strings_untouched() {
        let (sink, buf) = VecSink::new();
        with_subscriber(sink, || {
            info!(message = "hello world, short string, no secrets");
        });
        let out = captured(&buf);
        assert!(out.contains("hello world"));
        assert!(!out.contains("***REDACTED***"));
    }
}
