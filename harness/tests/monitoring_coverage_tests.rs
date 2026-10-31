use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, ErrorEvent, ErrorSeverity, HealthMonitor, HealthStatus,
    MetricsCollector, MetricsFormat, ModelMetrics, ModelResourceUsage, MonitoringSystem,
    QualityMetrics,
};
use std::time::Duration;

#[tokio::test]
async fn test_health_status_is_healthy() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[tokio::test]
async fn test_health_status_requires_attention() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

#[tokio::test]
async fn test_error_severity_ordering() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
    assert!(ErrorSeverity::Critical > ErrorSeverity::Info);
}

#[tokio::test]
async fn test_alert_severity_ordering() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
}

#[tokio::test]
async fn test_record_error() {
    let mut collector = DefaultMetricsCollector::new();
    let error = ErrorEvent {
        timestamp: chrono::Utc::now(),
        error_type: "TestError".to_string(),
        message: "A test error occurred".to_string(),
        component: "test-component".to_string(),
        severity: ErrorSeverity::Warning,
    };
    collector.record_error(error).await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert!(metrics
        .error_metrics
        .errors_by_type
        .contains_key("TestError"));
}

#[tokio::test]
async fn test_record_model_inference() {
    let mut collector = DefaultMetricsCollector::new();
    let model_metrics = ModelMetrics {
        model_name: "test-model".to_string(),
        inference_count: 100,
        avg_inference_time_ms: 250.0,
        tokens_per_second: 50.0,
        success_rate: 0.99,
        quality_scores: QualityMetrics {
            avg_coherence: 0.9,
            avg_relevance: 0.85,
            consistency: 0.95,
            accuracy_rate: 0.88,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 512.0,
            avg_cpu_percent: 45.0,
            gpu_utilization_percent: Some(80.0),
        },
    };
    collector
        .record_model_inference("test-model", model_metrics)
        .await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.model_metrics.contains_key("test-model"));
}

#[tokio::test]
async fn test_reset_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("test", Duration::from_millis(100))
        .await;
    collector.record_cache_hit("k").await;
    collector.reset_metrics().await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.request_latencies.is_empty());
    assert_eq!(metrics.cache_metrics.hits, 0);
}

#[tokio::test]
async fn test_export_metrics_custom_format_errors() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("my-format".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_acknowledge_nonexistent_alert() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("nonexistent-id").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_configure_thresholds() {
    let mut manager = DefaultAlertManager::new();
    let thresholds = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.7,
        max_error_rate: 0.01,
        max_cpu_usage: 0.8,
        max_memory_usage: 0.85,
        health_check_timeout: Duration::from_secs(15),
    };
    manager.configure_thresholds(thresholds).await.unwrap();
}

#[tokio::test]
async fn test_get_alert_history() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("Alert 1", "desc1", AlertSeverity::Info)
        .await
        .unwrap();
    manager
        .send_alert("Alert 2", "desc2", AlertSeverity::Error)
        .await
        .unwrap();
    let history = manager.get_alert_history(10).await.unwrap();
    assert_eq!(history.len(), 2);
    let limited = manager.get_alert_history(1).await.unwrap();
    assert_eq!(limited.len(), 1);
}

#[tokio::test]
async fn test_default_alert_manager() {
    let manager = DefaultAlertManager::default();
    let alerts = manager.get_active_alerts().await.unwrap();
    assert!(alerts.is_empty());
}

#[tokio::test]
async fn test_default_metrics_collector() {
    let collector = DefaultMetricsCollector::default();
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hits, 0);
}

#[tokio::test]
async fn test_monitoring_system_start_stop() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    system.stop_monitoring().await;
    system.stop_monitoring().await;
}

#[tokio::test]
async fn test_health_monitor_set_check_interval() {
    let mut monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    monitor.set_check_interval(Duration::from_secs(60));
}

#[tokio::test]
async fn test_send_alert_all_severities() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("critical", "desc", AlertSeverity::Critical)
        .await
        .unwrap();
    manager
        .send_alert("error", "desc", AlertSeverity::Error)
        .await
        .unwrap();
    manager
        .send_alert("info", "desc", AlertSeverity::Info)
        .await
        .unwrap();
    let alerts = manager.get_active_alerts().await.unwrap();
    assert_eq!(alerts.len(), 3);
}
