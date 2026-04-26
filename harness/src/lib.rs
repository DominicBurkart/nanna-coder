pub mod agent;
pub mod container;
pub mod entities;
pub mod eval;
pub mod mcp;
#[cfg(feature = "observability")]
pub mod monitoring;
#[cfg(feature = "observability")]
pub mod observability;
pub mod onboarding;
pub mod task;
#[cfg(feature = "observability")]
pub mod telemetry;
pub mod tools;
pub mod workspace;

/// Default system prompt used when a repo does not supply any repo-level
/// guidance (no `AGENTS.md` / `CLAUDE.md`). Defined here — in the library
/// root — so that both the binary entry-point (`main.rs`) and the
/// task-dispatch path (`task.rs`) share a single source of truth instead of
/// maintaining in-sync copies.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a helpful coding assistant. Use the available tools to accomplish tasks. When you have completed the task, respond with a summary.";

pub use container::{
    cleanup_container, detect_runtime, exec_in_container, health_check_container,
    load_image_from_path, start_container_with_fallback, verify_image_exists, CommandOutput,
    ContainerConfig, ContainerError, ContainerHandle, ContainerRuntime, SharedModelPool,
};
#[cfg(feature = "observability")]
pub use monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
    MonitoringError, MonitoringSystem, SystemStatus,
};
#[cfg(feature = "observability")]
pub use observability::{
    AlertCategory, AlertInfo, AlertPolicy, ComprehensiveStatus, HealthThreshold,
    ObservabilityError, ObservabilitySystem, PerformanceTrends, TrendDirection,
};
#[cfg(feature = "observability")]
pub use telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryError, TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
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
