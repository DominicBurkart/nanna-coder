use crate::provider::{ModelProvider, ModelResult};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub default_context_length: u32,
    pub default_temperature: f32,
    pub default_max_tokens: Option<u32>,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434".to_string(),
            timeout: Duration::from_secs(30),
            default_context_length: 110_000,
            default_temperature: 0.7,
            default_max_tokens: None,
        }
    }
}

impl OllamaConfig {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAICompatConfig {
    pub base_url: String,
    pub api_key: Option<String>,
    pub default_model: String,
    pub timeout: Duration,
}

impl Default for OpenAICompatConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:8080".to_string(),
            api_key: None,
            default_model: "default".to_string(),
            timeout: Duration::from_secs(30),
        }
    }
}

impl OpenAICompatConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.base_url.is_empty() {
            return Err("Base URL cannot be empty".to_string());
        }
        if !self.base_url.starts_with("http://") && !self.base_url.starts_with("https://") {
            return Err("Base URL must start with http:// or https://".to_string());
        }
        if self.default_model.is_empty() {
            return Err("Default model cannot be empty".to_string());
        }
        if self.timeout.is_zero() {
            return Err("Timeout must be greater than 0".to_string());
        }
        Ok(())
    }
}

/// Unified gateway configuration for provider selection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "kebab-case")]
pub enum GatewayConfig {
    Ollama(OllamaConfig),
    OpenaiCompat(OpenAICompatConfig),
}

impl Default for GatewayConfig {
    fn default() -> Self {
        GatewayConfig::Ollama(OllamaConfig::default())
    }
}

impl GatewayConfig {
    /// Build the appropriate `ModelProvider` from this configuration.
    pub fn build_provider(&self) -> ModelResult<Arc<dyn ModelProvider>> {
        match self {
            #[cfg(feature = "ollama")]
            GatewayConfig::Ollama(cfg) => {
                let provider = crate::ollama::OllamaProvider::new(cfg.clone())?;
                Ok(Arc::new(provider))
            }
            #[cfg(not(feature = "ollama"))]
            GatewayConfig::Ollama(_) => Err(crate::provider::ModelError::InvalidConfig {
                message: "Ollama feature is not enabled".to_string(),
            }),
            #[cfg(feature = "openai-compat")]
            GatewayConfig::OpenaiCompat(cfg) => {
                cfg.validate()
                    .map_err(|msg| crate::provider::ModelError::InvalidConfig { message: msg })?;
                let provider = crate::openai_compat::OpenAICompatProvider::new(
                    &cfg.base_url,
                    cfg.api_key.clone(),
                    &cfg.default_model,
                    cfg.timeout,
                )?;
                Ok(Arc::new(provider))
            }
            #[cfg(not(feature = "openai-compat"))]
            GatewayConfig::OpenaiCompat(_) => Err(crate::provider::ModelError::InvalidConfig {
                message: "openai-compat feature is not enabled".to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_default_gateway_config_is_ollama() {
        let gw = GatewayConfig::default();
        assert!(matches!(gw, GatewayConfig::Ollama(_)));
    }

    #[test]
    fn test_gateway_config_serde_roundtrip_ollama() {
        let gw = GatewayConfig::Ollama(OllamaConfig::default());
        let json = serde_json::to_string(&gw).unwrap();
        assert!(json.contains("\"provider\":\"ollama\""));
        let deserialized: GatewayConfig = serde_json::from_str(&json).unwrap();
        assert!(matches!(deserialized, GatewayConfig::Ollama(_)));
    }

    #[test]
    fn test_gateway_config_serde_roundtrip_openai_compat() {
        let cfg = OpenAICompatConfig {
            base_url: "https://api.example.com".to_string(),
            api_key: Some("sk-test".to_string()),
            default_model: "gpt-4".to_string(),
            timeout: Duration::from_secs(60),
        };
        let gw = GatewayConfig::OpenaiCompat(cfg);
        let json = serde_json::to_string(&gw).unwrap();
        assert!(json.contains("\"provider\":\"openai-compat\""));
        let deserialized: GatewayConfig = serde_json::from_str(&json).unwrap();
        match deserialized {
            GatewayConfig::OpenaiCompat(c) => {
                assert_eq!(c.base_url, "https://api.example.com");
                assert_eq!(c.api_key, Some("sk-test".to_string()));
                assert_eq!(c.default_model, "gpt-4");
            }
            _ => panic!("Expected OpenaiCompat variant"),
        }
    }

    #[test]
    fn test_openai_compat_config_validation() {
        let mut cfg = OpenAICompatConfig::default();
        assert!(cfg.validate().is_ok());

        cfg.base_url = "".to_string();
        assert!(cfg.validate().is_err());

        cfg.base_url = "ftp://bad".to_string();
        assert!(cfg.validate().is_err());

        cfg.base_url = "http://localhost:8080".to_string();
        cfg.default_model = "".to_string();
        assert!(cfg.validate().is_err());
    }
}
