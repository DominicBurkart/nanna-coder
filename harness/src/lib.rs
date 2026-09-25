pub mod agent;
pub mod auditor;
pub mod container;
pub mod effects;
pub mod entities;
pub mod eval;
pub mod identity;
pub mod marker;
pub mod mcp;
pub mod monitoring;
pub mod observability;
pub mod onboarding;
pub mod pod;
pub mod protected;
pub mod scope;
pub mod task;
pub mod telemetry;
pub mod tools;
pub mod workspace;

pub use container::{
    cleanup_container, detect_runtime, exec_in_container, health_check_container,
    load_image_from_path, start_container_with_fallback, verify_image_exists, CommandOutput,
    ContainerConfig, ContainerError, ContainerHandle, ContainerRuntime, NetworkPolicy,
    ReadOnlyMount, SharedModelPool,
};
pub use effects::{EffectClass, UnknownEffectClass};
pub use identity::{AgentIdentity, DevLoop, IdentityCatalog, IdentityError, ToolPattern};
pub use marker::{parse_identity_from_text, render_html_marker, render_trailer, IDENTITY_TRAILER};
pub use monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
    MonitoringError, MonitoringSystem, SystemStatus,
};
pub use observability::{
    AlertCategory, AlertInfo, AlertPolicy, ComprehensiveStatus, HealthThreshold,
    ObservabilityError, ObservabilitySystem, PerformanceTrends, TrendDirection,
};
pub use protected::{
    AuditHook, NoopAuditHook, ProtectedPathViolation, ProtectedPaths, PROTECTED_PATTERNS,
};
pub use scope::{DenialReason, PathAccess, PathScope, ScopeDenial, ScopeError};
pub use telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryError, TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
pub use tools::{
    create_container_tool_registry, create_container_tool_registry_for, create_tool_registry,
    create_tool_registry_for, CalculatorTool, EchoTool, GitDiffTool, GitHubPrStatusTool,
    GitHubStatus, GitStatusTool, ListDirTool, PrStatusData, ReadFileTool, RunCommandTool,
    SearchTool, Tool, ToolError, ToolRegistry, ToolResult, WriteFileTool, CONTAINER_WORKSPACE_DIR,
};

// Export agent types
pub use agent::{
    AgentComponent, AgentConfig, AgentContext, AgentError, AgentLoop, AgentResult, AgentRunReport,
    AgentRunResult, AgentState, TokenUsageDto, ToolCallSummary,
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
