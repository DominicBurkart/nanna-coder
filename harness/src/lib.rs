pub mod agent;
pub mod container;
pub mod entities;
pub mod eval;
pub mod mcp;
pub mod monitoring;
pub mod observability;
pub mod onboarding;
pub mod output;
pub mod task;
pub mod telemetry;
pub mod tools;
pub mod workspace;

pub use container::{
    cleanup_container, detect_runtime, exec_in_container, health_check_container,
    load_image_from_path, start_container_with_fallback, verify_image_exists, CommandOutput,
    ContainerConfig, ContainerError, ContainerHandle, ContainerRuntime, SharedModelPool,
};
pub use monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
    MonitoringError, MonitoringSystem, SystemStatus,
};
pub use observability::{
    AlertCategory, AlertInfo, AlertPolicy, ComprehensiveStatus, HealthThreshold,
    ObservabilityError, ObservabilitySystem, PerformanceTrends, TrendDirection,
};
pub use telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryError, TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
pub use tools::{
    create_tool_registry, CalculatorTool, EchoTool, GitDiffTool, GitHubPrStatusTool, GitHubStatus,
    GitStatusTool, ListDirTool, PrStatusData, ReadFileTool, RunCommandTool, SearchTool, Tool,
    ToolError, ToolRegistry, ToolResult, WriteFileTool,
};

// Export agent types
pub use agent::{
    AgentComponent, AgentConfig, AgentContext, AgentError, AgentLoop, AgentResult, AgentRunResult,
    AgentState,
};

// Export eval report types
pub use eval::report::EvalReport;

// Export entity types
pub use entities::{
    Entity, EntityError, EntityId, EntityMetadata, EntityQuery, EntityRelationship, EntityResult,
    EntityStore, EntityType, InMemoryEntityStore, QueryResult, RelationshipType, TimeRange,
};
