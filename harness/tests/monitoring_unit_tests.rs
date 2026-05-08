//! Unit tests for monitoring module public APIs

use chrono::Utc;
use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, ErrorEvent, ErrorSeverity, HealthStatus, MetricsCollector,
    MetricsFormat,
};
use std::time::Duration;

#[tokio::test]
async fn health_status_is_healthy() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[tokio::test]
async fn health_status_requires_attention() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

#[tokio::test]
async fn sequential_alert_ids() {
    let manager = DefaultAlertManager::new();
    let id1 = manager
        .send_alert("Alert 1", "First alert", AlertSeverity::Info)
        .await
        .unwrap();
    let id2 = manager
        .send_alert("Alert 2", "Second alert", AlertSeverity::Info)
        .await
        .unwrap();
    assert_eq!(id1, "alert_1");
    assert_eq!(id2, "alert_2");
}

#[tokio::test]
async fn reset_metrics_clears_state() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(50))
        .await;
    collector.record_cache_hit("k1").await;

    let metrics_before = collector.get_current_metrics().await.unwrap();
    assert!(!metrics_before.request_latencies.is_empty());
    assert_eq!(metrics_before.cache_metrics.hits, 1);

    collector.reset_metrics().await;

    let metrics_after = collector.get_current_metrics().await.unwrap();
    assert!(metrics_after.request_latencies.is_empty());
    assert_eq!(metrics_after.cache_metrics.hits, 0);
}

#[tokio::test]
async fn custom_metrics_format_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("my-format".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn latency_statistics_correct() {
    let mut collector = DefaultMetricsCollector::new();
    for ms in [100u64, 200, 300] {
        collector
            .record_request_latency("svc", Duration::from_millis(ms))
            .await;
    }
    let metrics = collector.get_current_metrics().await.unwrap();
    let latency = &metrics.request_latencies["svc"];
    assert_eq!(latency.request_count, 3);
    assert!((latency.avg_latency_ms - 200.0).abs() < 1.0);
    assert!((latency.min_latency_ms - 100.0).abs() < 1.0);
    assert!((latency.max_latency_ms - 300.0).abs() < 1.0);
}

#[tokio::test]
async fn acknowledge_nonexistent_alert_returns_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("alert_999").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn alert_history_respects_limit() {
    let manager = DefaultAlertManager::new();
    for i in 0..5 {
        manager
            .send_alert(&format!("Alert {}", i), "desc", AlertSeverity::Info)
            .await
            .unwrap();
    }
    let history = manager.get_alert_history(3).await.unwrap();
    assert_eq!(history.len(), 3);
}

#[tokio::test]
async fn cache_hit_rate_zero_when_no_activity() {
    let collector = DefaultMetricsCollector::new();
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hit_rate, 0.0);
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
}

#[tokio::test]
async fn alert_thresholds_default_values() {
    let thresholds = AlertThresholds::default();
    assert_eq!(thresholds.max_latency_ms, 5000);
    assert!((thresholds.min_cache_hit_rate - 0.8).abs() < f64::EPSILON);
    assert!((thresholds.max_error_rate - 0.05).abs() < f64::EPSILON);
    assert!((thresholds.max_cpu_usage - 0.9).abs() < f64::EPSILON);
}

#[tokio::test]
async fn model_health_check_returns_healthy() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("qwen3:0.6b").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
    assert!(result.component.contains("model"));
}

#[tokio::test]
async fn multiple_error_events_tracked() {
    let mut collector = DefaultMetricsCollector::new();
    let err1 = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "network".to_string(),
        message: "connection timeout".to_string(),
        component: "http-client".to_string(),
        severity: ErrorSeverity::Error,
    };
    let err2 = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "network".to_string(),
        message: "dns failure".to_string(),
        component: "http-client".to_string(),
        severity: ErrorSeverity::Warning,
    };
    collector.record_error(err1).await;
    collector.record_error(err2).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 2);
    assert_eq!(metrics.error_metrics.errors_by_type["network"], 2);
}
