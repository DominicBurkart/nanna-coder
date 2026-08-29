use chrono::Utc;
use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
    MonitoringSystem,
};
use harness::monitoring::{
    ErrorEvent, ErrorSeverity, ModelMetrics, ModelResourceUsage, QualityMetrics,
};
use std::time::Duration;

#[test]
fn test_health_status_is_healthy() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[test]
fn test_health_status_requires_attention() {
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

#[test]
fn test_error_severity_ordering() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
}

#[test]
fn test_alert_severity_ordering() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
}

#[test]
fn test_alert_thresholds_default() {
    let thresholds = AlertThresholds::default();
    assert!(thresholds.max_latency_ms > 0);
    assert!(thresholds.min_cache_hit_rate > 0.0);
    assert!(thresholds.max_error_rate > 0.0);
    assert!(thresholds.max_cpu_usage > 0.0);
    assert!(thresholds.max_memory_usage > 0.0);
}

#[tokio::test]
async fn test_metrics_format_custom_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("myformat".to_string()))
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("myformat"));
}

#[tokio::test]
async fn test_reset_metrics_clears_all_counters() {
    let mut collector = DefaultMetricsCollector::new();
    collector.record_cache_hit("key1").await;
    collector.record_cache_miss("key2").await;
    collector.reset_metrics().await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
    assert_eq!(metrics.cache_metrics.hit_rate, 0.0);
}

#[tokio::test]
async fn test_record_error_stores_event() {
    let mut collector = DefaultMetricsCollector::new();
    let error = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "test_error".to_string(),
        message: "test message".to_string(),
        component: "test_component".to_string(),
        severity: ErrorSeverity::Error,
    };
    collector.record_error(error).await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert!(metrics
        .error_metrics
        .errors_by_type
        .contains_key("test_error"));
    assert_eq!(metrics.error_metrics.recent_errors.len(), 1);
}

#[tokio::test]
async fn test_record_multiple_errors_computes_rate() {
    let mut collector = DefaultMetricsCollector::new();
    // Record requests to have a denominator
    collector
        .record_request_latency("svc", Duration::from_millis(50))
        .await;
    collector
        .record_request_latency("svc", Duration::from_millis(60))
        .await;
    // Record errors
    for severity in [ErrorSeverity::Warning, ErrorSeverity::Critical] {
        let error = ErrorEvent {
            timestamp: Utc::now(),
            error_type: "type_a".to_string(),
            message: "msg".to_string(),
            component: "comp".to_string(),
            severity,
        };
        collector.record_error(error).await;
    }
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 2);
    assert!(metrics.error_metrics.error_rate > 0.0);
    assert!(metrics.error_metrics.errors_by_type["type_a"] == 2);
}

#[tokio::test]
async fn test_record_model_inference_stores_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    let model_metrics = ModelMetrics {
        model_name: "test-model".to_string(),
        inference_count: 10,
        avg_inference_time_ms: 150.0,
        tokens_per_second: 100.0,
        success_rate: 0.99,
        quality_scores: QualityMetrics {
            avg_coherence: 0.8,
            avg_relevance: 0.9,
            consistency: 0.85,
            accuracy_rate: 0.95,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 512.0,
            avg_cpu_percent: 25.0,
            gpu_utilization_percent: Some(75.0),
        },
    };
    collector
        .record_model_inference("test-model", model_metrics)
        .await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.model_metrics.contains_key("test-model"));
    assert_eq!(metrics.model_metrics["test-model"].inference_count, 10);
}

#[tokio::test]
async fn test_record_model_inference_no_gpu() {
    let mut collector = DefaultMetricsCollector::new();
    let model_metrics = ModelMetrics {
        model_name: "cpu-model".to_string(),
        inference_count: 5,
        avg_inference_time_ms: 200.0,
        tokens_per_second: 50.0,
        success_rate: 1.0,
        quality_scores: QualityMetrics {
            avg_coherence: 0.9,
            avg_relevance: 0.85,
            consistency: 0.9,
            accuracy_rate: 0.99,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 256.0,
            avg_cpu_percent: 80.0,
            gpu_utilization_percent: None,
        },
    };
    collector
        .record_model_inference("cpu-model", model_metrics)
        .await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.model_metrics["cpu-model"]
        .resource_usage
        .gpu_utilization_percent
        .is_none());
}

#[tokio::test]
async fn test_get_alert_history_returns_recent_first() {
    let manager = DefaultAlertManager::new();
    let id1 = manager
        .send_alert("Alert 1", "desc 1", AlertSeverity::Info)
        .await
        .unwrap();
    let id2 = manager
        .send_alert("Alert 2", "desc 2", AlertSeverity::Warning)
        .await
        .unwrap();
    let history = manager.get_alert_history(10).await.unwrap();
    assert_eq!(history.len(), 2);
    // Most recent is first (reversed order)
    assert_eq!(history[0].id, id2);
    assert_eq!(history[1].id, id1);
}

#[tokio::test]
async fn test_get_alert_history_respects_limit() {
    let manager = DefaultAlertManager::new();
    for i in 0..5u32 {
        manager
            .send_alert(&format!("Alert {}", i), "desc", AlertSeverity::Info)
            .await
            .unwrap();
    }
    let history = manager.get_alert_history(3).await.unwrap();
    assert_eq!(history.len(), 3);
}

#[tokio::test]
async fn test_acknowledge_unknown_alert_returns_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("nonexistent_id").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("nonexistent_id"));
}

#[tokio::test]
async fn test_send_alert_all_severity_levels() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("Critical", "desc", AlertSeverity::Critical)
        .await
        .unwrap();
    manager
        .send_alert("Error", "desc", AlertSeverity::Error)
        .await
        .unwrap();
    manager
        .send_alert("Info", "desc", AlertSeverity::Info)
        .await
        .unwrap();
    let active = manager.get_active_alerts().await.unwrap();
    assert_eq!(active.len(), 3);
}

#[tokio::test]
async fn test_configure_thresholds_succeeds() {
    let mut manager = DefaultAlertManager::new();
    let thresholds = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.9,
        max_error_rate: 0.01,
        max_cpu_usage: 0.8,
        max_memory_usage: 0.8,
        health_check_timeout: Duration::from_secs(10),
    };
    manager.configure_thresholds(thresholds).await.unwrap();
}

#[test]
fn test_set_check_interval_updates_without_panic() {
    let mut monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    monitor.set_check_interval(Duration::from_secs(60));
    monitor.set_check_interval(Duration::from_millis(500));
}

#[tokio::test]
async fn test_monitoring_system_start_and_stop() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    system.stop_monitoring().await;
}

#[tokio::test]
async fn test_monitoring_system_stop_without_start_is_noop() {
    let mut system = MonitoringSystem::new();
    // stop_monitoring when no background task is running should not panic
    system.stop_monitoring().await;
}
