use chrono::Utc;
use harness::monitoring::{CacheMetrics, ErrorMetrics, LatencyMetrics, SystemMetrics, SystemResourceMetrics};
use harness::telemetry::{
    CustomEvent, ExportEndpoints, MetricPoint, MetricType, PrometheusExporter, ServiceInfo,
    SpanStatus, TelemetryConfig, TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn test_trace_context_with_attribute() {
    let trace = TraceContext::new("test_op")
        .with_attribute("key1", "val1")
        .with_attribute("key2", "val2");
    assert_eq!(trace.attributes.get("key1"), Some(&"val1".to_string()));
    assert_eq!(trace.attributes.get("key2"), Some(&"val2".to_string()));
}

#[test]
fn test_trace_context_set_status_variants() {
    let mut trace = TraceContext::new("op");
    trace.set_status(SpanStatus::Cancelled);
    assert_eq!(trace.status, SpanStatus::Cancelled);
    trace.set_status(SpanStatus::Timeout);
    assert_eq!(trace.status, SpanStatus::Timeout);
    trace.set_status(SpanStatus::Ok);
    assert_eq!(trace.status, SpanStatus::Ok);
}

#[test]
fn test_telemetry_builder_with_service_name_version_environment() {
    let system = TelemetrySystem::new()
        .with_service_name("my-service")
        .with_version("2.0.0")
        .with_environment("production");
    drop(system);
}

#[test]
fn test_telemetry_builder_with_global_attribute() {
    let system = TelemetrySystem::new()
        .with_global_attribute("region", "us-east-1")
        .with_global_attribute("team", "platform");
    drop(system);
}

#[test]
fn test_telemetry_builder_with_config() {
    let config = TelemetryConfig {
        service: ServiceInfo {
            name: "custom".to_string(),
            version: "1.0.0".to_string(),
            environment: "staging".to_string(),
            instance_id: "test-instance".to_string(),
            metadata: HashMap::new(),
        },
        enable_logging: false,
        enable_tracing: true,
        enable_metrics: true,
        log_level: "debug".to_string(),
        metrics_export_interval: Duration::from_secs(30),
        trace_sample_rate: 0.5,
        export_endpoints: ExportEndpoints {
            prometheus_endpoint: None,
            otlp_endpoint: None,
            webhook_endpoints: Vec::new(),
            log_endpoint: None,
        },
        global_attributes: HashMap::new(),
    };
    let system = TelemetrySystem::new().with_config(config);
    drop(system);
}

#[test]
fn test_telemetry_builder_add_exporter() {
    let exporter = PrometheusExporter::new(Some("http://localhost:9090".to_string()));
    let system = TelemetrySystem::new().add_exporter(Box::new(exporter));
    drop(system);
}

#[test]
fn test_telemetry_get_uptime() {
    let system = TelemetrySystem::new();
    let uptime = system.get_uptime();
    assert!(uptime < Duration::from_secs(1));
}

#[test]
fn test_get_prometheus_exporter_returns_none() {
    let system = TelemetrySystem::new();
    assert!(system.get_prometheus_exporter().is_none());
}

#[tokio::test]
async fn test_export_all_no_exporters_no_data() {
    let system = TelemetrySystem::new();
    let result = system.export_all().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_export_all_no_exporters_with_data() {
    let system = TelemetrySystem::new();
    system.record_counter("counter", 1.0, vec![]);
    system.record_event("event", "cat", serde_json::json!({}));
    let result = system.export_all().await;
    assert!(result.is_ok());
    // After export_all, buffers are cleared
    assert_eq!(system.get_buffered_metrics_count(), 0);
}

#[tokio::test]
async fn test_export_all_with_exporter_and_data() {
    let exporter = PrometheusExporter::new(None);
    let system = TelemetrySystem::new().add_exporter(Box::new(exporter));
    system.record_counter("my_counter", 5.0, vec![("env", "test")]);
    system.record_event("deploy", "operations", serde_json::json!({"version": "1.0"}));
    let result = system.export_all().await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_prometheus_export_traces_with_finished_trace() {
    let exporter = PrometheusExporter::new(None);
    let mut trace = TraceContext::new("test_trace");
    trace.finish();
    let result = exporter.export_traces(vec![trace]).await;
    assert!(result.is_ok());
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("trace_duration_seconds"));
}

#[tokio::test]
async fn test_prometheus_export_traces_without_duration() {
    let exporter = PrometheusExporter::new(None);
    let trace = TraceContext::new("unfinished_trace");
    // Not finished, so duration is None → not added to buffer
    let result = exporter.export_traces(vec![trace]).await;
    assert!(result.is_ok());
    let output = exporter.export_prometheus().await.unwrap();
    assert!(!output.contains("trace_duration_seconds"));
}

#[tokio::test]
async fn test_prometheus_export_metrics_trait_method() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "gauge_metric".to_string(),
        metric_type: MetricType::Gauge,
        value: 7.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    let result = exporter.export_metrics(vec![metric]).await;
    assert!(result.is_ok());
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("gauge_metric 7"));
}

#[tokio::test]
async fn test_prometheus_export_events_trait_method() {
    let exporter = PrometheusExporter::new(None);
    let event = CustomEvent {
        name: "test_event".to_string(),
        timestamp: Utc::now(),
        category: "testing".to_string(),
        attributes: HashMap::new(),
        data: serde_json::json!({}),
        trace_context: None,
    };
    let result = exporter.export_events(vec![event]).await;
    assert!(result.is_ok());
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("custom_events_total"));
    assert!(output.contains("event_name=\"test_event\""));
}

#[tokio::test]
async fn test_prometheus_export_system_metrics_empty_latencies() {
    let exporter = PrometheusExporter::new(None);
    let metrics = SystemMetrics {
        timestamp: Utc::now(),
        request_latencies: HashMap::new(),
        cache_metrics: CacheMetrics {
            hits: 80,
            misses: 20,
            hit_rate: 0.8,
            size_bytes: 1024,
            item_count: 100,
            evictions: 5,
        },
        container_metrics: Vec::new(),
        system_resources: SystemResourceMetrics {
            cpu_usage_percent: 50.0,
            total_memory_bytes: 8589934592,
            used_memory_bytes: 4294967296,
            memory_usage_percent: 50.0,
            available_disk_bytes: 107374182400,
            total_disk_bytes: 214748364800,
            disk_usage_percent: 50.0,
            load_average: [1.0, 1.2, 1.1],
        },
        model_metrics: HashMap::new(),
        error_metrics: ErrorMetrics {
            total_errors: 0,
            errors_by_type: HashMap::new(),
            error_rate: 0.0,
            recent_errors: Vec::new(),
        },
    };
    let result = exporter.export_system_metrics(metrics).await;
    assert!(result.is_ok());
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("cache_hit_rate"));
    assert!(output.contains("error_rate"));
}

#[tokio::test]
async fn test_prometheus_export_system_metrics_with_latencies() {
    let exporter = PrometheusExporter::new(None);
    let mut latencies = HashMap::new();
    latencies.insert(
        "api".to_string(),
        LatencyMetrics {
            avg_latency_ms: 100.0,
            p95_latency_ms: 150.0,
            p99_latency_ms: 200.0,
            max_latency_ms: 300.0,
            min_latency_ms: 10.0,
            request_count: 1000,
            requests_per_second: 50.0,
        },
    );
    let metrics = SystemMetrics {
        timestamp: Utc::now(),
        request_latencies: latencies,
        cache_metrics: CacheMetrics {
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            size_bytes: 0,
            item_count: 0,
            evictions: 0,
        },
        container_metrics: Vec::new(),
        system_resources: SystemResourceMetrics {
            cpu_usage_percent: 20.0,
            total_memory_bytes: 8589934592,
            used_memory_bytes: 1073741824,
            memory_usage_percent: 12.5,
            available_disk_bytes: 107374182400,
            total_disk_bytes: 214748364800,
            disk_usage_percent: 50.0,
            load_average: [0.5, 0.7, 0.6],
        },
        model_metrics: HashMap::new(),
        error_metrics: ErrorMetrics {
            total_errors: 0,
            errors_by_type: HashMap::new(),
            error_rate: 0.0,
            recent_errors: Vec::new(),
        },
    };
    let result = exporter.export_system_metrics(metrics).await;
    assert!(result.is_ok());
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("request_duration_seconds"));
    assert!(output.contains("requests_per_second"));
}

#[tokio::test]
async fn test_prometheus_health_check() {
    let exporter = PrometheusExporter::new(None);
    let result = exporter.health_check().await;
    assert!(result.unwrap());
}

#[tokio::test]
async fn test_prometheus_clear_buffer() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "to_clear".to_string(),
        metric_type: MetricType::Counter,
        value: 1.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.add_metric(metric);
    exporter.clear_buffer();
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.is_empty());
}

#[tokio::test]
async fn test_prometheus_summary_metric_type() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "summary_metric".to_string(),
        metric_type: MetricType::Summary,
        value: 0.5,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("# TYPE summary_metric summary"));
}

#[tokio::test]
async fn test_prometheus_histogram_metric_type() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "histogram_metric".to_string(),
        metric_type: MetricType::Histogram,
        value: 1.5,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("# TYPE histogram_metric histogram"));
}

#[tokio::test]
async fn test_prometheus_metric_no_description() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "nodesc".to_string(),
        metric_type: MetricType::Gauge,
        value: 1.5,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    let output = exporter.export_prometheus().await.unwrap();
    assert!(!output.contains("# HELP nodesc"));
    assert!(output.contains("nodesc 1.5"));
}

#[tokio::test]
async fn test_prometheus_metric_empty_labels() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "nolabels".to_string(),
        metric_type: MetricType::Counter,
        value: 42.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    let output = exporter.export_prometheus().await.unwrap();
    // Empty labels → no braces in output
    assert!(output.contains("nolabels 42"));
    assert!(!output.contains("nolabels{"));
}

#[test]
fn test_trace_guard_new_and_trace_access() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("guarded_op");
    let guard = TraceGuard::new(&system, trace);
    assert!(guard.trace().is_some());
    assert_eq!(guard.trace().unwrap().operation_name, "guarded_op");
    // drop → finish_trace called automatically
}

#[test]
fn test_trace_guard_record_error() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("error_op");
    let mut guard = TraceGuard::new(&system, trace);
    guard.record_error("something bad");
    assert_eq!(guard.trace().unwrap().status, SpanStatus::Error);
    assert!(guard.trace().unwrap().attributes.contains_key("error"));
}

#[test]
fn test_trace_guard_set_status() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("cancel_op");
    let mut guard = TraceGuard::new(&system, trace);
    guard.set_status(SpanStatus::Cancelled);
    assert_eq!(guard.trace().unwrap().status, SpanStatus::Cancelled);
}

#[test]
fn test_trace_guard_drop_calls_finish() {
    let system = TelemetrySystem::new();
    let trace = system.start_trace("drop_op");
    assert_eq!(system.get_active_trace_count(), 1);
    {
        let _guard = TraceGuard::new(&system, trace);
    } // guard dropped here → finish_trace called
    assert_eq!(system.get_active_trace_count(), 0);
}

#[test]
fn test_trace_span_macro_basic() {
    // TraceGuard must be in scope where the macro expands
    #[allow(unused_imports)]
    use harness::telemetry::TraceGuard;
    let system = TelemetrySystem::new();
    let _guard = harness::trace_span!(&system, "macro_basic_op");
}

#[test]
fn test_trace_span_macro_with_attributes() {
    #[allow(unused_imports)]
    use harness::telemetry::TraceGuard;
    let system = TelemetrySystem::new();
    let _guard = harness::trace_span!(&system, "macro_attr_op", "key" => "value", "env" => "test");
}
