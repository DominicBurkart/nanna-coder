//! Coverage tests for uncovered paths in monitoring.rs, telemetry.rs, and
//! observability.rs.  Each test targets a specific function or branch that
//! was not exercised by the existing inline test suites.

use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
    MonitoringSystem,
};
use harness::monitoring as mon;
use harness::observability::{AlertPolicy, HealthThreshold, ObservabilitySystem};
use harness::telemetry::{
    CustomEvent, MetricPoint, MetricType, PrometheusExporter, SpanStatus, TelemetryConfig,
    TelemetryExporter, TelemetrySystem, TraceContext, TraceGuard,
};
use std::collections::HashMap;
use std::time::Duration;

// ── monitoring.rs: HealthStatus helper methods ───────────────────────────────

#[test]
fn health_status_is_healthy() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[test]
fn health_status_requires_attention() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

// ── monitoring.rs: AlertThresholds::default ──────────────────────────────────

#[test]
fn alert_thresholds_default_non_zero() {
    let t = AlertThresholds::default();
    assert!(t.max_latency_ms > 0);
    assert!(t.min_cache_hit_rate > 0.0);
    assert!(t.max_error_rate > 0.0);
    assert!(t.max_cpu_usage > 0.0);
    assert!(t.max_memory_usage > 0.0);
}

// ── monitoring.rs: MetricsCollector::record_error ────────────────────────────

#[tokio::test]
async fn metrics_collector_record_error_visible_in_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    let event = mon::ErrorEvent {
        timestamp: chrono::Utc::now(),
        error_type: "IoError".to_string(),
        message: "disk full".to_string(),
        component: "storage".to_string(),
        severity: mon::ErrorSeverity::Error,
    };
    collector.record_error(event).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert!(metrics.error_metrics.errors_by_type.contains_key("IoError"));
    // recent_errors contains the latest 10 entries in reverse order
    assert_eq!(metrics.error_metrics.recent_errors.len(), 1);
}

// ── monitoring.rs: MetricsCollector::record_model_inference ──────────────────

#[tokio::test]
async fn metrics_collector_record_model_inference_visible_in_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    let model_metrics = mon::ModelMetrics {
        model_name: "qwen3".to_string(),
        inference_count: 5,
        avg_inference_time_ms: 120.0,
        tokens_per_second: 42.0,
        success_rate: 0.98,
        quality_scores: mon::QualityMetrics {
            avg_coherence: 0.85,
            avg_relevance: 0.90,
            consistency: 0.88,
            accuracy_rate: 0.92,
        },
        resource_usage: mon::ModelResourceUsage {
            peak_memory_mb: 256.0,
            avg_cpu_percent: 35.0,
            gpu_utilization_percent: Some(60.0),
        },
    };
    collector
        .record_model_inference("qwen3", model_metrics)
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.model_metrics.contains_key("qwen3"));
}

// ── monitoring.rs: MetricsCollector::reset_metrics ───────────────────────────

#[tokio::test]
async fn metrics_collector_reset_clears_all_data() {
    let mut collector = DefaultMetricsCollector::new();
    collector.record_cache_hit("key").await;
    collector.record_cache_miss("key2").await;
    collector
        .record_request_latency("svc", Duration::from_millis(50))
        .await;

    collector.reset_metrics().await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
    assert!(metrics.request_latencies.is_empty());
}

// ── monitoring.rs: MetricsFormat::Custom error path ──────────────────────────

#[tokio::test]
async fn metrics_export_custom_format_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("protobuf".to_string()))
        .await;
    assert!(
        result.is_err(),
        "Custom format must return an unsupported-format error"
    );
}

// ── monitoring.rs: AlertManager – acknowledge unknown alert ───────────────────

#[tokio::test]
async fn alert_manager_acknowledge_unknown_alert_returns_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("ghost-alert-99").await;
    assert!(result.is_err(), "Acknowledging a non-existent alert must fail");
}

// ── monitoring.rs: AlertManager – configure_thresholds ───────────────────────

#[tokio::test]
async fn alert_manager_configure_thresholds_succeeds() {
    let mut manager = DefaultAlertManager::new();
    let thresholds = AlertThresholds {
        max_latency_ms: 500,
        min_cache_hit_rate: 0.95,
        max_error_rate: 0.02,
        max_cpu_usage: 0.75,
        max_memory_usage: 0.80,
        health_check_timeout: Duration::from_secs(20),
    };
    manager.configure_thresholds(thresholds).await.unwrap();
}

// ── monitoring.rs: AlertManager – get_alert_history ──────────────────────────

#[tokio::test]
async fn alert_manager_get_alert_history_with_limit() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("A1", "desc1", AlertSeverity::Info)
        .await
        .unwrap();
    manager
        .send_alert("A2", "desc2", AlertSeverity::Warning)
        .await
        .unwrap();

    let all = manager.get_alert_history(10).await.unwrap();
    assert_eq!(all.len(), 2);

    let limited = manager.get_alert_history(1).await.unwrap();
    assert_eq!(limited.len(), 1);
}

// ── monitoring.rs: HealthMonitor – set_check_interval ────────────────────────

#[tokio::test]
async fn health_monitor_set_check_interval() {
    let mut monitor = DefaultHealthMonitor::new(Duration::from_secs(60));
    // Should complete without panic.
    monitor.set_check_interval(Duration::from_secs(15));
}

// ── monitoring.rs: MonitoringSystem – start/stop monitoring ──────────────────

#[tokio::test]
async fn monitoring_system_start_and_stop() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    system.stop_monitoring().await;
}

#[tokio::test]
async fn monitoring_system_stop_when_not_started_is_noop() {
    let mut system = MonitoringSystem::new();
    // Stopping a system that was never started must not panic.
    system.stop_monitoring().await;
}

// ── telemetry.rs: TelemetryConfig::default ───────────────────────────────────

#[test]
fn telemetry_config_default_values() {
    let cfg = TelemetryConfig::default();
    assert_eq!(cfg.service.name, "nanna-coder");
    assert!(cfg.enable_logging);
    assert!(cfg.enable_tracing);
    assert!(cfg.enable_metrics);
    assert_eq!(cfg.trace_sample_rate, 1.0);
}

// ── telemetry.rs: TelemetrySystem builder methods ────────────────────────────

#[test]
fn telemetry_system_builder_chain() {
    let t = TelemetrySystem::new()
        .with_service_name("my-svc")
        .with_version("3.0.0")
        .with_environment("staging")
        .with_global_attribute("region", "eu-west-1");
    // get_uptime is tiny since we just built it.
    assert!(t.get_uptime() < Duration::from_secs(1));
    // No metrics yet.
    assert_eq!(t.get_buffered_metrics_count(), 0);
}

#[test]
fn telemetry_system_with_config() {
    let cfg = TelemetryConfig::default();
    let t = TelemetrySystem::new().with_config(cfg);
    assert_eq!(t.get_buffered_metrics_count(), 0);
}

// ── telemetry.rs: TelemetrySystem::add_exporter ──────────────────────────────

#[tokio::test]
async fn telemetry_system_add_exporter() {
    let exporter = Box::new(PrometheusExporter::new(None)) as Box<dyn TelemetryExporter>;
    let t = TelemetrySystem::new().add_exporter(exporter);
    // No data recorded yet; export_all must succeed with no error.
    t.export_all().await.unwrap();
}

// ── telemetry.rs: TelemetrySystem::export_all ────────────────────────────────

#[tokio::test]
async fn telemetry_export_all_no_exporters_clears_buffer() {
    let t = TelemetrySystem::new();
    t.record_counter("c", 1.0, vec![]);
    t.record_gauge("g", 2.0, vec![]);
    assert_eq!(t.get_buffered_metrics_count(), 2);

    t.export_all().await.unwrap();
    // Buffer is drained even when no exporters are configured.
    assert_eq!(t.get_buffered_metrics_count(), 0);
}

#[tokio::test]
async fn telemetry_export_all_with_exporter_drains_buffer() {
    let exporter = Box::new(PrometheusExporter::new(None)) as Box<dyn TelemetryExporter>;
    let t = TelemetrySystem::new().add_exporter(exporter);

    t.record_counter("requests", 5.0, vec![("env", "test")]);
    assert_eq!(t.get_buffered_metrics_count(), 1);

    t.export_all().await.unwrap();
    assert_eq!(t.get_buffered_metrics_count(), 0);
}

// ── telemetry.rs: TraceContext::set_status ───────────────────────────────────

#[test]
fn trace_context_set_status_variants() {
    let mut tc = TraceContext::new("op");
    assert_eq!(tc.status, SpanStatus::InProgress);

    tc.set_status(SpanStatus::Cancelled);
    assert_eq!(tc.status, SpanStatus::Cancelled);

    tc.set_status(SpanStatus::Timeout);
    assert_eq!(tc.status, SpanStatus::Timeout);
}

// ── telemetry.rs: TelemetrySystem::get_prometheus_exporter ───────────────────

#[test]
fn telemetry_system_get_prometheus_exporter_returns_none() {
    let t = TelemetrySystem::new();
    assert!(t.get_prometheus_exporter().is_none());
}

// ── telemetry.rs: PrometheusExporter – trait methods ─────────────────────────

#[tokio::test]
async fn prometheus_exporter_export_finished_trace() {
    let exporter = PrometheusExporter::new(None);
    let mut tc = TraceContext::new("inference");
    tc.finish();
    assert!(tc.duration.is_some());

    exporter.export_traces(vec![tc]).await.unwrap();

    let out = exporter.export_prometheus().await.unwrap();
    assert!(out.contains("trace_duration_seconds"));
}

#[tokio::test]
async fn prometheus_exporter_export_unfinished_trace_skipped() {
    let exporter = PrometheusExporter::new(None);
    let tc = TraceContext::new("pending"); // not finished → no duration
    assert!(tc.duration.is_none());

    exporter.export_traces(vec![tc]).await.unwrap();

    let out = exporter.export_prometheus().await.unwrap();
    assert!(!out.contains("trace_duration_seconds"));
}

#[tokio::test]
async fn prometheus_exporter_export_metrics_adds_to_buffer() {
    let exporter = PrometheusExporter::new(None);
    let m = MetricPoint {
        name: "pushed_metric".to_string(),
        metric_type: MetricType::Gauge,
        value: 7.0,
        timestamp: chrono::Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    };
    exporter.export_metrics(vec![m]).await.unwrap();

    let out = exporter.export_prometheus().await.unwrap();
    assert!(out.contains("pushed_metric"));
    assert!(out.contains('7'));
}

#[tokio::test]
async fn prometheus_exporter_export_events_counted() {
    let exporter = PrometheusExporter::new(None);
    let event = CustomEvent {
        name: "deploy".to_string(),
        timestamp: chrono::Utc::now(),
        category: "ops".to_string(),
        attributes: HashMap::new(),
        data: serde_json::json!({"version": "1.2.3"}),
        trace_context: None,
    };
    exporter.export_events(vec![event]).await.unwrap();

    let out = exporter.export_prometheus().await.unwrap();
    assert!(out.contains("custom_events_total"));
}

#[tokio::test]
async fn prometheus_exporter_export_system_metrics() {
    let exporter = PrometheusExporter::new(Some("http://localhost:9090".to_string()));

    let mut latencies = HashMap::new();
    latencies.insert(
        "api".to_string(),
        mon::LatencyMetrics {
            avg_latency_ms: 80.0,
            p95_latency_ms: 150.0,
            p99_latency_ms: 400.0,
            max_latency_ms: 800.0,
            min_latency_ms: 5.0,
            request_count: 100,
            requests_per_second: 10.0,
        },
    );

    let system_metrics = mon::SystemMetrics {
        timestamp: chrono::Utc::now(),
        request_latencies: latencies,
        cache_metrics: mon::CacheMetrics {
            hits: 80,
            misses: 20,
            hit_rate: 0.8,
            size_bytes: 1024,
            item_count: 50,
            evictions: 3,
        },
        container_metrics: vec![],
        system_resources: mon::SystemResourceMetrics {
            cpu_usage_percent: 40.0,
            total_memory_bytes: 8_000_000_000,
            used_memory_bytes: 3_000_000_000,
            memory_usage_percent: 37.5,
            available_disk_bytes: 50_000_000_000,
            total_disk_bytes: 100_000_000_000,
            disk_usage_percent: 50.0,
            load_average: [1.0, 1.1, 1.2],
        },
        model_metrics: HashMap::new(),
        error_metrics: mon::ErrorMetrics {
            total_errors: 0,
            errors_by_type: HashMap::new(),
            error_rate: 0.0,
            recent_errors: vec![],
        },
    };

    exporter
        .export_system_metrics(system_metrics)
        .await
        .unwrap();

    let out = exporter.export_prometheus().await.unwrap();
    assert!(out.contains("cache_hit_rate"));
    assert!(out.contains("request_duration_seconds"));
    assert!(out.contains("requests_per_second"));
    assert!(out.contains("error_rate"));
}

#[tokio::test]
async fn prometheus_exporter_health_check_returns_true() {
    let exporter = PrometheusExporter::new(None);
    let healthy = exporter.health_check().await.unwrap();
    assert!(healthy);
}

#[tokio::test]
async fn prometheus_exporter_clear_buffer() {
    let exporter = PrometheusExporter::new(None);
    exporter.add_metric(MetricPoint {
        name: "temp".to_string(),
        metric_type: MetricType::Counter,
        value: 1.0,
        timestamp: chrono::Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: None,
    });

    let before = exporter.export_prometheus().await.unwrap();
    assert!(before.contains("temp"));

    exporter.clear_buffer();

    let after = exporter.export_prometheus().await.unwrap();
    assert!(!after.contains("temp"));
}

// ── telemetry.rs: TraceGuard ─────────────────────────────────────────────────

#[tokio::test]
async fn trace_guard_new_and_trace_access() {
    let t = TelemetrySystem::new();
    let trace = t.start_trace("guarded");
    let guard = TraceGuard::new(&t, trace);

    assert!(guard.trace().is_some());
    assert_eq!(guard.trace().unwrap().operation_name, "guarded");
    // Dropping guard calls finish_trace which calls tokio::spawn.
}

#[tokio::test]
async fn trace_guard_record_error_sets_status() {
    let t = TelemetrySystem::new();
    let trace = t.start_trace("failing_op");
    let mut guard = TraceGuard::new(&t, trace);

    guard.record_error("disk full");

    assert_eq!(guard.trace().unwrap().status, SpanStatus::Error);
    assert_eq!(
        guard.trace().unwrap().attributes.get("error"),
        Some(&"disk full".to_string())
    );
}

#[tokio::test]
async fn trace_guard_set_status() {
    let t = TelemetrySystem::new();
    let trace = t.start_trace("cancellable");
    let mut guard = TraceGuard::new(&t, trace);

    guard.set_status(SpanStatus::Cancelled);
    assert_eq!(guard.trace().unwrap().status, SpanStatus::Cancelled);
}

#[tokio::test]
async fn trace_guard_drop_removes_trace_from_active() {
    let t = TelemetrySystem::new();
    let trace = t.start_trace("auto_finish");
    assert_eq!(t.get_active_trace_count(), 1);

    {
        let _guard = TraceGuard::new(&t, trace);
        // Guard is alive; trace is still registered.
    }
    // Guard dropped → finish_trace called → removed from active_traces.
    assert_eq!(t.get_active_trace_count(), 0);
}

// ── observability.rs: builder methods ────────────────────────────────────────

#[test]
fn observability_system_builder_methods() {
    let system = ObservabilitySystem::new()
        .with_service_name("test-svc")
        .with_alert_policy(AlertPolicy::immediate_critical())
        .with_health_thresholds(HealthThreshold::default())
        .with_health_check_interval(Duration::from_secs(5));

    assert!(system.get_uptime() < Duration::from_secs(1));
}

// ── observability.rs: start_monitoring / stop_monitoring ─────────────────────

#[tokio::test]
async fn observability_start_and_stop_monitoring() {
    let mut system = ObservabilitySystem::new()
        // Very long interval so the background loop never fires during the test.
        .with_health_check_interval(Duration::from_secs(3600));

    system.start_monitoring().await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    system.stop_monitoring().await;
}

#[tokio::test]
async fn observability_stop_when_not_running_is_noop() {
    let mut system = ObservabilitySystem::new();
    // Must not panic when there is no background task to cancel.
    system.stop_monitoring().await;
}
