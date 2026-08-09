//! Additional unit-level coverage for harness/src/monitoring.rs.
//!
//! These tests target code paths not exercised by the inline `#[cfg(test)]`
//! block already in monitoring.rs.  Specifically:
//!
//! * `record_error` / `record_model_inference` / `reset_metrics`
//! * `MetricsFormat::Custom` error path
//! * `HealthStatus::is_healthy` / `requires_attention` for every variant
//! * `AlertThresholds::default` field values
//! * `ErrorSeverity` and `AlertSeverity` ord/partial-ord impls
//! * `get_alert_history` with a limit
//! * `configure_thresholds`
//! * `MonitoringSystem::start_monitoring` / `stop_monitoring`
//! * zero-state (empty) cache-hit-rate calculation
//! * multiple-service latency tracking
//! * `DefaultHealthMonitor::check_model_health` returns `Healthy`
//! * `comprehensive_health_check` has system as the first result

use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, ErrorEvent, ErrorSeverity, HealthMonitor, HealthStatus,
    MetricsCollector, MetricsFormat, ModelMetrics, ModelResourceUsage, MonitoringSystem,
    QualityMetrics,
};
use std::time::Duration;

// ---------------------------------------------------------------------------
// DefaultMetricsCollector – newly covered paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_record_error_increments_error_count() {
    let mut collector = DefaultMetricsCollector::new();

    let event = ErrorEvent {
        timestamp: chrono::Utc::now(),
        error_type: "network".to_string(),
        message: "connection refused".to_string(),
        component: "ollama-provider".to_string(),
        severity: ErrorSeverity::Error,
    };
    collector.record_error(event).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert_eq!(
        metrics.error_metrics.errors_by_type.get("network").copied(),
        Some(1)
    );
    assert_eq!(metrics.error_metrics.recent_errors.len(), 1);
    assert_eq!(
        metrics.error_metrics.recent_errors[0].message,
        "connection refused"
    );
}

#[tokio::test]
async fn test_record_multiple_errors_same_type() {
    let mut collector = DefaultMetricsCollector::new();

    for i in 0..3_u32 {
        collector
            .record_error(ErrorEvent {
                timestamp: chrono::Utc::now(),
                error_type: "timeout".to_string(),
                message: format!("timeout #{}", i),
                component: "test".to_string(),
                severity: ErrorSeverity::Warning,
            })
            .await;
    }

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 3);
    assert_eq!(
        metrics.error_metrics.errors_by_type.get("timeout").copied(),
        Some(3)
    );
}

#[tokio::test]
async fn test_record_model_inference_stored_and_retrieved() {
    let mut collector = DefaultMetricsCollector::new();

    let model_metrics = ModelMetrics {
        model_name: "qwen3:0.6b".to_string(),
        inference_count: 42,
        avg_inference_time_ms: 123.4,
        tokens_per_second: 88.0,
        success_rate: 0.99,
        quality_scores: QualityMetrics {
            avg_coherence: 0.85,
            avg_relevance: 0.90,
            consistency: 0.88,
            accuracy_rate: 0.92,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 512.0,
            avg_cpu_percent: 25.0,
            gpu_utilization_percent: None,
        },
    };

    collector
        .record_model_inference("qwen3:0.6b", model_metrics)
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    let stored = metrics
        .model_metrics
        .get("qwen3:0.6b")
        .expect("model metrics should be stored under the model name");
    assert_eq!(stored.inference_count, 42);
    assert!(
        (stored.avg_inference_time_ms - 123.4).abs() < 1e-6,
        "avg_inference_time_ms should be preserved exactly"
    );
    assert!(stored.quality_scores.avg_coherence > 0.0);
}

#[tokio::test]
async fn test_reset_metrics_clears_all_data() {
    let mut collector = DefaultMetricsCollector::new();

    // Populate data across every counter
    collector
        .record_request_latency("svc", Duration::from_millis(50))
        .await;
    collector.record_cache_hit("k1").await;
    collector.record_cache_miss("k2").await;
    collector
        .record_error(ErrorEvent {
            timestamp: chrono::Utc::now(),
            error_type: "bad".to_string(),
            message: "oops".to_string(),
            component: "svc".to_string(),
            severity: ErrorSeverity::Critical,
        })
        .await;

    // Verify non-zero state before reset
    let before = collector.get_current_metrics().await.unwrap();
    assert!(!before.request_latencies.is_empty());
    assert_eq!(before.cache_metrics.hits, 1);
    assert_eq!(before.cache_metrics.misses, 1);
    assert_eq!(before.error_metrics.total_errors, 1);

    // Reset all metrics
    collector.reset_metrics().await;

    let after = collector.get_current_metrics().await.unwrap();
    assert!(after.request_latencies.is_empty(), "latencies should be cleared");
    assert_eq!(after.cache_metrics.hits, 0, "cache hits should reset to 0");
    assert_eq!(after.cache_metrics.misses, 0, "cache misses should reset to 0");
    assert!(
        (after.cache_metrics.hit_rate - 0.0).abs() < 1e-9,
        "hit_rate should reset to 0.0"
    );
    assert_eq!(
        after.error_metrics.total_errors, 0,
        "total errors should reset to 0"
    );
    assert!(after.error_metrics.recent_errors.is_empty());
}

#[tokio::test]
async fn test_empty_cache_metrics_hit_rate_is_zero() {
    // A freshly-created collector has no cache events; hit_rate must be 0.0
    // (not NaN from a 0/0 division).
    let collector = DefaultMetricsCollector::new();
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
    assert!(
        (metrics.cache_metrics.hit_rate - 0.0).abs() < 1e-9,
        "hit_rate should be 0.0 when there are no cache events, got {}",
        metrics.cache_metrics.hit_rate
    );
}

#[tokio::test]
async fn test_multiple_services_latency_tracked_independently() {
    let mut collector = DefaultMetricsCollector::new();

    // Two requests for "alpha" at 100ms and 200ms (avg = 150ms)
    collector
        .record_request_latency("alpha", Duration::from_millis(100))
        .await;
    collector
        .record_request_latency("alpha", Duration::from_millis(200))
        .await;
    // One request for "beta" at 300ms (avg = 300ms)
    collector
        .record_request_latency("beta", Duration::from_millis(300))
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();

    let alpha = metrics
        .request_latencies
        .get("alpha")
        .expect("alpha latency should be present");
    assert_eq!(alpha.request_count, 2, "alpha should have 2 requests");
    assert!(
        (alpha.avg_latency_ms - 150.0).abs() < 1e-6,
        "alpha avg latency should be 150ms, got {}",
        alpha.avg_latency_ms
    );

    let beta = metrics
        .request_latencies
        .get("beta")
        .expect("beta latency should be present");
    assert_eq!(beta.request_count, 1, "beta should have 1 request");
    assert!(
        (beta.avg_latency_ms - 300.0).abs() < 1e-6,
        "beta avg latency should be 300ms, got {}",
        beta.avg_latency_ms
    );
}

#[tokio::test]
async fn test_custom_format_export_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("xml".to_string()))
        .await;
    assert!(
        result.is_err(),
        "Custom format 'xml' should return an error, but got: {:?}",
        result
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("xml"),
        "error message should mention the unsupported format name 'xml', got: {}",
        err_msg
    );
}

// ---------------------------------------------------------------------------
// HealthStatus – is_healthy / requires_attention
// ---------------------------------------------------------------------------

#[test]
fn test_health_status_is_healthy() {
    assert!(HealthStatus::Healthy.is_healthy(), "Healthy should be is_healthy()");
    assert!(!HealthStatus::Warning.is_healthy(), "Warning should NOT be is_healthy()");
    assert!(!HealthStatus::Degraded.is_healthy(), "Degraded should NOT be is_healthy()");
    assert!(!HealthStatus::Unhealthy.is_healthy(), "Unhealthy should NOT be is_healthy()");
    assert!(!HealthStatus::Unknown.is_healthy(), "Unknown should NOT be is_healthy()");
}

#[test]
fn test_health_status_requires_attention() {
    assert!(
        !HealthStatus::Healthy.requires_attention(),
        "Healthy should NOT require attention"
    );
    assert!(
        HealthStatus::Warning.requires_attention(),
        "Warning SHOULD require attention"
    );
    assert!(
        HealthStatus::Degraded.requires_attention(),
        "Degraded SHOULD require attention"
    );
    assert!(
        HealthStatus::Unhealthy.requires_attention(),
        "Unhealthy SHOULD require attention"
    );
    assert!(
        !HealthStatus::Unknown.requires_attention(),
        "Unknown should NOT require attention"
    );
}

// ---------------------------------------------------------------------------
// ErrorSeverity / AlertSeverity – ordering
// ---------------------------------------------------------------------------

#[test]
fn test_error_severity_ordering() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
    // Transitivity
    assert!(ErrorSeverity::Info < ErrorSeverity::Critical);
    // Equality
    assert_eq!(ErrorSeverity::Error, ErrorSeverity::Error);
    // Ensure the derived Ord uses discriminant order (Info=0, Warning=1, …)
    let mut severities = vec![
        ErrorSeverity::Critical,
        ErrorSeverity::Info,
        ErrorSeverity::Error,
        ErrorSeverity::Warning,
    ];
    severities.sort();
    assert_eq!(
        severities,
        vec![
            ErrorSeverity::Info,
            ErrorSeverity::Warning,
            ErrorSeverity::Error,
            ErrorSeverity::Critical,
        ]
    );
}

#[test]
fn test_alert_severity_ordering() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
    assert!(AlertSeverity::Info < AlertSeverity::Critical);
    assert_eq!(AlertSeverity::Critical, AlertSeverity::Critical);
    let mut severities = vec![
        AlertSeverity::Critical,
        AlertSeverity::Info,
        AlertSeverity::Error,
        AlertSeverity::Warning,
    ];
    severities.sort();
    assert_eq!(
        severities,
        vec![
            AlertSeverity::Info,
            AlertSeverity::Warning,
            AlertSeverity::Error,
            AlertSeverity::Critical,
        ]
    );
}

// ---------------------------------------------------------------------------
// AlertThresholds – default values
// ---------------------------------------------------------------------------

#[test]
fn test_alert_thresholds_defaults() {
    let t = AlertThresholds::default();
    assert_eq!(t.max_latency_ms, 5000);
    assert!(
        (t.min_cache_hit_rate - 0.8).abs() < 1e-9,
        "default min_cache_hit_rate should be 0.8"
    );
    assert!(
        (t.max_error_rate - 0.05).abs() < 1e-9,
        "default max_error_rate should be 0.05"
    );
    assert!(
        (t.max_cpu_usage - 0.9).abs() < 1e-9,
        "default max_cpu_usage should be 0.9"
    );
    assert!(
        (t.max_memory_usage - 0.9).abs() < 1e-9,
        "default max_memory_usage should be 0.9"
    );
    assert_eq!(t.health_check_timeout, Duration::from_secs(30));
}

// ---------------------------------------------------------------------------
// DefaultAlertManager – history and configure_thresholds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_alert_history_and_active_alerts() {
    let manager = DefaultAlertManager::new();

    let id1 = manager
        .send_alert("Alert 1", "first alert", AlertSeverity::Info)
        .await
        .expect("send first alert");
    let _id2 = manager
        .send_alert("Alert 2", "second alert", AlertSeverity::Warning)
        .await
        .expect("send second alert");
    let _id3 = manager
        .send_alert("Alert 3", "third alert", AlertSeverity::Error)
        .await
        .expect("send third alert");

    // Acknowledge only the first alert
    manager
        .acknowledge_alert(&id1)
        .await
        .expect("acknowledge first alert");

    // get_alert_history respects the limit
    let history_2 = manager.get_alert_history(2).await.expect("get history 2");
    assert_eq!(history_2.len(), 2, "limit=2 should return exactly 2 alerts");

    let history_all = manager.get_alert_history(100).await.expect("get full history");
    assert_eq!(history_all.len(), 3, "full history should contain all 3 alerts");

    // Active alerts exclude the acknowledged one
    let active = manager.get_active_alerts().await.expect("get active alerts");
    assert_eq!(
        active.len(),
        2,
        "2 unacknowledged alerts should be active, got {}",
        active.len()
    );
    assert!(
        active.iter().all(|a| !a.acknowledged),
        "every active alert must be unacknowledged"
    );
}

#[tokio::test]
async fn test_configure_thresholds_succeeds() {
    let mut manager = DefaultAlertManager::new();

    let custom = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.5,
        max_error_rate: 0.1,
        max_cpu_usage: 0.75,
        max_memory_usage: 0.8,
        health_check_timeout: Duration::from_secs(10),
    };

    manager
        .configure_thresholds(custom)
        .await
        .expect("configure_thresholds should not error");
}

// ---------------------------------------------------------------------------
// DefaultHealthMonitor – model health and comprehensive check
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_model_health_check_returns_healthy() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor
        .check_model_health("qwen3:0.6b")
        .await
        .expect("check_model_health should not error");

    assert_eq!(
        result.status,
        HealthStatus::Healthy,
        "model health should be Healthy"
    );
    assert!(
        result.component.starts_with("model:"),
        "component should start with 'model:', got '{}'",
        result.component
    );
    assert!(
        result.details.contains_key("model"),
        "details should include 'model' key"
    );
}

#[tokio::test]
async fn test_comprehensive_health_check_first_entry_is_system() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let results = monitor
        .comprehensive_health_check()
        .await
        .expect("comprehensive_health_check should not error");

    assert!(
        !results.is_empty(),
        "comprehensive check should return at least one result"
    );
    assert_eq!(
        results[0].component, "system",
        "first entry must be the system health check, got '{}'",
        results[0].component
    );
}

// ---------------------------------------------------------------------------
// MonitoringSystem – start / stop background monitoring
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_monitoring_start_stop() {
    let mut system = MonitoringSystem::new();

    system
        .start_monitoring()
        .await
        .expect("start_monitoring should succeed");

    // Let the background task tick at least once
    tokio::time::sleep(Duration::from_millis(20)).await;

    // stop_monitoring should abort the background task cleanly
    system.stop_monitoring().await;

    // Calling stop again on an already-stopped system should be a no-op
    system.stop_monitoring().await;
}
