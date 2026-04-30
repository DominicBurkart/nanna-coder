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
//! Note: PII (email addresses, phone numbers, IP addresses, SSNs, and credit-
//! card PANs) is **out of scope** for this module. The redaction pass targets
//! credential shapes only.
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
/// The adapter is UTF-8 tolerant: if a flushed buffer is not valid UTF-8 the
/// bytes are interpreted with `from_utf8_lossy` so that redaction still runs
/// on the lossy string before being forwarded to the inner writer.
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
///
/// Bytes are accumulated in an internal buffer. Redaction runs and the buffer
/// is flushed to the inner writer whenever a newline (`\n`) is seen in the
/// incoming data, or when [`flush`](Write::flush) is called explicitly. This
/// ensures that a secret split across two `write` calls — which the
/// `MakeWriter` contract permits — is still fully redacted.
pub struct RedactingWriter<W> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: Write> Write for RedactingWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.contains(&b'\n') {
            self.flush()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let chunk = std::mem::take(&mut self.buf);
            // Use `from_utf8_lossy` so the redaction pass always runs, even
            // when the buffer contains invalid UTF-8 bytes. This is strictly
            // safer than forwarding verbatim: a secret prefixed with a single
            // invalid byte would otherwise bypass redaction entirely.
            let s = String::from_utf8_lossy(&chunk);
            let redacted = redaction::redact_secrets(&s);
            self.inner.write_all(redacted.as_bytes())?;
        }
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
            buf: Vec::new(),
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

    /// Asserts that bytes containing invalid UTF-8 are processed via
    /// `from_utf8_lossy` so the redaction pass still runs. A secret
    /// prefixed with an invalid byte must not bypass redaction.
    #[test]
    fn non_utf8_bytes_redacted_via_lossy() {
        let inner: Vec<u8> = Vec::new();
        let mut writer = RedactingWriter {
            inner,
            buf: Vec::new(),
        };
        // Prepend an invalid UTF-8 byte before a known secret pattern.
        // The lossy conversion replaces 0xFF with U+FFFD; the `sk-` key that
        // follows must still be redacted.
        let secret = b"sk-ABCDEFabcdef0123456789TOPSECRETxyz";
        let mut bad_bytes: Vec<u8> = vec![0xFF];
        bad_bytes.extend_from_slice(secret);
        bad_bytes.push(b'\n');
        writer.write_all(&bad_bytes).expect("write should succeed");
        let output = String::from_utf8_lossy(&writer.inner).into_owned();
        assert!(
            !output.contains("sk-ABCDEFabcdef0123456789TOPSECRETxyz"),
            "secret must be redacted even after lossy UTF-8 conversion; got: {output}"
        );
        assert!(
            output.contains("***REDACTED***"),
            "expected REDACTED marker; got: {output}"
        );
    }

    /// Asserts that a secret split across two `write` calls is still fully
    /// redacted. The `MakeWriter` contract does not guarantee a single `write`
    /// per event; this test exercises the buffering logic.
    #[test]
    fn split_write_redacts_secret() {
        let inner: Vec<u8> = Vec::new();
        let mut writer = RedactingWriter {
            inner,
            buf: Vec::new(),
        };
        // Split a known `sk-` secret across two writes; the newline (and
        // therefore the flush+redact) only arrives with the second write.
        let first_half = b"oops sk-ABCDEFabcdef012345";
        let second_half = b"6789TOPSECRETxyz\n";
        writer.write_all(first_half).expect("first write");
        // Buffer should still be held; nothing flushed yet.
        assert!(
            writer.inner.is_empty(),
            "inner writer must be empty before newline"
        );
        writer.write_all(second_half).expect("second write");
        let output = String::from_utf8_lossy(&writer.inner).into_owned();
        assert!(
            !output.contains("sk-ABCDEFabcdef0123456789TOPSECRETxyz"),
            "split secret must be redacted; got: {output}"
        );
        assert!(
            output.contains("***REDACTED***"),
            "expected REDACTED marker; got: {output}"
        );
    }

    /// Exercises `RedactingWriter::flush` to ensure the delegation to the
    /// inner writer is covered.
    #[test]
    fn flush_delegates_to_inner() {
        let inner: Vec<u8> = Vec::new();
        let mut writer = RedactingWriter {
            inner,
            buf: Vec::new(),
        };
        writer.flush().expect("flush should succeed");
    }
}
