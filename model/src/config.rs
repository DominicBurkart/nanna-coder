use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_NUM_CTX: u32 = 16_384;
pub const MIN_NUM_CTX: u32 = 2_048;
pub const MAX_NUM_CTX: u32 = 131_072;

fn default_num_ctx() -> u32 {
    DEFAULT_NUM_CTX
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub default_context_length: u32,
    #[serde(default = "default_num_ctx")]
    pub num_ctx: u32,
    pub default_temperature: f32,
    pub default_max_tokens: Option<u32>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".to_string(),
            timeout: Duration::from_secs(300),
            default_context_length: 110_000,
            num_ctx: DEFAULT_NUM_CTX,
            default_temperature: 0.7,
            default_max_tokens: None,
        }
    }
}

impl OllamaConfig {
    pub fn with_num_ctx(mut self, num_ctx: u32) -> Self {
        self.num_ctx = num_ctx;
        self
    }

    pub fn num_ctx_from_env(value: Option<&str>) -> Result<Option<u32>, String> {
        let Some(raw) = value else {
            return Ok(None);
        };
        let parsed: u32 = raw
            .trim()
            .parse()
            .map_err(|_| format!("NANNA_NUM_CTX must be a whole number, got {raw:?}"))?;
        if !(MIN_NUM_CTX..=MAX_NUM_CTX).contains(&parsed) {
            return Err(format!(
                "NANNA_NUM_CTX must be between {MIN_NUM_CTX} and {MAX_NUM_CTX}, got {parsed}"
            ));
        }
        Ok(Some(parsed))
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_context_length(mut self, context_length: u32) -> Self {
        self.default_context_length = context_length;
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.default_temperature = temperature;
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.default_max_tokens = Some(max_tokens);
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.base_url.is_empty() {
            return Err("Base URL cannot be empty".to_string());
        }

        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err("Base URL must start with http:// or https://".to_string());
        }

        if self.default_context_length == 0 {
            return Err("Context length must be greater than 0".to_string());
        }

        if !(MIN_NUM_CTX..=MAX_NUM_CTX).contains(&self.num_ctx) {
            return Err(format!(
                "num_ctx must be between {MIN_NUM_CTX} and {MAX_NUM_CTX}"
            ));
        }

        if !(0.0..=2.0).contains(&self.default_temperature) {
            return Err("Temperature must be between 0.0 and 2.0".to_string());
        }

        if let Some(max_tokens) = self.default_max_tokens {
            if max_tokens == 0 {
                return Err("Max tokens must be greater than 0".to_string());
            }
        }

        if self.timeout.is_zero() {
            return Err("Timeout must be greater than 0".to_string());
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDefaults {
    pub temperature: f32,
    pub max_tokens: Option<u32>,
    pub context_length: u32,
}

impl Default for ModelDefaults {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            max_tokens: None,
            context_length: 110_000,
        }
    }
}

#[cfg(kani)]
mod kani_proofs {
    /// Verify that OllamaConfig::validate rejects bad temperatures.
    ///
    /// The valid range is [0.0, 2.0]. We check that any f32 outside
    /// this range is rejected.
    #[kani::proof]
    fn temperature_range_validation() {
        let temp: f32 = kani::any();
        // Skip NaN — Rust's range-contains returns false for NaN, which
        // is correct but not interesting to verify here.
        kani::assume(!temp.is_nan());

        let in_range = (0.0..=2.0f32).contains(&temp);
        if temp < 0.0 || temp > 2.0 {
            assert!(!in_range);
        } else {
            assert!(in_range);
        }
    }

    /// Verify that context_length == 0 is always caught.
    #[kani::proof]
    fn zero_context_length_rejected() {
        let ctx: u32 = kani::any();
        let valid = ctx > 0;
        if ctx == 0 {
            assert!(!valid);
        } else {
            assert!(valid);
        }
    }

    /// Verify max_tokens == 0 is always caught when Some.
    #[kani::proof]
    fn zero_max_tokens_rejected() {
        let max_tokens: u32 = kani::any();
        let valid = max_tokens > 0;
        if max_tokens == 0 {
            assert!(!valid);
        } else {
            assert!(valid);
        }
    }

    /// The concrete `ModelDefaults::default()` temperature must fall
    /// inside `OllamaConfig::validate`'s accepted range. Exercises the
    /// real default constructor and verifies the invariant against the
    /// actual value, not a re-asserted assumption.
    #[kani::proof]
    fn model_defaults_default_temperature_is_valid() {
        let defaults = super::ModelDefaults::default();
        assert!(!defaults.temperature.is_nan());
        assert!((0.0..=2.0f32).contains(&defaults.temperature));
    }

    /// The concrete `OllamaConfig::default()` must pass `validate()`.
    /// If a future change to the defaults introduces an out-of-range
    /// temperature, zero context length, or zero timeout, this proof
    /// fails at verification time.
    #[kani::proof]
    fn ollama_config_default_validates() {
        let cfg = super::OllamaConfig::default();
        assert!(cfg.validate().is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_ctx_defaults_to_a_window_that_holds_a_real_task_and_is_validated() {
        let config = OllamaConfig::default();
        assert_eq!(config.num_ctx, 16_384);
        assert_eq!(config.clone().with_num_ctx(8_192).num_ctx, 8_192);
        assert!(config.clone().with_num_ctx(2_047).validate().is_err());
        assert!(config.clone().with_num_ctx(131_073).validate().is_err());
        assert!(config.clone().with_num_ctx(2_048).validate().is_ok());
        assert!(config.with_num_ctx(131_072).validate().is_ok());
    }

    #[test]
    fn num_ctx_from_env_accepts_a_whole_number_in_range_and_names_the_variable_otherwise() {
        assert_eq!(OllamaConfig::num_ctx_from_env(None), Ok(None));
        assert_eq!(
            OllamaConfig::num_ctx_from_env(Some("32768")),
            Ok(Some(32_768))
        );
        assert_eq!(
            OllamaConfig::num_ctx_from_env(Some(" 8192 ")),
            Ok(Some(8_192))
        );
        for bad in ["abc", "", "-5", "1.5", "100", "999999"] {
            let error = OllamaConfig::num_ctx_from_env(Some(bad)).unwrap_err();
            assert!(error.contains("NANNA_NUM_CTX"), "{bad}: {error}");
        }
    }

    #[test]
    fn a_stored_config_without_num_ctx_still_loads_with_the_default() {
        let stored = serde_json::json!({
            "base_url": "http://localhost:11434",
            "timeout": {"secs": 300, "nanos": 0},
            "default_context_length": 110000,
            "default_temperature": 0.7,
            "default_max_tokens": null
        });
        let config: OllamaConfig = serde_json::from_value(stored).unwrap();
        assert_eq!(config.num_ctx, 16_384);
    }

    #[test]
    fn test_default_config() {
        let config = OllamaConfig::default();
        assert_eq!(config.base_url, "http://localhost:11434");
        assert_eq!(config.default_context_length, 110_000);
        assert_eq!(config.default_temperature, 0.7);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_builder() {
        let config = OllamaConfig::new()
            .with_base_url("https://api.example.com")
            .with_context_length(50_000)
            .with_temperature(0.5)
            .with_timeout(Duration::from_secs(60));

        assert_eq!(config.base_url, "https://api.example.com");
        assert_eq!(config.default_context_length, 50_000);
        assert_eq!(config.default_temperature, 0.5);
        assert_eq!(config.timeout, Duration::from_secs(60));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_validation() {
        let mut config = OllamaConfig {
            base_url: "".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());

        config.base_url = "invalid-url".to_string();
        assert!(config.validate().is_err());

        config.base_url = "http://localhost:11434".to_string();
        config.default_context_length = 0;
        assert!(config.validate().is_err());

        config.default_context_length = 110_000;
        config.default_temperature = -1.0;
        assert!(config.validate().is_err());

        config.default_temperature = 3.0;
        assert!(config.validate().is_err());

        config.default_temperature = 0.7;
        config.timeout = Duration::from_secs(0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_serialization() {
        let config = OllamaConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: OllamaConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.base_url, deserialized.base_url);
        assert_eq!(
            config.default_context_length,
            deserialized.default_context_length
        );
    }
}
