use rand::RngCore;
use std::path::Path;
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

/// Write token to file with restrictive permissions (0600 on Unix).
pub fn write_token_file(token: &AuthToken, path: &Path) -> Result<(), AuthError> {
    use std::fs;
    use std::io::Write;
    let mut file = fs::File::create(path)?;
    file.write_all(token.as_str().as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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
    fn test_write_token_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let token = AuthToken::generate();
        write_token_file(&token, &path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, token.as_str());
    }

    #[cfg(unix)]
    #[test]
    fn test_token_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let token = AuthToken::generate();
        write_token_file(&token, &path).unwrap();

        let metadata = std::fs::metadata(&path).unwrap();
        let mode = metadata.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
