pub mod agent;
pub mod container;
pub mod monitoring;
pub mod observability;
pub mod telemetry;
pub mod tools;

pub use container::{
    cleanup_container, detect_runtime, health_check_container, load_image_from_path,
    start_container_with_fallback, verify_image_exists, ContainerConfig, ContainerError,
    ContainerHandle, ContainerRuntime,
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
pub use tools::{CalculatorTool, EchoTool, Tool, ToolError, ToolRegistry, ToolResult};

// Export agent types
pub use agent::decision::{DecisionContext, EntityType, ModificationDecision};
pub use agent::enrichment::{EnrichmentConfig, EnrichmentResult};
pub use agent::entity::{Entity, EntityError, EntityGraph, EntityRelation, EntityResult};
pub use agent::modification::{
    ExecutionResult, ImpactEstimate, ModificationPlan, ModificationStep, RiskLevel, StepOperation,
};
pub use agent::rag::{QueryContext, QueryResult, RagConfig};
pub use agent::{
    AgentComponent, AgentConfig, AgentContext, AgentError, AgentLoop, AgentResult, AgentRunResult,
    AgentState,
};
