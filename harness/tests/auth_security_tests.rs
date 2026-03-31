use harness::auth::{validate_bind_address, AuthToken, RateLimiter, TokenStore};
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
