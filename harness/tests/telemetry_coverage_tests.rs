use harness::telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
use std::time::Duration;

#[test]
fn telemetry_config_default_has_service_name() {
    let config = TelemetryConfig::default();
    assert!(!config.service_name.is_empty());
}

#[test]
fn prometheus_exporter_clear_buffer_leaves_empty() {
    let mut exporter = PrometheusExporter::new();
    // add a finished trace
    let ctx = TraceContext::new("op".to_string(), None);
    exporter.record_finished_trace(ctx);
    exporter.clear_buffer();
    let output = exporter.export_all();
    assert!(output.is_ok());
}

#[test]
fn prometheus_exporter_health_check_ok() {
    let exporter = PrometheusExporter::new();
    assert!(exporter.health_check().is_ok());
}

#[test]
fn prometheus_exporter_export_finished_trace() {
    let mut exporter = PrometheusExporter::new();
    let ctx = TraceContext::new("my-op".to_string(), None);
    exporter.record_finished_trace(ctx);
    let result = exporter.export_all();
    assert!(result.is_ok());
}

#[test]
fn prometheus_exporter_export_events() {
    let mut exporter = PrometheusExporter::new();
    let event = CustomEvent {
        name: "test-event".to_string(),
        attributes: std::collections::HashMap::new(),
        timestamp: std::time::SystemTime::now(),
    };
    exporter.record_event(event);
    let result = exporter.export_events();
    assert!(result.is_ok());
}

#[test]
fn prometheus_exporter_export_metrics_batch() {
    let mut exporter = PrometheusExporter::new();
    let point = MetricPoint {
        name: "test_metric".to_string(),
        value: 1.0,
        metric_type: MetricType::Counter,
        timestamp: std::time::SystemTime::now(),
        labels: std::collections::HashMap::new(),
    };
    exporter.record_metric(point);
    let result = exporter.export_system_metrics();
    assert!(result.is_ok());
}

#[tokio::test]
async fn trace_guard_drop_removes_active_trace() {
    let system = TelemetrySystem::new(TelemetryConfig::default());
    {
        let _guard = system.start_trace("test-op".to_string(), None);
        assert!(system.active_trace_count() > 0);
    }
    // after drop the trace should be finished
    assert_eq!(system.active_trace_count(), 0);
}

#[tokio::test]
async fn trace_guard_trace_accessor_returns_context() {
    let system = TelemetrySystem::new(TelemetryConfig::default());
    let guard = system.start_trace("op".to_string(), None);
    let ctx = guard.trace();
    assert_eq!(ctx.operation_name(), "op");
}

#[tokio::test]
async fn trace_guard_record_error_sets_error_status() {
    let system = TelemetrySystem::new(TelemetryConfig::default());
    let mut guard = system.start_trace("op".to_string(), None);
    guard.record_error("something went wrong".to_string());
    assert_eq!(guard.trace().status(), SpanStatus::Error);
}

#[tokio::test]
async fn trace_guard_set_status_cancelled() {
    let system = TelemetrySystem::new(TelemetryConfig::default());
    let mut guard = system.start_trace("op".to_string(), None);
    guard.set_status(SpanStatus::Cancelled);
    assert_eq!(guard.trace().status(), SpanStatus::Cancelled);
}

#[test]
fn telemetry_system_export_all_clears_buffer() {
    let system = TelemetrySystem::new(TelemetryConfig::default());
    let result = system.export_all();
    assert!(result.is_ok());
}

#[test]
fn telemetry_system_uptime_is_positive() {
    let system = TelemetrySystem::new(TelemetryConfig::default());
    std::thread::sleep(Duration::from_millis(1));
    assert!(system.get_uptime() > Duration::ZERO);
}

#[test]
fn telemetry_system_builder_sets_service_fields() {
    let config = TelemetryConfig::builder()
        .service_name("my-svc".to_string())
        .service_version("1.2.3".to_string())
        .build();
    let system = TelemetrySystem::new(config);
    assert_eq!(system.service_name(), "my-svc");
}
