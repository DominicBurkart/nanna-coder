pub mod config;
pub mod judge;
pub mod ollama;
pub mod provider;
pub mod types;

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

pub mod prelude {
    pub use crate::config::{ModelDefaults, OllamaConfig};
    pub use crate::judge::{JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult};
    pub use crate::provider::{ModelError, ModelProvider, ModelResult, StreamingModelProvider};
    pub use crate::types::{
        ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall,
        FunctionDefinition, JsonSchema, MessageRole, ModelInfo, PropertySchema, SchemaType,
        ToolCall, ToolChoice, ToolDefinition, Usage,
    };

    #[cfg(feature = "ollama")]
    pub use crate::ollama::OllamaProvider;
}
