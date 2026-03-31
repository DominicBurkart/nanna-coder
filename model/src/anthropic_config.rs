use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub timeout: Duration,
    pub max_retries: u32,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key: std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            model: "claude-sonnet-4-20250514".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            timeout: Duration::from_secs(120),
            max_retries: 3,
        }
    }
}

impl AnthropicConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = api_key.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.api_key.is_empty() {
            return Err("API key cannot be empty. Set ANTHROPIC_API_KEY environment variable or provide it via with_api_key()".to_string());
        }
        if self.base_url.is_empty() {
            return Err("Base URL cannot be empty".to_string());
        }
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err("Base URL must start with http:// or https://".to_string());
        }
        if self.model.is_empty() {
            return Err("Model name cannot be empty".to_string());
        }
        if self.timeout.is_zero() {
            return Err("Timeout must be greater than 0".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = AnthropicConfig::new()
            .with_api_key("sk-test-key")
            .with_model("claude-sonnet-4-20250514")
            .with_base_url("https://api.anthropic.com")
            .with_timeout(Duration::from_secs(60))
            .with_max_retries(5);
        assert_eq!(config.api_key, "sk-test-key");
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.base_url, "https://api.anthropic.com");
        assert_eq!(config.timeout, Duration::from_secs(60));
        assert_eq!(config.max_retries, 5);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_from_env() {
        let config = AnthropicConfig::default();
        assert_eq!(config.model, "claude-sonnet-4-20250514");
        assert_eq!(config.base_url, "https://api.anthropic.com");
        assert_eq!(config.timeout, Duration::from_secs(120));
        assert_eq!(config.max_retries, 3);
    }

    #[test]
    fn test_config_validation() {
        let config = AnthropicConfig::new().with_api_key("");
        assert!(config.validate().is_err());
        let config = AnthropicConfig::new().with_api_key("sk-test").with_base_url("");
        assert!(config.validate().is_err());
        let config = AnthropicConfig::new().with_api_key("sk-test").with_base_url("not-a-url");
        assert!(config.validate().is_err());
        let config = AnthropicConfig::new().with_api_key("sk-test").with_model("");
        assert!(config.validate().is_err());
        let config = AnthropicConfig::new().with_api_key("sk-test").with_timeout(Duration::from_secs(0));
        assert!(config.validate().is_err());
        let config = AnthropicConfig::new().with_api_key("sk-test");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_serialization() {
        let config = AnthropicConfig::new().with_api_key("sk-test");
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: AnthropicConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.model, deserialized.model);
        assert_eq!(config.base_url, deserialized.base_url);
    }
}
