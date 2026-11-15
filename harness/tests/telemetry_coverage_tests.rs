use chrono::Utc;
use harness::telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn telemetry_config_default_has_service_name() {
    let config = TelemetryConfig::default();
    assert!(!config.service.name.is_empty());
}

#[test]
fn telemetry_config_default_has_version() {
    let config = TelemetryConfig::default();
    assert!(!config.service.version.is_empty());
}

#[tokio::test]
async fn prometheus_exporter_clear_buffer_empties_output() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "test".to_string(),
        metric_type: MetricType::Counter,
        value: 1.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    exporter.clear_buffer();
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.is_empty());
}

#[tokio::test]
async fn prometheus_exporter_health_check_returns_true() {
    let exporter = PrometheusExporter::new(None);
    let result = exporter.health_check().await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn prometheus_exporter_export_events_ok() {
    let exporter = PrometheusExporter::new(None);
    let event = CustomEvent {
        name: "test-event".to_string(),
        timestamp: Utc::now(),
        category: "test".to_string(),
        attributes: HashMap::new(),
        data: serde_json::json!({}),
        trace_context: None,
    };
    let result = exporter.export_events(vec![event]).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn prometheus_exporter_export_finished_trace() {
    let exporter = PrometheusExporter::new(None);
    let mut ctx = TraceContext::new("my-op");
    ctx.finish();
    let result = exporter.export_traces(vec![ctx]).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn trace_guard_drop_removes_active_trace() {
    let telemetry = TelemetrySystem::new();
    {
        let trace = telemetry.start_trace("test-op");
        let _guard = TraceGuard::new(&telemetry, trace);
        assert_eq!(telemetry.get_active_trace_count(), 1);
    }
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

#[tokio::test]
async fn trace_guard_trace_accessor_returns_context() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("my-op");
    let guard = TraceGuard::new(&telemetry, trace);
    let ctx = guard.trace().unwrap();
    assert_eq!(ctx.operation_name, "my-op");
}

#[tokio::test]
async fn trace_guard_record_error_sets_error_status() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("op");
    let mut guard = TraceGuard::new(&telemetry, trace);
    guard.record_error("something went wrong");
    assert_eq!(guard.trace().unwrap().status, SpanStatus::Error);
}

#[tokio::test]
async fn trace_guard_set_status_cancelled() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("op");
    let mut guard = TraceGuard::new(&telemetry, trace);
    guard.set_status(SpanStatus::Cancelled);
    assert_eq!(guard.trace().unwrap().status, SpanStatus::Cancelled);
}

#[tokio::test]
async fn telemetry_system_export_all_ok_and_clears_metrics() {
    let telemetry = TelemetrySystem::new();
    telemetry.record_counter("test", 1.0, vec![]);
    assert_eq!(telemetry.get_buffered_metrics_count(), 1);
    let result = telemetry.export_all().await;
    assert!(result.is_ok());
    assert_eq!(telemetry.get_buffered_metrics_count(), 0);
}

#[test]
fn telemetry_system_uptime_is_positive() {
    let telemetry = TelemetrySystem::new();
    std::thread::sleep(Duration::from_millis(1));
    assert!(telemetry.get_uptime() > Duration::ZERO);
}

#[tokio::test]
async fn telemetry_system_global_attribute_appears_in_trace() {
    let telemetry = TelemetrySystem::new().with_global_attribute("env", "test");
    let trace = telemetry.start_trace("op");
    assert_eq!(trace.attributes.get("env"), Some(&"test".to_string()));
    telemetry.finish_trace(trace);
}

#[tokio::test]
async fn telemetry_system_service_name_in_trace_attributes() {
    let telemetry = TelemetrySystem::new().with_service_name("my-svc");
    let trace = telemetry.start_trace("op");
    assert_eq!(
        trace.attributes.get("service.name"),
        Some(&"my-svc".to_string())
    );
    telemetry.finish_trace(trace);
}
