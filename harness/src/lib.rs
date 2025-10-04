pub mod agent;
pub mod container;
pub mod entity;
pub mod lifecycle;
pub mod monitoring;
pub mod observability;
pub mod telemetry;
pub mod tools;

pub use agent::{
    ActionType, AgentError, AgentLoop, AgentResult, ApplicationState, EntityModification,
    ModificationDecision, ModificationPlan, ModificationResult, PlannedAction, UserPrompt,
};
pub use container::{
    cleanup_container, detect_runtime, health_check_container, load_image_from_path,
    start_container_with_fallback, verify_image_exists, ContainerConfig, ContainerError,
    ContainerHandle, ContainerRuntime,
};
pub use entity::{
    Entity, EntityError, EntityGraph, EntityMetadata, EntityRelation, EntityResult, EntityType,
};
pub use lifecycle::{
    BuildSpec, ContainerRelationship, ContainerState, ContainerStatus, ContainerType,
    DevModifications, LifecycleError, LifecycleManager, LifecycleResult, ModelQuery, ModelResponse,
    PromotionSpec,
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
