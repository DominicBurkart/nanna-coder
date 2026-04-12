use async_trait::async_trait;
use harness::agent::{AgentConfig, AgentContext, AgentLoop};
use harness::container::{
    detect_runtime, health_check_container, start_container_with_fallback, verify_image_exists,
    ContainerConfig, ContainerError, ContainerRuntime, SharedModelPool,
};
use harness::entities::InMemoryEntityStore;
use harness::entities::{EntityQuery, EntityStore, EntityType};
use harness::mcp::handlers::{handle_assign_task, handle_get_result, handle_poll_task};
use harness::task::TaskManager;
use harness::tools::{CalculatorTool, EchoTool, Tool, ToolRegistry};
use model::judge::{
    JudgeConfig, ModelJudge, ValidationCriteria, ValidationMetrics, ValidationResult,
};
use model::prelude::*;
use model::types::{
    ChatMessage, ChatRequest, ChatResponse, Choice, FinishReason, FunctionCall, ModelInfo, ToolCall,
};
use model::{ModelError, ModelProvider, ModelResult};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{sleep, timeout};
// use futures::future; // Reserved for future concurrent test implementation

// E2E test configuration
const E2E_MODEL: &str = "qwen3:0.6b";
const E2E_TIMEOUT: Duration = Duration::from_secs(300);
const CONTAINER_STARTUP_WAIT: Duration = Duration::from_secs(30);
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(60);
