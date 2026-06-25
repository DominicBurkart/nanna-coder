use chrono::Utc;
use harness::monitoring::{ErrorEvent, ErrorSeverity, ModelMetrics, ModelResourceUsage, QualityMetrics};
use harness::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
    MonitoringSystem,
};
use std::time::Duration;

#[test]
fn metrics_collector_default_constructs() {
    let _collector = DefaultMetricsCollector::default();
}

#[tokio::test]
async fn record_error_appears_in_error_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    let event = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "network_timeout".to_string(),
        message: "Connection timed out".to_string(),
        component: "ollama-client".to_string(),
        severity: ErrorSeverity::Error,
    };
    collector.record_error(event).await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert_eq!(
        metrics.error_metrics.errors_by_type.get("network_timeout"),
        Some(&1)
    );
    assert!(!metrics.error_metrics.recent_errors.is_empty());
}

#[tokio::test]
async fn record_model_inference_appears_in_model_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    let model_metrics = ModelMetrics {
        model_name: "qwen3:0.6b".to_string(),
        inference_count: 10,
        avg_inference_time_ms: 250.0,
        tokens_per_second: 42.0,
        success_rate: 0.95,
        quality_scores: QualityMetrics {
            avg_coherence: 0.8,
            avg_relevance: 0.9,
            consistency: 0.85,
            accuracy_rate: 0.88,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 512.0,
            avg_cpu_percent: 45.0,
            gpu_utilization_percent: None,
        },
    };
    collector
        .record_model_inference("qwen3:0.6b", model_metrics)
        .await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.model_metrics.contains_key("qwen3:0.6b"));
    assert_eq!(metrics.model_metrics["qwen3:0.6b"].inference_count, 10);
}

#[tokio::test]
async fn export_metrics_custom_format_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("myformat".to_string()))
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("myformat"));
}

#[tokio::test]
async fn reset_metrics_clears_all_state() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(100))
        .await;
    collector.record_cache_hit("k").await;
    collector.reset_metrics().await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.request_latencies.is_empty());
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
    assert_eq!(metrics.error_metrics.total_errors, 0);
}

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

#[test]
fn health_monitor_set_check_interval() {
    let mut monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    monitor.set_check_interval(Duration::from_secs(60));
}

#[tokio::test]
async fn health_monitor_check_model_health_returns_healthy() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("qwen3:0.6b").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
    assert!(result.component.contains("qwen3:0.6b"));
}

#[tokio::test]
async fn health_monitor_comprehensive_health_check_returns_results() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let results = monitor.comprehensive_health_check().await.unwrap();
    assert!(!results.is_empty());
    let system_result = results.iter().find(|r| r.component == "system").unwrap();
    assert_eq!(system_result.status, HealthStatus::Healthy);
}

#[test]
fn alert_manager_default_constructs() {
    let _manager = DefaultAlertManager::default();
}

#[tokio::test]
async fn alert_manager_get_alert_history_ordered_most_recent_first() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("Alert 1", "First", AlertSeverity::Info)
        .await
        .unwrap();
    manager
        .send_alert("Alert 2", "Second", AlertSeverity::Warning)
        .await
        .unwrap();
    manager
        .send_alert("Alert 3", "Third", AlertSeverity::Error)
        .await
        .unwrap();
    let history = manager.get_alert_history(2).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].title, "Alert 3");
    assert_eq!(history[1].title, "Alert 2");
}

#[tokio::test]
async fn alert_manager_configure_thresholds() {
    let mut manager = DefaultAlertManager::new();
    let custom = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.7,
        max_error_rate: 0.02,
        max_cpu_usage: 0.8,
        max_memory_usage: 0.8,
        health_check_timeout: Duration::from_secs(10),
    };
    manager.configure_thresholds(custom).await.unwrap();
}

#[tokio::test]
async fn acknowledge_nonexistent_alert_returns_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("alert_9999").await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(err.contains("alert_9999"));
}

#[tokio::test]
async fn send_critical_alert_stores_with_correct_severity() {
    let manager = DefaultAlertManager::new();
    let id = manager
        .send_alert("Critical!", "System down", AlertSeverity::Critical)
        .await
        .unwrap();
    let alerts = manager.get_active_alerts().await.unwrap();
    assert_eq!(alerts.len(), 1);
    assert_eq!(alerts[0].severity, AlertSeverity::Critical);
    assert_eq!(alerts[0].id, id);
}

#[tokio::test]
async fn send_error_alert_stores_with_correct_severity() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("Error!", "Something failed", AlertSeverity::Error)
        .await
        .unwrap();
    let alerts = manager.get_active_alerts().await.unwrap();
    assert_eq!(alerts[0].severity, AlertSeverity::Error);
}

#[test]
fn alert_severity_ordering() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
}

#[test]
fn alert_thresholds_default_values() {
    let t = AlertThresholds::default();
    assert_eq!(t.max_latency_ms, 5000);
    assert!((t.min_cache_hit_rate - 0.8).abs() < f64::EPSILON);
    assert!((t.max_error_rate - 0.05).abs() < f64::EPSILON);
    assert!((t.max_cpu_usage - 0.9).abs() < f64::EPSILON);
    assert!((t.max_memory_usage - 0.9).abs() < f64::EPSILON);
    assert_eq!(t.health_check_timeout, Duration::from_secs(30));
}

#[test]
fn error_severity_ordering() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
}

#[test]
fn monitoring_system_default_constructs() {
    let _system = MonitoringSystem::default();
}

#[tokio::test]
async fn monitoring_system_start_and_stop() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    system.stop_monitoring().await;
}

#[tokio::test]
async fn monitoring_system_stop_without_prior_start_is_noop() {
    let mut system = MonitoringSystem::new();
    system.stop_monitoring().await;
}
