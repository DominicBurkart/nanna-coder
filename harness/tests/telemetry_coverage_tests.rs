use chrono::Utc;
use harness::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryExporter,
    TelemetrySystem, TraceContext, TraceGuard,
};
use std::collections::HashMap;

#[tokio::test]
async fn trace_guard_drop_removes_from_active_traces() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("guarded_op");
    assert_eq!(system.get_active_trace_count(), 1);
    {
        let _guard = TraceGuard::new(&system, trace);
    }
    assert_eq!(system.get_active_trace_count(), 0);
}

#[tokio::test]
async fn trace_guard_trace_returns_context() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("my_op");
    let guard = TraceGuard::new(&system, trace);
    let ctx = guard.trace().unwrap();
    assert_eq!(ctx.operation_name, "my_op");
    assert_eq!(ctx.status, SpanStatus::InProgress);
}

#[tokio::test]
async fn trace_guard_record_error() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("op");
    let mut guard = TraceGuard::new(&system, trace);
    guard.record_error("test error");
    let ctx = guard.trace().unwrap();
    assert_eq!(ctx.status, SpanStatus::Error);
    assert_eq!(
        ctx.attributes.get("error").map(|s| s.as_str()),
        Some("test error")
    );
}

#[tokio::test]
async fn trace_guard_set_status_cancelled() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("op");
    let mut guard = TraceGuard::new(&system, trace);
    guard.set_status(SpanStatus::Cancelled);
    assert_eq!(guard.trace().unwrap().status, SpanStatus::Cancelled);
}

#[tokio::test]
async fn trace_guard_set_status_timeout() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("op");
    let mut guard = TraceGuard::new(&system, trace);
    guard.set_status(SpanStatus::Timeout);
    assert_eq!(guard.trace().unwrap().status, SpanStatus::Timeout);
}

#[tokio::test]
async fn span_finish_preserves_cancelled_status() {
    let mut trace = TraceContext::new("op");
    trace.set_status(SpanStatus::Cancelled);
    trace.finish();
    assert_eq!(trace.status, SpanStatus::Cancelled);
}

#[tokio::test]
async fn span_finish_preserves_timeout_status() {
    let mut trace = TraceContext::new("op");
    trace.set_status(SpanStatus::Timeout);
    trace.finish();
    assert_eq!(trace.status, SpanStatus::Timeout);
}

#[tokio::test]
async fn trace_context_with_attribute_chain() {
    let trace = TraceContext::new("op")
        .with_attribute("key1", "v1")
        .with_attribute("key2", "v2");
    assert_eq!(trace.attributes.get("key1").map(|s| s.as_str()), Some("v1"));
    assert_eq!(trace.attributes.get("key2").map(|s| s.as_str()), Some("v2"));
}

#[tokio::test]
async fn trace_context_create_child() {
    let parent = TraceContext::new("parent");
    let child = parent.create_child("child");
    assert_eq!(child.trace_id, parent.trace_id);
    assert_eq!(child.parent_span_id, Some(parent.span_id.clone()));
    assert_ne!(child.span_id, parent.span_id);
    assert_eq!(child.status, SpanStatus::InProgress);
}

#[tokio::test]
async fn telemetry_global_attribute_propagates_to_trace() {
    let system = TelemetrySystem::new().with_global_attribute("env", "prod");
    let trace = system.start_trace("op");
    assert_eq!(
        trace.attributes.get("env").map(|s| s.as_str()),
        Some("prod")
    );
    system.finish_trace(trace);
}

#[tokio::test]
async fn telemetry_start_trace_includes_service_name() {
    let system = TelemetrySystem::new().with_service_name("my-svc");
    let trace = system.start_trace("op");
    assert_eq!(
        trace.attributes.get("service.name").map(|s| s.as_str()),
        Some("my-svc")
    );
    system.finish_trace(trace);
}

#[tokio::test]
async fn telemetry_export_all_clears_metrics_buffer() {
    let system = TelemetrySystem::new();
    system.record_counter("c1", 1.0, vec![]);
    system.record_counter("c2", 2.0, vec![]);
    assert_eq!(system.get_buffered_metrics_count(), 2);
    system.export_all().await.unwrap();
    assert_eq!(system.get_buffered_metrics_count(), 0);
}

#[tokio::test]
async fn prometheus_export_traces_adds_histogram() {
    let exporter = PrometheusExporter::new(None);
    let mut trace = TraceContext::new("my_op");
    trace.finish();
    exporter.export_traces(vec![trace]).await.unwrap();
    let output = exporter.export_prometheus().await.unwrap();
    assert!(
        output.contains("trace_duration_seconds"),
        "output: {output}"
    );
    assert!(output.contains("histogram"), "output: {output}");
    assert!(output.contains("operation=\"my_op\""), "output: {output}");
}

#[tokio::test]
async fn prometheus_export_events_adds_counter() {
    let exporter = PrometheusExporter::new(None);
    let event = CustomEvent {
        name: "user_login".to_string(),
        timestamp: Utc::now(),
        category: "auth".to_string(),
        attributes: HashMap::new(),
        data: serde_json::json!({}),
        trace_context: None,
    };
    exporter.export_events(vec![event]).await.unwrap();
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("custom_events_total"), "output: {output}");
    assert!(output.contains("counter"), "output: {output}");
    assert!(
        output.contains("event_name=\"user_login\""),
        "output: {output}"
    );
}

#[tokio::test]
async fn prometheus_export_system_metrics_adds_gauges() {
    let exporter = PrometheusExporter::new(None);
    let metrics = build_system_metrics();
    exporter.export_system_metrics(metrics).await.unwrap();
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("cache_hit_rate"), "output: {output}");
    assert!(
        output.contains("request_duration_seconds"),
        "output: {output}"
    );
    assert!(output.contains("error_rate"), "output: {output}");
}

#[tokio::test]
async fn prometheus_health_check_returns_ok() {
    let exporter = PrometheusExporter::new(None);
    let result = exporter.health_check().await.unwrap();
    assert!(result);
}

#[tokio::test]
async fn prometheus_clear_buffer_empties_output() {
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
    let before = exporter.export_prometheus().await.unwrap();
    assert!(!before.is_empty());
    exporter.clear_buffer();
    let after = exporter.export_prometheus().await.unwrap();
    assert!(after.is_empty());
}

#[tokio::test]
async fn prometheus_gauge_type_in_output() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "my_gauge".to_string(),
        metric_type: MetricType::Gauge,
        value: 42.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    let output = exporter.export_prometheus().await.unwrap();
    assert!(
        output.contains("# TYPE my_gauge gauge"),
        "output: {output}"
    );
}

#[tokio::test]
async fn prometheus_summary_type_in_output() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "my_summary".to_string(),
        metric_type: MetricType::Summary,
        value: 0.99,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    let output = exporter.export_prometheus().await.unwrap();
    assert!(
        output.contains("# TYPE my_summary summary"),
        "output: {output}"
    );
}

fn build_system_metrics() -> harness::monitoring::SystemMetrics {
    use harness::monitoring::*;
    let mut latencies = HashMap::new();
    latencies.insert(
        "api".to_string(),
        LatencyMetrics {
            avg_latency_ms: 100.0,
            p95_latency_ms: 150.0,
            p99_latency_ms: 200.0,
            max_latency_ms: 300.0,
            min_latency_ms: 20.0,
            request_count: 50,
            requests_per_second: 5.0,
        },
    );
    SystemMetrics {
        timestamp: Utc::now(),
        request_latencies: latencies,
        cache_metrics: CacheMetrics {
            hits: 80,
            misses: 20,
            hit_rate: 0.8,
            size_bytes: 0,
            item_count: 0,
            evictions: 0,
        },
        container_metrics: vec![],
        system_resources: SystemResourceMetrics {
            cpu_usage_percent: 30.0,
            total_memory_bytes: 8_000_000_000,
            used_memory_bytes: 2_000_000_000,
            memory_usage_percent: 25.0,
            available_disk_bytes: 100_000_000_000,
            total_disk_bytes: 200_000_000_000,
            disk_usage_percent: 50.0,
            load_average: [0.5, 0.6, 0.7],
        },
        model_metrics: HashMap::new(),
        error_metrics: ErrorMetrics {
            total_errors: 0,
            errors_by_type: HashMap::new(),
            error_rate: 0.0,
            recent_errors: vec![],
        },
    }
}
