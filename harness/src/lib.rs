pub mod agent;
pub mod container;
pub mod entities;
pub mod eval;
pub mod mcp;
pub mod onboarding;
pub mod task;
pub mod tools;
pub mod workspace;

pub use container::{
    cleanup_container, detect_runtime, exec_in_container, health_check_container,
    load_image_from_path, start_container_with_fallback, verify_image_exists, CommandOutput,
    ContainerConfig, ContainerError, ContainerHandle, ContainerRuntime, SharedModelPool,
};
pub use tools::{
    create_container_tool_registry, create_tool_registry, CalculatorTool, EchoTool, GitDiffTool,
    GitHubPrStatusTool, GitHubStatus, GitStatusTool, ListDirTool, PrStatusData, ReadFileTool,
    RunCommandTool, SearchTool, Tool, ToolError, ToolRegistry, ToolResult, WriteFileTool,
    CONTAINER_WORKSPACE_DIR,
};

// Export agent types
pub use agent::{
    AgentComponent, AgentConfig, AgentContext, AgentError, AgentLoop, AgentResult, AgentRunResult,
    AgentState,
};

// Export eval types
pub use eval::report::EvalReport;
#[cfg(feature = "eval-runner")]
pub use eval::runner::{run_eval, EvalRunResult, EvalRunnerConfig, EvalRunnerError};

// Export entity types
pub use entities::{
    Entity, EntityError, EntityId, EntityMetadata, EntityQuery, EntityRelationship, EntityResult,
    EntityStore, EntityType, InMemoryEntityStore, QueryResult, RelationshipType, TimeRange,
};
