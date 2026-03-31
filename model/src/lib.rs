pub mod config;
pub mod judge;
pub mod ollama;
pub mod provider;
pub mod types;

#[cfg(feature = "anthropic")]
pub mod anthropic;
#[cfg(feature = "anthropic")]
pub mod anthropic_config;

pub use config::{ModelDefaults, OllamaConfig};
pub use judge::{JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult};
pub use provider::{ModelError, ModelProvider, ModelResult, StreamingModelProvider};
pub use types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, FunctionDefinition,
    JsonSchema, MessageRole, ModelInfo, PropertySchema, SchemaType, ToolCall, ToolChoice,
    ToolDefinition, Usage,
};

#[cfg(feature = "ollama")]
pub use ollama::OllamaProvider;

#[cfg(feature = "anthropic")]
pub use anthropic::AnthropicProvider;
#[cfg(feature = "anthropic")]
pub use anthropic_config::AnthropicConfig;

pub mod prelude {
    pub use crate::config::*;
    pub use crate::judge::*;
    pub use crate::provider::*;
    pub use crate::types::*;

    #[cfg(feature = "ollama")]
    pub use crate::ollama::*;

    #[cfg(feature = "anthropic")]
    pub use crate::anthropic::*;
    #[cfg(feature = "anthropic")]
    pub use crate::anthropic_config::*;
}
