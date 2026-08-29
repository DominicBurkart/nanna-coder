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
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

#[test]
fn test_error_severity_ordering() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
}

#[test]
fn test_alert_thresholds_default() {
    let t = AlertThresholds::default();
    assert_eq!(t.max_latency_ms, 5000);
    assert_eq!(t.min_cache_hit_rate, 0.8);
    assert_eq!(t.max_error_rate, 0.05);
    assert_eq!(t.max_cpu_usage, 0.9);
    assert_eq!(t.max_memory_usage, 0.9);
    assert_eq!(t.health_check_timeout, Duration::from_secs(30));
}

#[tokio::test]
async fn test_record_error() {
    let mut collector = DefaultMetricsCollector::new();

    let event = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "inference_timeout".to_string(),
        message: "Model inference timed out".to_string(),
        component: "ollama".to_string(),
        severity: ErrorSeverity::Error,
    };

    collector.record_error(event).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert_eq!(metrics.error_metrics.recent_errors.len(), 1);
    assert_eq!(
        metrics.error_metrics.recent_errors[0].error_type,
        "inference_timeout"
    );
}

#[tokio::test]
async fn test_error_rate_with_requests() {
    let mut collector = DefaultMetricsCollector::new();

    collector
        .record_request_latency("ollama", Duration::from_millis(100))
        .await;

    let event = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "timeout".to_string(),
        message: "Timed out".to_string(),
        component: "ollama".to_string(),
        severity: ErrorSeverity::Warning,
    };
    collector.record_error(event).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.error_rate, 1.0);
}

#[tokio::test]
async fn test_record_model_inference() {
    let mut collector = DefaultMetricsCollector::new();

    let model_metrics = ModelMetrics {
        model_name: "qwen3:0.6b".to_string(),
        inference_count: 42,
        avg_inference_time_ms: 250.0,
        tokens_per_second: 120.0,
        success_rate: 0.98,
        quality_scores: QualityMetrics {
            avg_coherence: 0.9,
            avg_relevance: 0.85,
            consistency: 0.92,
            accuracy_rate: 0.88,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 512.0,
            avg_cpu_percent: 45.0,
            gpu_utilization_percent: Some(70.0),
        },
    };

    collector
        .record_model_inference("qwen3:0.6b", model_metrics)
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.model_metrics.contains_key("qwen3:0.6b"));
    assert_eq!(metrics.model_metrics["qwen3:0.6b"].inference_count, 42);
}

#[tokio::test]
async fn test_reset_metrics() {
    let mut collector = DefaultMetricsCollector::new();

    collector
        .record_request_latency("service", Duration::from_millis(50))
        .await;
    collector.record_cache_hit("key").await;

    let before = collector.get_current_metrics().await.unwrap();
    assert_eq!(before.cache_metrics.hits, 1);

    collector.reset_metrics().await;

    let after = collector.get_current_metrics().await.unwrap();
    assert_eq!(after.cache_metrics.hits, 0);
    assert!(after.request_latencies.is_empty());
}

#[tokio::test]
async fn test_custom_metrics_format_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("parquet".to_string()))
        .await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("parquet"));
}

#[tokio::test]
async fn test_send_alert_all_severities() {
    let manager = DefaultAlertManager::new();

    let id_info = manager
        .send_alert("Info", "info desc", AlertSeverity::Info)
        .await
        .unwrap();
    let id_warn = manager
        .send_alert("Warn", "warn desc", AlertSeverity::Warning)
        .await
        .unwrap();
    let id_err = manager
        .send_alert("Err", "err desc", AlertSeverity::Error)
        .await
        .unwrap();
    let id_crit = manager
        .send_alert("Crit", "crit desc", AlertSeverity::Critical)
        .await
        .unwrap();

    let active = manager.get_active_alerts().await.unwrap();
    assert_eq!(active.len(), 4);

    let ids: std::collections::HashSet<_> =
        [&id_info, &id_warn, &id_err, &id_crit].iter().collect();
    assert_eq!(ids.len(), 4);
}

#[tokio::test]
async fn test_alert_history_limit() {
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
async fn test_configure_thresholds() {
    let mut manager = DefaultAlertManager::new();

    let custom = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.5,
        max_error_rate: 0.1,
        max_cpu_usage: 0.8,
        max_memory_usage: 0.85,
        health_check_timeout: Duration::from_secs(10),
    };

    manager.configure_thresholds(custom).await.unwrap();
}

#[tokio::test]
async fn test_acknowledge_nonexistent_alert_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("nonexistent-id-abc123").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[tokio::test]
async fn test_check_model_health_directly() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("qwen3:0.6b").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
    assert!(result.component.contains("qwen3:0.6b"));
}

#[tokio::test]
async fn test_monitoring_start_stop() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    system.stop_monitoring().await;
    // Second stop should be idempotent
    system.stop_monitoring().await;
}
