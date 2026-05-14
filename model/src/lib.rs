pub mod config;
pub mod judge;
pub mod ollama;
#[cfg(feature = "openai-compat")]
pub mod openai_compat;
pub mod provider;
pub mod types;

pub use config::{GatewayConfig, ModelDefaults, OllamaConfig, OpenAICompatConfig};
pub use judge::{JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult};
pub use provider::{ModelError, ModelProvider, ModelResult, StreamingModelProvider};
pub use types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, FunctionDefinition,
    JsonSchema, MessageRole, ModelInfo, PropertySchema, SchemaType, ToolCall, ToolChoice,
    ToolDefinition, Usage,
};

#[cfg(feature = "ollama")]
pub use ollama::OllamaProvider;

#[cfg(feature = "openai-compat")]
pub use openai_compat::OpenAICompatProvider;

pub mod prelude {
    pub use crate::config::*;
    pub use crate::judge::*;
    pub use crate::provider::*;
    pub use crate::types::*;

    #[cfg(feature = "ollama")]
    pub use crate::ollama::*;

    #[cfg(feature = "openai-compat")]
    pub use crate::openai_compat::*;
}
