//! Request and response types shared between the fixture `api` and `ui` crates.
//!
//! Both sides serialise these with `serde`, so a change here that breaks the
//! wire format is caught at compile time on the backend and the frontend.

use serde::{Deserialize, Serialize};

/// Response body of `GET /health/v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    /// Always `"ok"` when the process is serving traffic.
    pub status: String,
}

impl Health {
    /// The only healthy response the fixture ever produces.
    pub fn ok() -> Self {
        Self {
            status: "ok".to_string(),
        }
    }
}

/// Response body of `GET /api/v1/greeting`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Greeting {
    /// Human-readable text the `ui` renders into `#greeting`.
    pub message: String,
}

/// The greeting the fixture returns when nothing is broken.
pub const DEFAULT_GREETING: &str = "Hello from the full-stack fixture";

impl Default for Greeting {
    fn default() -> Self {
        Self {
            message: DEFAULT_GREETING.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_ok_serialises_to_expected_json() {
        let json = serde_json::to_string(&Health::ok()).unwrap();
        assert_eq!(json, r#"{"status":"ok"}"#);
    }

    #[test]
    fn greeting_round_trips_through_json() {
        let greeting = Greeting::default();
        let json = serde_json::to_string(&greeting).unwrap();
        let back: Greeting = serde_json::from_str(&json).unwrap();
        assert_eq!(back, greeting);
        assert_eq!(back.message, DEFAULT_GREETING);
    }
}
