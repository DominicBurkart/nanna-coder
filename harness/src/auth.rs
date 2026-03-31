use rand::RngCore;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AuthError {
    #[error("invalid token")]
    InvalidToken,
    #[error("expired token")]
    ExpiredToken,
    #[error("missing authorization header")]
    MissingToken,
    #[error("insecure bind address: non-loopback addresses require TLS (not yet supported)")]
    InsecureBindAddress,
    #[error("rate limited")]
    RateLimited,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Auth token with redacted Debug impl - tokens never appear in logs.
pub struct AuthToken(String);

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl AuthToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn from_string(s: String) -> Self {
        Self(s)
    }
}

pub struct TokenStore {
    token: AuthToken,
    created_at: Instant,
    lifetime: Duration,
}

impl TokenStore {
    pub fn new(lifetime: Duration) -> Self {
        Self {
            token: AuthToken::generate(),
            created_at: Instant::now(),
            lifetime,
        }
    }

    pub fn with_token(token: AuthToken, lifetime: Duration) -> Self {
        Self {
            token,
            created_at: Instant::now(),
            lifetime,
        }
    }

    pub fn validate(&self, candidate: &str) -> Result<(), AuthError> {
        if self.is_expired() {
            return Err(AuthError::ExpiredToken);
        }
        if !constant_time_eq(self.token.as_str().as_bytes(), candidate.as_bytes()) {
            return Err(AuthError::InvalidToken);
        }
        Ok(())
    }

    pub fn is_expired(&self) -> bool {
        self.created_at.elapsed() > self.lifetime
    }

    pub fn token(&self) -> &AuthToken {
        &self.token
    }
}

/// Constant-time comparison to prevent timing attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Validate that a bind address is safe. Non-loopback addresses are rejected
/// because TLS is not yet supported and binding to a public interface would
/// expose the auth token in cleartext.
pub fn validate_bind_address(addr: &SocketAddr) -> Result<(), AuthError> {
    if !addr.ip().is_loopback() {
        return Err(AuthError::InsecureBindAddress);
    }
    Ok(())
}

/// Rate limiter that tracks failed authentication attempts per IP address.
/// After `max_failures` within `window`, subsequent requests from that IP
/// are rejected until the window expires or a successful auth resets the counter.
pub struct RateLimiter {
    state: Mutex<HashMap<IpAddr, (u32, Instant)>>,
    max_failures: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_failures: u32, window: Duration) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            max_failures,
            window,
        }
    }

    /// Check whether the given IP is currently rate-limited.
    pub fn check_rate_limit(&self, ip: &IpAddr) -> Result<(), AuthError> {
        let state = self.state.lock().expect("rate limiter lock poisoned");
        if let Some((failures, first_failure)) = state.get(ip) {
            if first_failure.elapsed() < self.window && *failures >= self.max_failures {
                return Err(AuthError::RateLimited);
            }
        }
        Ok(())
    }

    /// Record a failed authentication attempt for the given IP.
    pub fn record_failure(&self, ip: &IpAddr) {
        let mut state = self.state.lock().expect("rate limiter lock poisoned");
        let entry = state.entry(*ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= self.window {
            // Window expired — reset.
            *entry = (1, Instant::now());
        } else {
            entry.0 += 1;
        }
    }

    /// Record a successful authentication — resets the failure counter for the IP.
    pub fn record_success(&self, ip: &IpAddr) {
        let mut state = self.state.lock().expect("rate limiter lock poisoned");
        state.remove(ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_token_generation_unique() {
        let t1 = AuthToken::generate();
        let t2 = AuthToken::generate();
        assert_ne!(t1.as_str(), t2.as_str());
    }

    #[test]
    fn test_token_length() {
        let token = AuthToken::generate();
        assert_eq!(token.as_str().len(), 64); // 32 bytes = 64 hex chars
    }

    #[test]
    fn test_valid_token_accepted() {
        let store = TokenStore::new(Duration::from_secs(3600));
        let token_str = store.token().as_str().to_string();
        assert!(store.validate(&token_str).is_ok());
    }

    #[test]
    fn test_invalid_token_rejected() {
        let store = TokenStore::new(Duration::from_secs(3600));
        let result = store.validate("wrong_token_value");
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::InvalidToken => {}
            other => panic!("Expected InvalidToken, got: {:?}", other),
        }
    }

    #[test]
    fn test_expired_token_rejected() {
        let store = TokenStore::new(Duration::from_secs(0));
        // With 0-second lifetime, the token is already expired
        std::thread::sleep(Duration::from_millis(1));
        let token_str = store.token().as_str().to_string();
        let result = store.validate(&token_str);
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::ExpiredToken => {}
            other => panic!("Expected ExpiredToken, got: {:?}", other),
        }
    }

    #[test]
    fn test_debug_is_redacted() {
        let token = AuthToken::generate();
        let debug_output = format!("{:?}", token);
        assert!(debug_output.contains("REDACTED"));
        assert!(!debug_output.contains(token.as_str()));
    }

    #[test]
    fn test_auth_error_no_token_leak() {
        let token = AuthToken::generate();
        let token_str = token.as_str().to_string();

        let errors = [
            AuthError::InvalidToken,
            AuthError::ExpiredToken,
            AuthError::MissingToken,
            AuthError::InsecureBindAddress,
            AuthError::RateLimited,
        ];

        for err in &errors {
            let display = format!("{}", err);
            assert!(
                !display.contains(&token_str),
                "Error display leaked token: {}",
                display
            );
        }
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"hello", b"hell"));
        assert!(!constant_time_eq(b"", b"a"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn test_validate_bind_address_loopback_ok() {
        use std::net::SocketAddr;
        let addr: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        assert!(validate_bind_address(&addr).is_ok());
    }

    #[test]
    fn test_validate_bind_address_ipv6_loopback_ok() {
        use std::net::SocketAddr;
        let addr: SocketAddr = "[::1]:3000".parse().unwrap();
        assert!(validate_bind_address(&addr).is_ok());
    }

    #[test]
    fn test_validate_bind_address_rejects_wildcard() {
        use std::net::SocketAddr;
        let addr: SocketAddr = "0.0.0.0:3000".parse().unwrap();
        let result = validate_bind_address(&addr);
        assert!(result.is_err());
        match result.unwrap_err() {
            AuthError::InsecureBindAddress => {}
            other => panic!("Expected InsecureBindAddress, got: {:?}", other),
        }
    }

    #[test]
    fn test_rate_limiter_allows_under_threshold() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        limiter.record_failure(&ip);
        limiter.record_failure(&ip);
        assert!(limiter.check_rate_limit(&ip).is_ok());
    }

    #[test]
    fn test_rate_limiter_blocks_at_threshold() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        limiter.record_failure(&ip);
        limiter.record_failure(&ip);
        limiter.record_failure(&ip);
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
        limiter.record_success(&ip);
        assert!(limiter.check_rate_limit(&ip).is_ok());
    }
}
