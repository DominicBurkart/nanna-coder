//! Unit tests for harness::monitoring public API.
//!
//! Covers paths not exercised by the existing inline tests in monitoring.rs:
//! helper methods, Ord impls, default values, reset, Custom export format,
//! latency percentile calculation, error recording, alert history limits,
//! acknowledge error path, configure_thresholds, model health check, and
//! the MonitoringSystem start/stop cycle.

use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
    MonitoringError, MonitoringSystem,
};
use harness::monitoring::{ErrorEvent, ErrorSeverity};
use std::time::Duration;

// ---------------------------------------------------------------------------
// HealthStatus helper methods
// ---------------------------------------------------------------------------

#[test]
fn health_status_is_healthy_only_for_healthy_variant() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[test]
fn health_status_requires_attention_for_warning_degraded_unhealthy() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    // Unknown does not require_attention per current impl
    assert!(!HealthStatus::Unknown.requires_attention());
}

// ---------------------------------------------------------------------------
// ErrorSeverity ordering (PartialOrd / Ord)
// ---------------------------------------------------------------------------

#[test]
fn error_severity_ordering_is_strictly_ascending() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
    assert_eq!(ErrorSeverity::Info, ErrorSeverity::Info);
}

// ---------------------------------------------------------------------------
// AlertSeverity ordering
// ---------------------------------------------------------------------------

#[test]
fn alert_severity_ordering_is_strictly_ascending() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
    assert_eq!(AlertSeverity::Critical, AlertSeverity::Critical);
}

// ---------------------------------------------------------------------------
// AlertThresholds::default field values
// ---------------------------------------------------------------------------

#[test]
fn alert_thresholds_default_values_are_sensible() {
    let t = AlertThresholds::default();
    assert_eq!(t.max_latency_ms, 5000);
    assert!((t.min_cache_hit_rate - 0.8).abs() < f64::EPSILON);
    assert!((t.max_error_rate - 0.05).abs() < f64::EPSILON);
    assert!((t.max_cpu_usage - 0.9).abs() < f64::EPSILON);
    assert!((t.max_memory_usage - 0.9).abs() < f64::EPSILON);
    assert_eq!(t.health_check_timeout, Duration::from_secs(30));
}

// ---------------------------------------------------------------------------
// DefaultMetricsCollector::reset_metrics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn metrics_reset_clears_latencies_cache_and_errors() {
    let mut collector = DefaultMetricsCollector::new();

    collector
        .record_request_latency("svc", Duration::from_millis(50))
        .await;
    collector.record_cache_hit("k1").await;
    collector.record_cache_miss("k2").await;

    collector.reset_metrics().await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.request_latencies.is_empty(), "latencies must be cleared");
    assert_eq!(metrics.cache_metrics.hits, 0, "cache hits must be cleared");
    assert_eq!(metrics.cache_metrics.misses, 0, "cache misses must be cleared");
    assert_eq!(
        metrics.error_metrics.total_errors, 0,
        "error count must be cleared"
    );
}

// ---------------------------------------------------------------------------
// MetricsFormat::Custom error path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn export_metrics_custom_format_returns_collection_error() {
    let collector = DefaultMetricsCollector::new();
    let err = collector
        .export_metrics(MetricsFormat::Custom("my-format".to_string()))
        .await
        .unwrap_err();
    assert!(
        matches!(err, MonitoringError::MetricsCollectionFailed { .. }),
        "expected MetricsCollectionFailed for Custom format, got: {:?}",
        err
    );
}

// ---------------------------------------------------------------------------
// Latency calculation via public get_current_metrics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn latency_single_element_avg_equals_value() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(200))
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    let lat = &metrics.request_latencies["svc"];

    assert!((lat.avg_latency_ms - 200.0).abs() < 1.0, "avg should be 200ms");
    assert!((lat.min_latency_ms - 200.0).abs() < 1.0, "min should be 200ms");
    assert!((lat.max_latency_ms - 200.0).abs() < 1.0, "max should be 200ms");
    assert_eq!(lat.request_count, 1);
}

#[tokio::test]
async fn latency_two_elements_min_max_are_extremes() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(100))
        .await;
    collector
        .record_request_latency("svc", Duration::from_millis(300))
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    let lat = &metrics.request_latencies["svc"];

    assert!((lat.min_latency_ms - 100.0).abs() < 1.0);
    assert!((lat.max_latency_ms - 300.0).abs() < 1.0);
    assert!((lat.avg_latency_ms - 200.0).abs() < 1.0);
    assert_eq!(lat.request_count, 2);
}

// ---------------------------------------------------------------------------
// ErrorEvent recording
// ---------------------------------------------------------------------------

#[tokio::test]
async fn record_error_increments_count_and_by_type() {
    let mut collector = DefaultMetricsCollector::new();

    let event = ErrorEvent {
        timestamp: chrono::Utc::now(),
        error_type: "TimeoutError".to_string(),
        message: "request timed out".to_string(),
        component: "ollama-client".to_string(),
        severity: ErrorSeverity::Error,
    };
    collector.record_error(event).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert_eq!(
        metrics.error_metrics.errors_by_type.get("TimeoutError").copied(),
        Some(1)
    );
    assert!(
        !metrics.error_metrics.recent_errors.is_empty(),
        "recent_errors should contain the recorded event"
    );
}

// ---------------------------------------------------------------------------
// DefaultAlertManager::get_alert_history limit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn alert_history_respects_limit() {
    let manager = DefaultAlertManager::new();

    for i in 0u32..5 {
        manager
            .send_alert(&format!("Alert {}", i), "desc", AlertSeverity::Info)
            .await
            .unwrap();
    }

    let history = manager.get_alert_history(3).await.unwrap();
    assert_eq!(history.len(), 3, "limit=3 should cap at 3 results");
}

#[tokio::test]
async fn alert_history_returns_all_when_limit_exceeds_count() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("Only one", "desc", AlertSeverity::Warning)
        .await
        .unwrap();

    let history = manager.get_alert_history(100).await.unwrap();
    assert_eq!(history.len(), 1);
}

// ---------------------------------------------------------------------------
// DefaultAlertManager::acknowledge_alert error path (missing ID)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn acknowledge_nonexistent_alert_returns_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("ghost-alert-id").await;
    assert!(
        result.is_err(),
        "acknowledging a missing alert ID must return an error"
    );
    assert!(matches!(
        result.unwrap_err(),
        MonitoringError::AlertSendFailed { .. }
    ));
}

// ---------------------------------------------------------------------------
// DefaultAlertManager::configure_thresholds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn configure_thresholds_does_not_error() {
    let mut manager = DefaultAlertManager::new();
    let custom = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.5,
        max_error_rate: 0.1,
        max_cpu_usage: 0.7,
        max_memory_usage: 0.8,
        health_check_timeout: Duration::from_secs(10),
    };
    manager.configure_thresholds(custom).await.unwrap();
}

// ---------------------------------------------------------------------------
// DefaultHealthMonitor::check_model_health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_monitor_model_health_check_returns_healthy() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("llama3:8b").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
    assert!(result.details.contains_key("model"));
    assert!(
        result.component.starts_with("model:"),
        "component should be prefixed with 'model:'"
    );
}

// ---------------------------------------------------------------------------
// DefaultHealthMonitor::comprehensive_health_check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn comprehensive_health_check_includes_system_component() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let results = monitor.comprehensive_health_check().await.unwrap();
    assert!(!results.is_empty(), "should return at least one result");
    assert!(
        results.iter().any(|r| r.component == "system"),
        "system component must appear in comprehensive check"
    );
}

// ---------------------------------------------------------------------------
// MonitoringSystem start / stop
// ---------------------------------------------------------------------------

#[tokio::test]
async fn monitoring_system_start_and_stop_does_not_panic() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    system.stop_monitoring().await;
}

// ---------------------------------------------------------------------------
// CacheMetrics hit-rate computation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cache_metrics_hit_rate_zero_when_no_activity() {
    let collector = DefaultMetricsCollector::new();
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hit_rate, 0.0);
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
}

#[tokio::test]
async fn cache_metrics_hit_rate_computed_correctly() {
    let mut collector = DefaultMetricsCollector::new();
    collector.record_cache_hit("a").await;
    collector.record_cache_hit("b").await;
    collector.record_cache_miss("c").await;

    let metrics = collector.get_current_metrics().await.unwrap();
    // 2 hits, 1 miss → rate = 2/3
    assert!(
        (metrics.cache_metrics.hit_rate - (2.0_f64 / 3.0)).abs() < 1e-9,
        "hit rate should be 2/3, got {}",
        metrics.cache_metrics.hit_rate
    );
}
