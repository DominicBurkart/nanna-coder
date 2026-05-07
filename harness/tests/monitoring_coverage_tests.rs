use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, ErrorEvent, ErrorSeverity, HealthMonitor, HealthStatus,
    MetricsCollector, MetricsFormat, MonitoringSystem,
};
use std::time::Duration;

#[test]
fn metrics_collector_is_healthy_after_init() {
    let collector = DefaultMetricsCollector::new();
    assert!(collector.is_healthy());
}

#[test]
fn metrics_collector_requires_attention_false_initially() {
    let collector = DefaultMetricsCollector::new();
    assert!(!collector.requires_attention());
}

#[test]
fn metrics_collector_reset_clears_state() {
    let mut collector = DefaultMetricsCollector::new();
    collector.record_request(Duration::from_millis(50), true);
    collector.reset_metrics();
    let snapshot = collector.get_metrics_snapshot();
    assert_eq!(snapshot.total_requests, 0);
}

#[test]
fn alert_manager_get_alert_history_respects_limit() {
    let mut manager = DefaultAlertManager::new();
    let thresholds = AlertThresholds::default();
    // trigger several alerts by recording errors
    for i in 0..5 {
        let _ = manager.trigger_alert(
            format!("alert-{}", i),
            AlertSeverity::Warning,
            format!("message {}", i),
        );
    }
    let history = manager.get_alert_history(Some(3));
    assert!(history.len() <= 3);
}

#[test]
fn alert_manager_acknowledge_nonexistent_returns_error() {
    let mut manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("nonexistent-id");
    assert!(result.is_err());
}

#[test]
fn alert_manager_configure_thresholds_ok() {
    let mut manager = DefaultAlertManager::new();
    let thresholds = AlertThresholds::default();
    let result = manager.configure_thresholds(thresholds);
    assert!(result.is_ok());
}

#[test]
fn metrics_format_custom_returns_error_on_unsupported() {
    let collector = DefaultMetricsCollector::new();
    let result = collector.export_metrics(MetricsFormat::Custom("unknown".to_string()));
    assert!(result.is_err());
}

#[test]
fn alert_severity_ordering() {
    assert!(AlertSeverity::Critical > AlertSeverity::Warning);
    assert!(AlertSeverity::Warning > AlertSeverity::Info);
}

#[test]
fn error_severity_ordering() {
    assert!(ErrorSeverity::Critical > ErrorSeverity::High);
    assert!(ErrorSeverity::High > ErrorSeverity::Medium);
    assert!(ErrorSeverity::Medium > ErrorSeverity::Low);
}

#[test]
fn metrics_collector_record_error_increments_count() {
    let mut collector = DefaultMetricsCollector::new();
    let event = ErrorEvent {
        error_type: "TestError".to_string(),
        message: "test".to_string(),
        severity: ErrorSeverity::Low,
        timestamp: std::time::SystemTime::now(),
    };
    collector.record_error(event);
    let snapshot = collector.get_metrics_snapshot();
    assert!(snapshot.total_errors > 0);
}

#[test]
fn health_monitor_check_model_health_returns_healthy() {
    let monitor = DefaultHealthMonitor::new();
    let status = monitor.check_model_health("test-model");
    assert_eq!(status, HealthStatus::Healthy);
}

#[test]
fn alert_thresholds_default_values_are_sane() {
    let t = AlertThresholds::default();
    assert!(t.error_rate_threshold > 0.0);
    assert!(t.latency_threshold_ms > 0);
}

#[test]
fn monitoring_system_start_stop_lifecycle() {
    let mut system = MonitoringSystem::new();
    let start_result = system.start_monitoring();
    assert!(start_result.is_ok());
    let stop_result = system.stop_monitoring();
    assert!(stop_result.is_ok());
}

#[test]
fn metrics_snapshot_latency_percentiles_ordered() {
    let mut collector = DefaultMetricsCollector::new();
    for ms in [10, 20, 30, 50, 100, 200, 500] {
        collector.record_request(Duration::from_millis(ms), true);
    }
    let snapshot = collector.get_metrics_snapshot();
    assert!(snapshot.latency_p50_ms <= snapshot.latency_p95_ms);
    assert!(snapshot.latency_p95_ms <= snapshot.latency_p99_ms);
}

#[test]
fn metrics_snapshot_error_rate_calculation() {
    let mut collector = DefaultMetricsCollector::new();
    // 2 success, 1 failure => error rate ~0.33
    collector.record_request(Duration::from_millis(10), true);
    collector.record_request(Duration::from_millis(10), true);
    collector.record_request(Duration::from_millis(10), false);
    let snapshot = collector.get_metrics_snapshot();
    assert!(snapshot.error_rate > 0.0);
    assert!(snapshot.error_rate < 1.0);
}
