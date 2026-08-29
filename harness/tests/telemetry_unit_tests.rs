//! Unit tests for telemetry module public APIs

use chrono::Utc;
use harness::telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryExporter,
    TelemetrySystem, TraceContext, TraceGuard,
};
use std::collections::HashMap;
use std::time::Duration;

#[tokio::test]
async fn trace_context_with_attribute_builder() {
    let trace = TraceContext::new("test_op")
        .with_attribute("key1", "value1")
        .with_attribute("key2", "value2");
    assert_eq!(
        trace.attributes.get("key1").map(String::as_str),
        Some("value1")
    );
    assert_eq!(
        trace.attributes.get("key2").map(String::as_str),
        Some("value2")
    );
}

#[tokio::test]
async fn trace_context_set_status() {
    let mut trace = TraceContext::new("test_op");
    trace.set_status(SpanStatus::Cancelled);
    assert_eq!(trace.status, SpanStatus::Cancelled);
}

#[tokio::test]
async fn trace_context_record_error_sets_error_status() {
    let mut trace = TraceContext::new("test_op");
    trace.record_error("something broke");
    assert_eq!(trace.status, SpanStatus::Error);
    assert_eq!(
        trace.attributes.get("error").map(String::as_str),
        Some("something broke")
    );
}

#[tokio::test]
async fn trace_context_finish_preserves_error_status() {
    let mut trace = TraceContext::new("test_op");
    trace.record_error("pre-existing error");
    trace.finish();
    assert_eq!(trace.status, SpanStatus::Error);
    assert!(trace.end_time.is_some());
    assert!(trace.duration.is_some());
}

#[tokio::test]
async fn prometheus_exporter_clear_buffer() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "test_metric".to_string(),
        metric_type: MetricType::Counter,
        value: 1.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.add_metric(metric);

    let output = exporter.export_prometheus().await.unwrap();
    assert!(!output.is_empty());

    exporter.clear_buffer();

    let output_after_clear = exporter.export_prometheus().await.unwrap();
    assert!(output_after_clear.is_empty());
}

#[tokio::test]
async fn export_traces_adds_finished_trace_to_buffer() {
    let exporter = PrometheusExporter::new(None);
    let mut trace = TraceContext::new("my_op");
    trace.finish();

    exporter.export_traces(vec![trace]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("trace_duration_seconds"));
}

#[tokio::test]
async fn export_traces_skips_unfinished_traces() {
    let exporter = PrometheusExporter::new(None);
    let trace = TraceContext::new("my_op"); // not finished
    exporter.export_traces(vec![trace]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.is_empty());
}

#[tokio::test]
async fn export_events_creates_counter_metric() {
    let exporter = PrometheusExporter::new(None);
    let event = CustomEvent {
        name: "user_action".to_string(),
        timestamp: Utc::now(),
        category: "ui".to_string(),
        attributes: HashMap::new(),
        data: serde_json::Value::Null,
        trace_context: None,
    };

    exporter.export_events(vec![event]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("custom_events_total"));
}

#[tokio::test]
async fn global_attributes_appear_in_trace() {
    let telemetry = TelemetrySystem::new()
        .with_global_attribute("env", "test")
        .with_global_attribute("region", "us-east-1");

    let trace = telemetry.start_trace("op");
    assert_eq!(
        trace.attributes.get("env").map(String::as_str),
        Some("test")
    );
    assert_eq!(
        trace.attributes.get("region").map(String::as_str),
        Some("us-east-1")
    );
    telemetry.finish_trace(trace);
}

#[tokio::test]
async fn service_info_embedded_in_traces() {
    let telemetry = TelemetrySystem::new()
        .with_service_name("my-service")
        .with_version("2.0.0")
        .with_environment("staging");

    let trace = telemetry.start_trace("op");
    assert_eq!(
        trace.attributes.get("service.name").map(String::as_str),
        Some("my-service")
    );
    assert_eq!(
        trace.attributes.get("service.version").map(String::as_str),
        Some("2.0.0")
    );
    assert_eq!(
        trace
            .attributes
            .get("service.environment")
            .map(String::as_str),
        Some("staging")
    );
    telemetry.finish_trace(trace);
}

#[tokio::test]
async fn trace_guard_drop_finishes_trace() {
    let telemetry = TelemetrySystem::new();
    assert_eq!(telemetry.get_active_trace_count(), 0);

    {
        let trace = telemetry.start_trace("guarded_op");
        assert_eq!(telemetry.get_active_trace_count(), 1);
        let _guard = TraceGuard::new(&telemetry, trace);
        // _guard dropped here at end of block
    }

    assert_eq!(telemetry.get_active_trace_count(), 0);
}

#[tokio::test]
async fn prometheus_exporter_health_check() {
    let exporter = PrometheusExporter::new(None);
    let result = exporter.health_check().await.unwrap();
    assert!(result);
}

#[tokio::test]
async fn metric_type_format_strings() {
    let exporter = PrometheusExporter::new(None);

    let cases: &[(MetricType, &str)] = &[
        (MetricType::Counter, "counter"),
        (MetricType::Gauge, "gauge"),
        (MetricType::Histogram, "histogram"),
        (MetricType::Summary, "summary"),
    ];

    for (metric_type, expected) in cases {
        exporter.clear_buffer();
        let metric = MetricPoint {
            name: "type_test".to_string(),
            metric_type: metric_type.clone(),
            value: 1.0,
            timestamp: Utc::now(),
            labels: HashMap::new(),
            unit: None,
            description: None,
        };
        exporter.add_metric(metric);
        let output = exporter.export_prometheus().await.unwrap();
        assert!(
            output.contains(&format!("# TYPE type_test {}", expected)),
            "expected TYPE line for {}",
            expected
        );
    }
}
