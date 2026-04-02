use harness::auth::{validate_bind_address, AuthToken, RateLimiter, TokenStore};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn test_token_not_in_debug_output() {
    let token = AuthToken::generate();
    let debug_str = format!("{:?}", token);
    assert!(
        debug_str.contains("REDACTED"),
        "Debug output should contain REDACTED"
    );
    assert!(
        !debug_str.contains(token.as_str()),
        "Debug output must not contain the actual token value"
    );
}

#[test]
fn test_bind_address_validation() {
    // Loopback IPv4 should be accepted
    let addr = "127.0.0.1:3000".parse().unwrap();
    assert!(validate_bind_address(&addr).is_ok());

    // Loopback IPv6 should be accepted
    let addr = "[::1]:3000".parse().unwrap();
    assert!(validate_bind_address(&addr).is_ok());

    // Wildcard should be rejected
    let addr = "0.0.0.0:3000".parse().unwrap();
    assert!(validate_bind_address(&addr).is_err());

    // Public IP should be rejected
    let addr = "192.168.1.1:3000".parse().unwrap();
    assert!(validate_bind_address(&addr).is_err());

    // Public IPv6 should be rejected
    let addr = "[::]:3000".parse().unwrap();
    assert!(validate_bind_address(&addr).is_err());
}

#[test]
fn test_rate_limiter_blocks_after_threshold() {
    let limiter = RateLimiter::new(3, Duration::from_secs(60));
    let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();

    // Under threshold: allowed
    limiter.record_failure(&ip);
    limiter.record_failure(&ip);
    assert!(limiter.check_rate_limit(&ip).is_ok());

    // At threshold: blocked
    limiter.record_failure(&ip);
    assert!(limiter.check_rate_limit(&ip).is_err());

    // Still blocked
    assert!(limiter.check_rate_limit(&ip).is_err());
}

#[test]
fn test_rate_limiter_resets_on_success() {
    let limiter = RateLimiter::new(3, Duration::from_secs(60));
    let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();

    limiter.record_failure(&ip);
    limiter.record_failure(&ip);
    limiter.record_failure(&ip);
    assert!(limiter.check_rate_limit(&ip).is_err());

    // Successful auth should reset
    limiter.record_success(&ip);
    assert!(limiter.check_rate_limit(&ip).is_ok());
}

#[test]
fn test_bearer_token_extraction() {
    // Valid token store accepts the correct token
    let store = TokenStore::new(Duration::from_secs(3600));
    let token_str = store.token().as_str().to_string();
    assert!(store.validate(&token_str).is_ok());

    // Wrong token is rejected
    assert!(store.validate("wrong_token").is_err());

    // Empty string is rejected
    assert!(store.validate("").is_err());
}

/// Verify that the auth token does not appear in tracing log output.
///
/// Uses tracing-subscriber's MakeWriter to capture all bytes written by the
/// tracing formatter during a validation call and asserts the raw token value
/// is absent. The redacted marker ([REDACTED]) must appear instead when an
/// AuthToken is logged via its Debug impl.
#[test]
fn test_token_not_leaked_in_tracing_output() {
    use tracing_subscriber::fmt;
    use tracing_subscriber::prelude::*;

    let log_buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let writer = MakeWriterFactory(Arc::clone(&log_buffer));

    let subscriber =
        tracing_subscriber::registry().with(fmt::layer().with_writer(writer).with_ansi(false));

    tracing::subscriber::with_default(subscriber, || {
        let store = TokenStore::new(Duration::from_secs(3600));
        let token_str = store.token().as_str().to_string();

        // These calls exercise validation paths; ensure no token escapes.
        let _ = store.validate(&token_str);
        let _ = store.validate("wrong_token");

        // Emit a tracing event that includes the AuthToken via its Debug impl.
        // It must show [REDACTED], not the raw value.
        tracing::debug!(token = ?store.token(), "checking token in tracing field");

        let captured = log_buffer.lock().unwrap();
        let log_output = String::from_utf8_lossy(&captured);

        assert!(
            !log_output.contains(&token_str),
            "Auth token must not appear in tracing output. Got:\n{}",
            log_output
        );
        assert!(
            log_output.contains("REDACTED"),
            "Expected [REDACTED] in tracing output when logging an AuthToken. Got:\n{}",
            log_output
        );
    });
}

// --- helpers ----------------------------------------------------------------

struct WriterWrapper(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for WriterWrapper {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct MakeWriterFactory(Arc<Mutex<Vec<u8>>>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for MakeWriterFactory {
    type Writer = WriterWrapper;
    fn make_writer(&'a self) -> Self::Writer {
        WriterWrapper(Arc::clone(&self.0))
    }
}
