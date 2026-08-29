//! Tests covering gaps in harness/src/monitoring.rs.
//!
//! Coverage added:
//! - `HealthStatus::is_healthy` and `requires_attention` for all five variants
//! - `AlertSeverity` and `ErrorSeverity` ordering (PartialOrd / Ord)
//! - `DefaultMetricsCollector::reset_metrics` clears all counters
//! - `MetricsFormat::Custom` returns `MetricsCollectionFailed`
//! - `DefaultAlertManager::get_alert_history` (limit + acknowledged inclusion)
//! - `DefaultAlertManager::configure_thresholds` succeeds without error
//! - `MonitoringSystem::start_monitoring` / `stop_monitoring` lifecycle
//! - `DefaultMetricsCollector::record_error` grouping and `recent_errors` cap at 10

use chrono::Utc;
use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultMetricsCollector,
    ErrorEvent, ErrorSeverity, HealthStatus, MetricsCollector, MetricsFormat, MonitoringSystem,
};
use std::time::Duration;

// --- HealthStatus::is_healthy -----------------------------------------------

#[test]
fn health_status_healthy_is_healthy() {
    assert!(HealthStatus::Healthy.is_healthy());
}

#[test]
fn health_status_non_healthy_variants_are_not_healthy() {
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

// --- HealthStatus::requires_attention ----------------------------------------

#[test]
fn health_status_degraded_states_require_attention() {
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
}

#[test]
fn health_status_healthy_and_unknown_do_not_require_attention() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

// --- AlertSeverity ordering --------------------------------------------------

#[test]
fn alert_severity_ordering_is_increasing() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
    assert!(AlertSeverity::Info < AlertSeverity::Critical);
}

#[test]
fn alert_severity_sort_places_critical_last() {
    let mut severities = vec![
        AlertSeverity::Critical,
        AlertSeverity::Info,
        AlertSeverity::Warning,
        AlertSeverity::Error,
    ];
    severities.sort();
    assert_eq!(severities.last().unwrap(), &AlertSeverity::Critical);
    assert_eq!(severities.first().unwrap(), &AlertSeverity::Info);
}

// --- ErrorSeverity ordering --------------------------------------------------

#[test]
fn error_severity_ordering_is_increasing() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
}

// --- DefaultMetricsCollector::reset_metrics ----------------------------------

#[tokio::test]
async fn reset_metrics_clears_all_recorded_data() {
    let mut collector = DefaultMetricsCollector::new();

    collector
        .record_request_latency("svc", Duration::from_millis(100))
        .await;
    collector.record_cache_hit("k").await;
    collector.record_cache_miss("k2").await;
    collector
        .record_error(ErrorEvent {
            timestamp: Utc::now(),
            error_type: "TestError".to_string(),
            message: "test".to_string(),
            component: "test".to_string(),
            severity: ErrorSeverity::Error,
        })
        .await;

    let before = collector.get_current_metrics().await.unwrap();
    assert_eq!(before.cache_metrics.hits, 1);
    assert!(!before.request_latencies.is_empty());
    assert_eq!(before.error_metrics.total_errors, 1);

    collector.reset_metrics().await;

    let after = collector.get_current_metrics().await.unwrap();
    assert_eq!(after.cache_metrics.hits, 0);
    assert_eq!(after.cache_metrics.misses, 0);
    assert!(
        after.request_latencies.is_empty(),
        "request_latencies should be empty after reset"
    );
    assert_eq!(after.error_metrics.total_errors, 0);
}

// --- MetricsFormat::Custom error path ----------------------------------------

#[tokio::test]
async fn export_metrics_custom_format_returns_error_with_format_name() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("ndjson".to_string()))
        .await;

    assert!(
        result.is_err(),
        "MetricsFormat::Custom should return an error"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("ndjson"),
        "error message should include the format name, got: {err_msg}"
    );
}

// --- DefaultAlertManager::get_alert_history ----------------------------------

#[tokio::test]
async fn alert_history_includes_acknowledged_alerts() {
    let manager = DefaultAlertManager::new();
    let id = manager
        .send_alert("Test", "description", AlertSeverity::Warning)
        .await
        .unwrap();
    manager.acknowledge_alert(&id).await.unwrap();

    let active = manager.get_active_alerts().await.unwrap();
    assert_eq!(active.len(), 0, "acknowledged alert should not be active");

    let history = manager.get_alert_history(10).await.unwrap();
    assert_eq!(history.len(), 1, "history should include acknowledged alert");
    assert!(history[0].acknowledged);
}

#[tokio::test]
async fn alert_history_respects_limit_returning_most_recent() {
    let manager = DefaultAlertManager::new();

    for i in 0..7 {
        manager
            .send_alert(&format!("Alert {i}"), "desc", AlertSeverity::Info)
            .await
            .unwrap();
    }

    let history_3 = manager.get_alert_history(3).await.unwrap();
    assert_eq!(history_3.len(), 3, "should return exactly 3 alerts");

    let history_all = manager.get_alert_history(100).await.unwrap();
    assert_eq!(history_all.len(), 7, "should return all 7 alerts");
}

// --- DefaultAlertManager::configure_thresholds --------------------------------

#[tokio::test]
async fn configure_thresholds_succeeds() {
    let mut manager = DefaultAlertManager::new();
    let thresholds = AlertThresholds {
        max_latency_ms: 2000,
        min_cache_hit_rate: 0.6,
        max_error_rate: 0.02,
        max_cpu_usage: 0.75,
        max_memory_usage: 0.80,
        health_check_timeout: Duration::from_secs(15),
    };
    let result = manager.configure_thresholds(thresholds).await;
    assert!(
        result.is_ok(),
        "configure_thresholds should succeed, got: {:?}",
        result.err()
    );
}

// --- MonitoringSystem start/stop lifecycle -----------------------------------

#[tokio::test]
async fn monitoring_system_start_and_stop_lifecycle() {
    let mut system = MonitoringSystem::new();

    system.start_monitoring().await.unwrap();

    tokio::time::sleep(Duration::from_millis(10)).await;

    // stop_monitoring aborts the background task
    system.stop_monitoring().await;

    // calling stop again when no task is running must be a no-op (no panic)
    system.stop_monitoring().await;
}

// --- DefaultMetricsCollector::record_error grouping --------------------------

#[tokio::test]
async fn record_error_groups_by_error_type() {
    let mut collector = DefaultMetricsCollector::new();

    for _ in 0..3 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: "NetworkError".to_string(),
                message: "connection refused".to_string(),
                component: "http".to_string(),
                severity: ErrorSeverity::Error,
            })
            .await;
    }
    for _ in 0..2 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: "TimeoutError".to_string(),
                message: "deadline exceeded".to_string(),
                component: "rpc".to_string(),
                severity: ErrorSeverity::Warning,
            })
            .await;
    }

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 5);
    assert_eq!(
        metrics.error_metrics.errors_by_type["NetworkError"],
        3,
        "NetworkError count should be 3"
    );
    assert_eq!(
        metrics.error_metrics.errors_by_type["TimeoutError"],
        2,
        "TimeoutError count should be 2"
    );
}

#[tokio::test]
async fn record_error_recent_errors_capped_at_ten() {
    let mut collector = DefaultMetricsCollector::new();

    for i in 0..15u32 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: "BulkError".to_string(),
                message: format!("error #{i}"),
                component: "bulk".to_string(),
                severity: ErrorSeverity::Info,
            })
            .await;
    }

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(
        metrics.error_metrics.total_errors,
        15,
        "total_errors should count all 15"
    );
    assert!(
        metrics.error_metrics.recent_errors.len() <= 10,
        "recent_errors must be capped at 10, got {}",
        metrics.error_metrics.recent_errors.len()
    );
}
