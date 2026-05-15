//! Test-only provider that satisfies `ModelProvider` without a live model
//! backend. Used by the mcp stdio integration test (see issue #201).
//!
//! Activated via the `NANNA_MCP_MOCK_PROVIDER=1` env var checked at the top of
//! `run_mcp_server`. The integration test never exercises model-calling paths
//! (`tools/call` for `assign_task` etc.), so `chat` deliberately errors to
//! catch accidental production use.

use async_trait::async_trait;
use model::provider::{ModelProvider, ModelResult};
use model::types::{ChatRequest, ChatResponse, ModelInfo};

#[doc(hidden)]
pub struct MockProvider;

#[async_trait]
impl ModelProvider for MockProvider {
    async fn chat(&self, _: ChatRequest) -> ModelResult<ChatResponse> {
        Err(model::provider::ModelError::Unknown {
            message: "MockProvider::chat called — only initialize/tools/list are supported"
                .to_string(),
        })
    }

    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn health_check(&self) -> ModelResult<()> {
        Ok(())
    }

    fn provider_name(&self) -> &'static str {
        "mock"
    }
}
