use chrono::Utc;
use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, ErrorEvent, ErrorSeverity, HealthMonitor, HealthStatus,
    MetricsCollector, MetricsFormat, MonitoringSystem,
};
use std::time::Duration;

#[tokio::test]
async fn health_status_healthy_is_healthy() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Healthy.requires_attention());
}

#[tokio::test]
async fn health_status_warning_requires_attention() {
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(HealthStatus::Warning.requires_attention());
}

#[tokio::test]
async fn health_status_degraded_requires_attention() {
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(!HealthStatus::Degraded.is_healthy());
}

#[tokio::test]
async fn health_status_unhealthy_requires_attention() {
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unhealthy.is_healthy());
}

#[tokio::test]
async fn metrics_collector_reset_clears_latencies() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(50))
        .await;
    collector.reset_metrics().await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.request_latencies.is_empty());
}

#[tokio::test]
async fn alert_manager_history_limit_respected() {
    let manager = DefaultAlertManager::new();
    for i in 0..5 {
        manager
            .send_alert(&format!("t{}", i), "desc", AlertSeverity::Info)
            .await
            .unwrap();
    }
    let history = manager.get_alert_history(3).await.unwrap();
    assert!(history.len() <= 3);
}

#[tokio::test]
async fn alert_manager_acknowledge_nonexistent_returns_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("no-such-id").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn alert_manager_configure_thresholds_ok() {
    let mut manager = DefaultAlertManager::new();
    let result = manager.configure_thresholds(AlertThresholds::default()).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn metrics_format_custom_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("unknown".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn alert_severity_ordering() {
    assert!(AlertSeverity::Critical > AlertSeverity::Error);
    assert!(AlertSeverity::Error > AlertSeverity::Warning);
    assert!(AlertSeverity::Warning > AlertSeverity::Info);
}

#[tokio::test]
async fn error_severity_ordering() {
    assert!(ErrorSeverity::Critical > ErrorSeverity::Error);
    assert!(ErrorSeverity::Error > ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning > ErrorSeverity::Info);
}

#[tokio::test]
async fn metrics_collector_record_error_increments_count() {
    let mut collector = DefaultMetricsCollector::new();
    let event = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "TestError".to_string(),
        message: "test message".to_string(),
        component: "test-component".to_string(),
        severity: ErrorSeverity::Warning,
    };
    collector.record_error(event).await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
}

#[tokio::test]
async fn health_monitor_check_model_health_returns_healthy() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("test-model").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
}

#[tokio::test]
async fn alert_thresholds_default_values_are_sane() {
    let t = AlertThresholds::default();
    assert!(t.max_latency_ms > 0);
    assert!(t.max_error_rate > 0.0);
    assert!(t.max_cpu_usage > 0.0);
}

#[tokio::test]
async fn monitoring_system_start_stop_lifecycle() {
    let mut system = MonitoringSystem::new();
    assert!(system.start_monitoring().await.is_ok());
    system.stop_monitoring().await;
}

#[tokio::test]
async fn metrics_latency_min_lte_avg_lte_max() {
    let mut collector = DefaultMetricsCollector::new();
    for ms in [10u64, 20, 30, 50, 100] {
        collector
            .record_request_latency("test", Duration::from_millis(ms))
            .await;
    }
    let metrics = collector.get_current_metrics().await.unwrap();
    let l = metrics.request_latencies.get("test").unwrap();
    assert!(l.min_latency_ms <= l.avg_latency_ms);
    assert!(l.avg_latency_ms <= l.max_latency_ms);
}

#[tokio::test]
async fn metrics_error_rate_positive_after_errors() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(10))
        .await;
    collector
        .record_request_latency("svc", Duration::from_millis(10))
        .await;
    let event = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "E".to_string(),
        message: "m".to_string(),
        component: "c".to_string(),
        severity: ErrorSeverity::Error,
    };
    collector.record_error(event).await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.error_metrics.total_errors > 0);
}
