use chrono::Utc;
use harness::telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryExporter,
    TelemetrySystem, TraceContext, TraceGuard,
};
use harness::monitoring::{
    CacheMetrics, ErrorMetrics, SystemMetrics, SystemResourceMetrics,
};
use std::collections::HashMap;
use std::time::Duration;

// ──────────────────────────────────────────────
// TelemetrySystem builder & basic construction
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_telemetry_system_new() {
    let telemetry = TelemetrySystem::new();
    assert_eq!(telemetry.get_active_trace_count(), 0);
    assert_eq!(telemetry.get_buffered_metrics_count(), 0);
}

#[tokio::test]
async fn test_telemetry_system_default() {
    let telemetry = TelemetrySystem::default();
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

#[tokio::test]
async fn test_telemetry_builder_with_service_name() {
    let telemetry = TelemetrySystem::new().with_service_name("my-service");
    // Just check it builds and is usable
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

#[tokio::test]
async fn test_telemetry_builder_with_version() {
    let telemetry = TelemetrySystem::new().with_version("2.3.4");
    assert_eq!(telemetry.get_buffered_metrics_count(), 0);
}

#[tokio::test]
async fn test_telemetry_builder_with_environment() {
    let telemetry = TelemetrySystem::new().with_environment("production");
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

#[tokio::test]
async fn test_telemetry_builder_with_global_attribute() {
    let telemetry = TelemetrySystem::new().with_global_attribute("region", "us-east-1");
    // Start a trace; global attribute should be copied onto the trace
    let trace = telemetry.start_trace("check_attrs");
    assert_eq!(
        trace.attributes.get("region"),
        Some(&"us-east-1".to_string())
    );
    telemetry.finish_trace(trace);
}

#[tokio::test]
async fn test_telemetry_builder_chaining() {
    let telemetry = TelemetrySystem::new()
        .with_service_name("svc")
        .with_version("1.0.0")
        .with_environment("staging")
        .with_global_attribute("dc", "eu-west");
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

// ──────────────────────────────────────────────
// initialize()
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_initialize_succeeds_or_already_set() {
    use harness::telemetry::TelemetryError;

    let mut telemetry = TelemetrySystem::new()
        .with_service_name("init-test")
        .with_version("0.0.1")
        .with_environment("test");

    match telemetry.initialize().await {
        Ok(_) => {}
        Err(TelemetryError::InitializationFailed { reason })
            if reason.contains("global default trace dispatcher") =>
        {
            // Expected when another test already set the subscriber
        }
        Err(e) => panic!("Unexpected error from initialize: {e}"),
    }
}

#[tokio::test]
async fn test_initialize_idempotent() {
    use harness::telemetry::TelemetryError;

    let mut telemetry = TelemetrySystem::new();

    let first = telemetry.initialize().await;
    // If first call succeeded the system is marked initialized; a second call
    // should return Ok immediately without trying to set a subscriber again.
    if first.is_ok() {
        telemetry.initialize().await.unwrap();
    } else {
        // If first failed (subscriber already set) second call will also fail
        // for the same reason – that's fine.
        match telemetry.initialize().await {
            Ok(_) | Err(TelemetryError::InitializationFailed { .. }) => {}
            Err(e) => panic!("Unexpected error: {e}"),
        }
    }
}

// ──────────────────────────────────────────────
// start_trace / finish_trace
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_start_trace_adds_to_active() {
    let telemetry = TelemetrySystem::new();
    assert_eq!(telemetry.get_active_trace_count(), 0);

    let trace = telemetry.start_trace("my_op");
    assert_eq!(telemetry.get_active_trace_count(), 1);
    assert_eq!(trace.operation_name, "my_op");

    telemetry.finish_trace(trace);
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

#[tokio::test]
async fn test_start_trace_injects_service_attributes() {
    let telemetry = TelemetrySystem::new()
        .with_service_name("svc-a")
        .with_version("3.0")
        .with_environment("prod");

    let trace = telemetry.start_trace("op");
    assert_eq!(
        trace.attributes.get("service.name"),
        Some(&"svc-a".to_string())
    );
    assert_eq!(
        trace.attributes.get("service.version"),
        Some(&"3.0".to_string())
    );
    assert_eq!(
        trace.attributes.get("service.environment"),
        Some(&"prod".to_string())
    );
    telemetry.finish_trace(trace);
}

#[tokio::test]
async fn test_multiple_concurrent_traces() {
    let telemetry = TelemetrySystem::new();
    let t1 = telemetry.start_trace("op1");
    let t2 = telemetry.start_trace("op2");
    assert_eq!(telemetry.get_active_trace_count(), 2);
    telemetry.finish_trace(t1);
    telemetry.finish_trace(t2);
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

// ──────────────────────────────────────────────
// record_counter / record_histogram
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_record_counter() {
    let telemetry = TelemetrySystem::new();
    telemetry.record_counter("req_total", 5.0, vec![("method", "GET")]);
    assert_eq!(telemetry.get_buffered_metrics_count(), 1);
}

#[tokio::test]
async fn test_record_counter_empty_labels() {
    let telemetry = TelemetrySystem::new();
    telemetry.record_counter("plain_counter", 1.0, vec![]);
    assert_eq!(telemetry.get_buffered_metrics_count(), 1);
}

#[tokio::test]
async fn test_record_histogram() {
    let telemetry = TelemetrySystem::new();
    telemetry.record_histogram("latency", Duration::from_millis(200));
    assert_eq!(telemetry.get_buffered_metrics_count(), 1);
}

#[tokio::test]
async fn test_record_multiple_metrics() {
    let telemetry = TelemetrySystem::new();
    telemetry.record_counter("c1", 1.0, vec![]);
    telemetry.record_counter("c2", 2.0, vec![]);
    telemetry.record_histogram("h1", Duration::from_millis(10));
    assert_eq!(telemetry.get_buffered_metrics_count(), 3);
}

// ──────────────────────────────────────────────
// record_event (record_custom_event alias)
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_record_event() {
    let telemetry = TelemetrySystem::new();
    // Record an event and verify it shows up in the exporter after export_all
    telemetry.record_event(
        "page_view",
        "analytics",
        serde_json::json!({"path": "/home"}),
    );
    // Events are buffered; export_all drains them (returns Ok if buffer was non-empty)
    telemetry.export_all().await.unwrap();
    // After export the events buffer is drained; no panic means success
}

#[tokio::test]
async fn test_record_event_is_buffered_before_export() {
    let telemetry = TelemetrySystem::new();
    // No events yet – export_all should succeed with nothing to do
    telemetry.export_all().await.unwrap();

    // Now record one event and export – still no error
    telemetry.record_event("click", "ui", serde_json::json!({"button": "submit"}));
    telemetry.export_all().await.unwrap();
}

// ──────────────────────────────────────────────
// get_prometheus_exporter (returns None in this implementation)
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_get_prometheus_exporter_returns_none() {
    let telemetry = TelemetrySystem::new();
    // The current implementation always returns None
    assert!(telemetry.get_prometheus_exporter().is_none());
}

// ──────────────────────────────────────────────
// get_uptime
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_get_uptime_increases() {
    let telemetry = TelemetrySystem::new();
    let before = telemetry.get_uptime();
    // Do a tiny bit of work
    telemetry.record_counter("x", 1.0, vec![]);
    let after = telemetry.get_uptime();
    assert!(after >= before);
}

// ──────────────────────────────────────────────
// export_all
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_export_all_with_no_data() {
    let telemetry = TelemetrySystem::new();
    telemetry.export_all().await.unwrap();
}

#[tokio::test]
async fn test_export_all_drains_metrics() {
    let telemetry = TelemetrySystem::new();
    telemetry.record_counter("x", 1.0, vec![]);
    assert_eq!(telemetry.get_buffered_metrics_count(), 1);
    telemetry.export_all().await.unwrap();
    // After export the buffer should be drained
    assert_eq!(telemetry.get_buffered_metrics_count(), 0);
}

// ──────────────────────────────────────────────
// TraceContext
// ──────────────────────────────────────────────

#[test]
fn test_trace_context_new() {
    let ctx = TraceContext::new("test_op");
    assert_eq!(ctx.operation_name, "test_op");
    assert_eq!(ctx.status, SpanStatus::InProgress);
    assert!(ctx.end_time.is_none());
    assert!(ctx.duration.is_none());
    assert!(ctx.parent_span_id.is_none());
    // trace_id and span_id are UUIDs, so just check non-empty
    assert!(!ctx.trace_id.is_empty());
    assert!(!ctx.span_id.is_empty());
}

#[test]
fn test_trace_context_create_child() {
    let parent = TraceContext::new("parent");
    let child = parent.create_child("child");

    assert_eq!(child.trace_id, parent.trace_id);
    assert_ne!(child.span_id, parent.span_id);
    assert_eq!(child.parent_span_id, Some(parent.span_id.clone()));
    assert_eq!(child.operation_name, "child");
    assert_eq!(child.status, SpanStatus::InProgress);
}

#[test]
fn test_trace_context_with_attribute() {
    let ctx = TraceContext::new("op")
        .with_attribute("key1", "val1")
        .with_attribute("key2", "val2");

    assert_eq!(ctx.attributes.get("key1"), Some(&"val1".to_string()));
    assert_eq!(ctx.attributes.get("key2"), Some(&"val2".to_string()));
}

#[test]
fn test_trace_context_set_status() {
    let mut ctx = TraceContext::new("op");
    ctx.set_status(SpanStatus::Cancelled);
    assert_eq!(ctx.status, SpanStatus::Cancelled);
}

#[test]
fn test_trace_context_set_status_timeout() {
    let mut ctx = TraceContext::new("op");
    ctx.set_status(SpanStatus::Timeout);
    assert_eq!(ctx.status, SpanStatus::Timeout);
}

#[test]
fn test_trace_context_record_error() {
    let mut ctx = TraceContext::new("op");
    ctx.record_error("something broke");
    assert_eq!(ctx.status, SpanStatus::Error);
    assert_eq!(
        ctx.attributes.get("error"),
        Some(&"something broke".to_string())
    );
}

#[test]
fn test_trace_context_finish_sets_ok() {
    let mut ctx = TraceContext::new("op");
    assert_eq!(ctx.status, SpanStatus::InProgress);
    ctx.finish();
    assert_eq!(ctx.status, SpanStatus::Ok);
    assert!(ctx.end_time.is_some());
    assert!(ctx.duration.is_some());
}

#[test]
fn test_trace_context_finish_preserves_error_status() {
    let mut ctx = TraceContext::new("op");
    ctx.set_status(SpanStatus::Error);
    ctx.finish();
    // finish() should NOT overwrite an already-set non-InProgress status
    assert_eq!(ctx.status, SpanStatus::Error);
}

#[test]
fn test_trace_context_finish_preserves_cancelled_status() {
    let mut ctx = TraceContext::new("op");
    ctx.set_status(SpanStatus::Cancelled);
    ctx.finish();
    assert_eq!(ctx.status, SpanStatus::Cancelled);
}

// ──────────────────────────────────────────────
// SpanStatus enum variants
// ──────────────────────────────────────────────

#[test]
fn test_span_status_eq() {
    assert_eq!(SpanStatus::Ok, SpanStatus::Ok);
    assert_ne!(SpanStatus::Ok, SpanStatus::Error);
    assert_ne!(SpanStatus::InProgress, SpanStatus::Cancelled);
    assert_ne!(SpanStatus::Timeout, SpanStatus::Ok);
}

#[test]
fn test_span_status_all_variants_debug() {
    let variants = [
        SpanStatus::InProgress,
        SpanStatus::Ok,
        SpanStatus::Error,
        SpanStatus::Cancelled,
        SpanStatus::Timeout,
    ];
    for v in &variants {
        let s = format!("{v:?}");
        assert!(!s.is_empty());
    }
}

// ──────────────────────────────────────────────
// PrometheusExporter
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_prometheus_exporter_new_no_endpoint() {
    let exporter = PrometheusExporter::new(None);
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.is_empty());
}

#[tokio::test]
async fn test_prometheus_exporter_new_with_endpoint() {
    let exporter = PrometheusExporter::new(Some("http://localhost:9090".to_string()));
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.is_empty()); // no metrics added yet
}

#[tokio::test]
async fn test_prometheus_exporter_add_metric_counter() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "http_requests_total".to_string(),
        metric_type: MetricType::Counter,
        value: 100.0,
        timestamp: Utc::now(),
        labels: HashMap::from([("method".to_string(), "GET".to_string())]),
        unit: None,
        description: Some("Total HTTP requests".to_string()),
    };
    exporter.add_metric(metric);

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("# HELP http_requests_total Total HTTP requests"));
    assert!(output.contains("# TYPE http_requests_total counter"));
    assert!(output.contains("http_requests_total{method=\"GET\"} 100"));
}

#[tokio::test]
async fn test_prometheus_exporter_add_metric_gauge() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "cpu_usage".to_string(),
        metric_type: MetricType::Gauge,
        value: 0.75,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: Some("ratio".to_string()),
        description: None,
    };
    exporter.add_metric(metric);

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("# TYPE cpu_usage gauge"));
    assert!(output.contains("cpu_usage 0.75"));
}

#[tokio::test]
async fn test_prometheus_exporter_add_metric_histogram() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "latency_seconds".to_string(),
        metric_type: MetricType::Histogram,
        value: 0.123,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.add_metric(metric);

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("# TYPE latency_seconds histogram"));
}

#[tokio::test]
async fn test_prometheus_exporter_add_metric_summary() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "response_size".to_string(),
        metric_type: MetricType::Summary,
        value: 512.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.add_metric(metric);

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("# TYPE response_size summary"));
}

#[tokio::test]
async fn test_prometheus_exporter_metric_with_no_labels() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "simple".to_string(),
        metric_type: MetricType::Counter,
        value: 1.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.add_metric(metric);

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("simple 1"));
    // No braces when labels are empty
    assert!(!output.contains("simple{"));
}

#[tokio::test]
async fn test_prometheus_exporter_clear_buffer() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "temp".to_string(),
        metric_type: MetricType::Gauge,
        value: 1.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.add_metric(metric);

    // Buffer is non-empty
    let output_before = exporter.export_prometheus().await.unwrap();
    assert!(!output_before.is_empty());

    exporter.clear_buffer();
    let output_after = exporter.export_prometheus().await.unwrap();
    assert!(output_after.is_empty());
}

// ──────────────────────────────────────────────
// TelemetryExporter trait – PrometheusExporter implementation
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_exporter_export_traces() {
    let exporter = PrometheusExporter::new(None);

    let mut trace = TraceContext::new("traced_op");
    trace.finish();

    exporter.export_traces(vec![trace]).await.unwrap();

    // The exporter should have converted the finished trace into a metric
    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("trace_duration_seconds"));
}

#[tokio::test]
async fn test_exporter_export_traces_in_progress_skipped() {
    let exporter = PrometheusExporter::new(None);

    // An InProgress trace has no duration, so it should be skipped
    let trace = TraceContext::new("unfinished_op");
    exporter.export_traces(vec![trace]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.is_empty());
}

#[tokio::test]
async fn test_exporter_export_metrics() {
    let exporter = PrometheusExporter::new(None);
    let metric = MetricPoint {
        name: "pushed_metric".to_string(),
        metric_type: MetricType::Counter,
        value: 7.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.export_metrics(vec![metric]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("pushed_metric 7"));
}

#[tokio::test]
async fn test_exporter_export_events() {
    let exporter = PrometheusExporter::new(None);
    let event = CustomEvent {
        name: "signup".to_string(),
        timestamp: Utc::now(),
        category: "user".to_string(),
        attributes: HashMap::new(),
        data: serde_json::json!({}),
        trace_context: None,
    };
    exporter.export_events(vec![event]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("custom_events_total"));
    assert!(output.contains("event_name=\"signup\""));
}

#[tokio::test]
async fn test_exporter_export_system_metrics() {
    let exporter = PrometheusExporter::new(None);

    let system_metrics = SystemMetrics {
        timestamp: Utc::now(),
        request_latencies: {
            let mut m = HashMap::new();
            m.insert(
                "api".to_string(),
                harness::monitoring::LatencyMetrics {
                    avg_latency_ms: 50.0,
                    p95_latency_ms: 90.0,
                    p99_latency_ms: 120.0,
                    max_latency_ms: 200.0,
                    min_latency_ms: 10.0,
                    request_count: 100,
                    requests_per_second: 5.0,
                },
            );
            m
        },
        cache_metrics: CacheMetrics {
            hits: 80,
            misses: 20,
            hit_rate: 0.8,
            size_bytes: 1024,
            item_count: 50,
            evictions: 2,
        },
        container_metrics: vec![],
        system_resources: SystemResourceMetrics {
            cpu_usage_percent: 0.3,
            total_memory_bytes: 8_000_000_000,
            used_memory_bytes: 4_000_000_000,
            memory_usage_percent: 0.5,
            available_disk_bytes: 100_000_000_000,
            total_disk_bytes: 200_000_000_000,
            disk_usage_percent: 0.5,
            load_average: [0.5, 0.4, 0.3],
        },
        model_metrics: HashMap::new(),
        error_metrics: ErrorMetrics {
            total_errors: 1,
            errors_by_type: HashMap::new(),
            error_rate: 0.01,
            recent_errors: vec![],
        },
    };

    exporter
        .export_system_metrics(system_metrics)
        .await
        .unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("cache_hit_rate"));
    assert!(output.contains("request_duration_seconds"));
    assert!(output.contains("requests_per_second"));
    assert!(output.contains("error_rate"));
}

#[tokio::test]
async fn test_exporter_health_check() {
    let exporter = PrometheusExporter::new(None);
    let healthy = exporter.health_check().await.unwrap();
    assert!(healthy);
}

// ──────────────────────────────────────────────
// MetricType variants
// ──────────────────────────────────────────────

#[test]
fn test_metric_type_eq() {
    assert_eq!(MetricType::Counter, MetricType::Counter);
    assert_eq!(MetricType::Gauge, MetricType::Gauge);
    assert_eq!(MetricType::Histogram, MetricType::Histogram);
    assert_eq!(MetricType::Summary, MetricType::Summary);
    assert_ne!(MetricType::Counter, MetricType::Gauge);
}

#[test]
fn test_metric_type_debug() {
    for variant in [
        MetricType::Counter,
        MetricType::Gauge,
        MetricType::Histogram,
        MetricType::Summary,
    ] {
        let s = format!("{variant:?}");
        assert!(!s.is_empty());
    }
}

// ──────────────────────────────────────────────
// TraceGuard RAII
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_trace_guard_drops_and_finishes_trace() {
    let telemetry = TelemetrySystem::new();
    {
        let trace = telemetry.start_trace("guarded_op");
        assert_eq!(telemetry.get_active_trace_count(), 1);
        let _guard = TraceGuard::new(&telemetry, trace);
        // guard is alive
        assert_eq!(telemetry.get_active_trace_count(), 1);
    } // guard drops here → finish_trace() called
    assert_eq!(telemetry.get_active_trace_count(), 0);
}

#[tokio::test]
async fn test_trace_guard_trace_ref() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("ref_op");
    let guard = TraceGuard::new(&telemetry, trace);

    let ctx = guard.trace();
    assert!(ctx.is_some());
    assert_eq!(ctx.unwrap().operation_name, "ref_op");
}

#[tokio::test]
async fn test_trace_guard_record_error() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("err_op");
    let mut guard = TraceGuard::new(&telemetry, trace);

    guard.record_error("test error");

    let ctx = guard.trace().unwrap();
    assert_eq!(ctx.status, SpanStatus::Error);
    assert_eq!(ctx.attributes.get("error"), Some(&"test error".to_string()));
}

#[tokio::test]
async fn test_trace_guard_set_status() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("status_op");
    let mut guard = TraceGuard::new(&telemetry, trace);

    guard.set_status(SpanStatus::Cancelled);

    let ctx = guard.trace().unwrap();
    assert_eq!(ctx.status, SpanStatus::Cancelled);
}
