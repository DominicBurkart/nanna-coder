pub mod action_auditor;
pub mod agent;
pub mod apprun;
pub mod auditor;
pub mod backlog;
pub mod budget;
pub mod capabilities;
pub mod ci_tools;
pub mod container;
pub mod deploy;
pub mod effects;
pub mod entities;
pub mod escalation;
pub mod eval;
pub mod identity;
pub mod leases;
pub mod marker;
pub mod mcp;
pub mod monitoring;
pub mod observability;
pub mod onboarding;
pub mod pod;
pub mod pr_tools;
pub mod protected;
pub mod qa;
pub mod rollout;
pub mod sandbox_tools;
pub mod scheduler;
pub mod scope;
pub mod sidecar;
pub mod task;
pub mod telemetry;
pub mod tools;
pub mod windows;
pub mod workspace;

pub use capabilities::{
    detect_capabilities, detect_capabilities_from_entries, detect_capability_locations,
    find_capability, CargoCapability, SignalScope, CARGO_CAPABILITIES,
};
pub use ci_tools::{CiLogsTool, CiStatusTool, CiTriggerTool};
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
pub use pr_tools::{
    GitPushBranchTool, GithubIssueCommentTool, GithubIssueReadTool, GithubPrCloseTool,
    GithubPrCommentsTool, GithubPrOpenTool, GithubPrPromoteTool,
};
pub use protected::{
    AuditHook, NoopAuditHook, ProtectedPathViolation, ProtectedPaths, PROTECTED_PATTERNS,
};
pub use scope::{DenialReason, PathAccess, PathScope, ScopeDenial, ScopeError};
pub use sidecar::{
    task_network_name, CommandRunner, PostgresSidecar, ReadinessConfig, RunOutput, RunningSidecar,
    SidecarError, SidecarSet, SidecarSpec, SystemRunner, TaskNetwork, DATABASE_URL_VAR,
    POSTGRES_ALIAS, POSTGRES_IMAGE, POSTGRES_PORT, POSTGRES_USER,
};
pub use telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryError, TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
pub use tools::{
    cargo_audit_args, cargo_bench_args, cargo_build_args, cargo_check_args, cargo_deny_args,
    cargo_run_args, cargo_test_args, create_container_tool_registry,
    create_container_tool_registry_for, create_tool_registry, create_tool_registry_for,
    member_working_dir, sqlx_migrate_args, trunk_build_args, CalculatorTool, CargoAuditTool,
    CargoBenchTool, CargoBuildTool, CargoCheckTool, CargoDenyTool, CargoRunTool, CargoTestTool,
    EchoTool, GitDiffTool, GitHubPrStatusTool, GitHubStatus, GitStatusTool, ListDirTool,
    PrStatusData, ReadFileTool, RunCommandTool, SearchTool, SqlxMigrateTool, Tool, ToolError,
    ToolRegistry, ToolResult, TrunkBuildTool, WriteFileTool, CONTAINER_WORKSPACE_DIR,
    SQLX_MIGRATE_COMMANDS,
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
