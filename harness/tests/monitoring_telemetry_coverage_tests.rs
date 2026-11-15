//! Unit tests covering branches in `monitoring.rs` and `telemetry.rs` that
//! were not exercised by the pre-existing test suite.
//!
//! Coverage added:
//! - `MetricsFormat::Custom` → error path in `DefaultMetricsCollector::export_metrics`
//! - `DefaultAlertManager::acknowledge_alert` → not-found error path
//! - `HealthStatus::is_healthy` and `requires_attention` helper methods
//! - `ErrorSeverity` ordering (`PartialOrd` / `Ord`)
//! - All four `AlertSeverity` levels through `send_alert`
//! - `get_alert_history` limit enforcement
//! - `DefaultMetricsCollector::reset_metrics`
//! - `DefaultAlertManager::configure_thresholds`
//! - `AlertThresholds::default`
//! - `DefaultMetricsCollector::record_model_inference` / `record_error`
//! - `PrometheusExporter` trait methods: `export_traces`, `export_events`,
//!   `export_system_metrics`, `health_check`, `clear_buffer`
//! - `TelemetrySystem::export_all`
//! - `TelemetrySystem` builder methods (`with_global_attribute`, `with_version`,
//!   `with_environment`, `with_config`)
//! - `TelemetryConfig::default`
//! - `TraceGuard::trace`, `record_error`, `set_status`

use chrono::Utc;
use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultMetricsCollector,
    ErrorSeverity, HealthStatus, MetricsCollector, MetricsFormat, ModelMetrics, ModelResourceUsage,
    QualityMetrics,
};
use harness::monitoring::{ErrorEvent, LatencyMetrics};
use harness::telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
use std::collections::HashMap;
use std::time::Duration;

// ── monitoring.rs ──────────────────────────────────────────────────────────

#[tokio::test]
async fn export_metrics_custom_format_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let err = collector
        .export_metrics(MetricsFormat::Custom("cbor".to_string()))
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("cbor"), "error should name the format: {msg}");
}

#[tokio::test]
async fn acknowledge_alert_unknown_id_returns_error() {
    let mgr = DefaultAlertManager::new();
    let err = mgr
        .acknowledge_alert("no-such-alert-id")
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("no-such-alert-id"),
        "error should mention the id: {msg}"
    );
}

#[test]
fn health_status_is_healthy_covers_all_variants() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[test]
fn health_status_requires_attention_covers_all_variants() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

#[test]
fn error_severity_total_ordering() {
    assert!(ErrorSeverity::Critical > ErrorSeverity::Error);
    assert!(ErrorSeverity::Error > ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning > ErrorSeverity::Info);
    assert!(ErrorSeverity::Info < ErrorSeverity::Critical);
}

#[tokio::test]
async fn send_alert_covers_all_severity_levels() {
    let mgr = DefaultAlertManager::new();
    mgr.send_alert("a", "d", AlertSeverity::Critical)
        .await
        .unwrap();
    mgr.send_alert("a", "d", AlertSeverity::Error)
        .await
        .unwrap();
    mgr.send_alert("a", "d", AlertSeverity::Warning)
        .await
        .unwrap();
    mgr.send_alert("a", "d", AlertSeverity::Info)
        .await
        .unwrap();

    let history = mgr.get_alert_history(10).await.unwrap();
    assert_eq!(history.len(), 4);
}

#[tokio::test]
async fn get_alert_history_limit_is_respected() {
    let mgr = DefaultAlertManager::new();
    for _ in 0..6 {
        mgr.send_alert("t", "d", AlertSeverity::Info)
            .await
            .unwrap();
    }
    let history = mgr.get_alert_history(3).await.unwrap();
    assert_eq!(history.len(), 3);
}

#[tokio::test]
async fn reset_metrics_clears_all_state() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(50))
        .await;
    collector.record_cache_hit("k").await;
    collector.record_cache_miss("m").await;

    collector.reset_metrics().await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.request_latencies.is_empty());
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
    assert_eq!(metrics.cache_metrics.hit_rate, 0.0);
}

#[tokio::test]
async fn configure_thresholds_succeeds_and_returns_ok() {
    let mut mgr = DefaultAlertManager::new();
    let custom = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.9,
        max_error_rate: 0.01,
        max_cpu_usage: 0.8,
        max_memory_usage: 0.75,
        health_check_timeout: Duration::from_secs(60),
    };
    mgr.configure_thresholds(custom).await.unwrap();
}

#[test]
fn alert_thresholds_default_values_are_valid() {
    let t = AlertThresholds::default();
    assert!(t.max_latency_ms > 0);
    assert!((0.0..=1.0).contains(&t.min_cache_hit_rate));
    assert!((0.0..=1.0).contains(&t.max_error_rate));
    assert!((0.0..=1.0).contains(&t.max_cpu_usage));
    assert!((0.0..=1.0).contains(&t.max_memory_usage));
    assert!(t.health_check_timeout > Duration::ZERO);
}

#[tokio::test]
async fn record_model_inference_shows_up_in_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    let mm = ModelMetrics {
        model_name: "qwen3:0.6b".to_string(),
        inference_count: 5,
        avg_inference_time_ms: 80.0,
        tokens_per_second: 120.0,
        success_rate: 1.0,
        quality_scores: QualityMetrics {
            avg_coherence: 0.85,
            avg_relevance: 0.90,
            consistency: 0.88,
            accuracy_rate: 0.92,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 256.0,
            avg_cpu_percent: 20.0,
            gpu_utilization_percent: None,
        },
    };
    collector
        .record_model_inference("qwen3:0.6b", mm)
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(
        metrics.model_metrics.contains_key("qwen3:0.6b"),
        "model key missing from metrics map"
    );
}

#[tokio::test]
async fn record_error_increments_error_count_and_type_map() {
    let mut collector = DefaultMetricsCollector::new();
    let ev = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "NetworkTimeout".to_string(),
        message: "connection timed out".to_string(),
        component: "http-client".to_string(),
        severity: ErrorSeverity::Error,
    };
    collector.record_error(ev).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert_eq!(
        metrics.error_metrics.errors_by_type.get("NetworkTimeout"),
        Some(&1u64)
    );
}

// ── telemetry.rs ───────────────────────────────────────────────────────────

#[tokio::test]
async fn prometheus_exporter_export_traces_adds_histogram_metric() {
    let exporter = PrometheusExporter::new(None);
    let mut trace = TraceContext::new("db-query");
    trace.finish();

    exporter.export_traces(vec![trace]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(
        output.contains("trace_duration_seconds"),
        "expected trace histogram in output: {output}"
    );
}

#[tokio::test]
async fn prometheus_exporter_export_events_counts_events() {
    let exporter = PrometheusExporter::new(None);
    let event = CustomEvent {
        name: "user_signup".to_string(),
        timestamp: Utc::now(),
        category: "auth".to_string(),
        attributes: HashMap::new(),
        data: serde_json::json!({"method": "email"}),
        trace_context: None,
    };

    exporter.export_events(vec![event]).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(
        output.contains("custom_events_total"),
        "expected event counter in output: {output}"
    );
    assert!(
        output.contains("user_signup"),
        "expected event name in labels: {output}"
    );
}

#[tokio::test]
async fn prometheus_exporter_export_system_metrics_emits_cache_and_latency() {
    use harness::monitoring::{
        CacheMetrics, ErrorMetrics, SystemMetrics, SystemResourceMetrics,
    };

    let exporter = PrometheusExporter::new(None);
    let mut latencies = HashMap::new();
    latencies.insert(
        "api".to_string(),
        LatencyMetrics {
            avg_latency_ms: 45.0,
            p95_latency_ms: 80.0,
            p99_latency_ms: 150.0,
            max_latency_ms: 200.0,
            min_latency_ms: 5.0,
            request_count: 200,
            requests_per_second: 20.0,
        },
    );
    let sys = SystemMetrics {
        timestamp: Utc::now(),
        request_latencies: latencies,
        cache_metrics: CacheMetrics {
            hits: 180,
            misses: 20,
            hit_rate: 0.9,
            size_bytes: 4096,
            item_count: 100,
            evictions: 2,
        },
        container_metrics: vec![],
        system_resources: SystemResourceMetrics {
            cpu_usage_percent: 35.0,
            total_memory_bytes: 8_000_000_000,
            used_memory_bytes: 3_000_000_000,
            memory_usage_percent: 37.5,
            available_disk_bytes: 50_000_000_000,
            total_disk_bytes: 100_000_000_000,
            disk_usage_percent: 50.0,
            load_average: [0.8, 0.9, 1.0],
        },
        model_metrics: HashMap::new(),
        error_metrics: ErrorMetrics {
            total_errors: 0,
            errors_by_type: HashMap::new(),
            error_rate: 0.0,
            recent_errors: vec![],
        },
    };

    exporter.export_system_metrics(sys).await.unwrap();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.contains("cache_hit_rate"), "missing cache_hit_rate");
    assert!(
        output.contains("request_duration_seconds"),
        "missing request_duration_seconds"
    );
    assert!(
        output.contains("requests_per_second"),
        "missing requests_per_second"
    );
}

#[tokio::test]
async fn prometheus_exporter_health_check_returns_true() {
    let exporter = PrometheusExporter::new(None);
    assert!(exporter.health_check().await.unwrap());
}

#[tokio::test]
async fn prometheus_exporter_clear_buffer_empties_output() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "dummy".to_string(),
        metric_type: MetricType::Gauge,
        value: 1.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });
    exporter.clear_buffer();

    let output = exporter.export_prometheus().await.unwrap();
    assert!(output.is_empty(), "buffer should be empty after clear");
}

#[tokio::test]
async fn telemetry_system_export_all_drains_metrics_buffer() {
    let telemetry = TelemetrySystem::new();
    telemetry.record_counter("reqs", 1.0, vec![("service", "api")]);
    telemetry.record_gauge("mem_mb", 512.0, vec![]);
    telemetry.record_histogram("latency", Duration::from_millis(80));
    telemetry.record_event("deploy", "ops", serde_json::json!({"version": "v2"}));

    assert_eq!(telemetry.get_buffered_metrics_count(), 3);

    telemetry.export_all().await.unwrap();

    // After export the buffer must be empty (no exporters configured, so
    // nothing was uploaded, but the local buffer is still drained).
    assert_eq!(telemetry.get_buffered_metrics_count(), 0);
}

#[test]
fn telemetry_system_builder_chain_compiles_and_runs() {
    let t = TelemetrySystem::new()
        .with_service_name("coverage-service")
        .with_version("0.0.1")
        .with_environment("ci")
        .with_global_attribute("datacenter", "aws-us-east-1");

    assert_eq!(t.get_active_trace_count(), 0);
    assert_eq!(t.get_buffered_metrics_count(), 0);
}

#[test]
fn telemetry_system_with_config_applies_config() {
    let mut config = TelemetryConfig::default();
    config.service.name = "custom-service".to_string();

    let t = TelemetrySystem::new().with_config(config);
    // We can't easily inspect internal config, but the chain must compile and
    // not panic.
    let _ = t.get_uptime();
}

#[test]
fn telemetry_config_default_has_valid_fields() {
    let c = TelemetryConfig::default();
    assert!(!c.service.name.is_empty());
    assert!(!c.service.version.is_empty());
    assert!(!c.service.environment.is_empty());
    assert!(!c.service.instance_id.is_empty(), "UUID must be generated");
    assert!(c.trace_sample_rate >= 0.0 && c.trace_sample_rate <= 1.0);
    assert!(c.enable_logging);
    assert!(c.enable_tracing);
    assert!(c.enable_metrics);
}

#[tokio::test]
async fn trace_guard_record_error_sets_error_status() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("failing-op");
    let mut guard = TraceGuard::new(&telemetry, trace);

    assert!(guard.trace().is_some());
    assert_eq!(guard.trace().unwrap().status, SpanStatus::InProgress);

    guard.record_error("disk full");

    assert_eq!(
        guard.trace().unwrap().status,
        SpanStatus::Error,
        "recording an error must set status to Error"
    );
    // Drop auto-finishes the trace via telemetry.finish_trace (needs Tokio runtime)
}

#[tokio::test]
async fn trace_guard_set_status_overrides_status() {
    let telemetry = TelemetrySystem::new();
    let trace = telemetry.start_trace("timed-op");
    let mut guard = TraceGuard::new(&telemetry, trace);

    guard.set_status(SpanStatus::Timeout);

    assert_eq!(
        guard.trace().unwrap().status,
        SpanStatus::Timeout,
        "set_status must update the inner trace context"
    );
    // Drop needs Tokio runtime for finish_trace
}

#[test]
fn trace_context_create_child_inherits_trace_id() {
    let parent = TraceContext::new("parent-op");
    let child = parent.create_child("child-op");

    assert_eq!(child.trace_id, parent.trace_id, "child must share trace_id");
    assert_eq!(
        child.parent_span_id,
        Some(parent.span_id.clone()),
        "child must reference parent span_id"
    );
    assert_ne!(child.span_id, parent.span_id, "child must have its own span_id");
}
