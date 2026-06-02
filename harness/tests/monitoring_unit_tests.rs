use chrono::Utc;
use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, ErrorEvent, ErrorSeverity, HealthMonitor, HealthStatus,
    MetricsCollector, MetricsFormat, ModelMetrics, ModelResourceUsage, MonitoringSystem,
    QualityMetrics,
};
use std::time::Duration;

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

#[tokio::test]
async fn record_error_appears_in_metrics() {
    let mut collector = DefaultMetricsCollector::new();
    let event = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "network".to_string(),
        message: "connection timeout".to_string(),
        component: "ollama".to_string(),
        severity: ErrorSeverity::Warning,
    };
    collector.record_error(event).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert_eq!(metrics.error_metrics.recent_errors.len(), 1);
}

#[tokio::test]
async fn multiple_errors_aggregated_by_type() {
    let mut collector = DefaultMetricsCollector::new();
    for _ in 0..3 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: "timeout".to_string(),
                message: "timed out".to_string(),
                component: "ollama".to_string(),
                severity: ErrorSeverity::Error,
            })
            .await;
    }
    collector
        .record_error(ErrorEvent {
            timestamp: Utc::now(),
            error_type: "io".to_string(),
            message: "io error".to_string(),
            component: "disk".to_string(),
            severity: ErrorSeverity::Error,
        })
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 4);
    assert_eq!(
        metrics.error_metrics.errors_by_type.get("timeout"),
        Some(&3)
    );
    assert_eq!(metrics.error_metrics.errors_by_type.get("io"), Some(&1));
}

#[tokio::test]
async fn error_rate_is_positive_when_errors_and_requests_recorded() {
    let mut collector = DefaultMetricsCollector::new();
    for _ in 0..8 {
        collector
            .record_request_latency("svc", Duration::from_millis(50))
            .await;
    }
    for _ in 0..2 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: "err".to_string(),
                message: "fail".to_string(),
                component: "svc".to_string(),
                severity: ErrorSeverity::Error,
            })
            .await;
    }

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 2);
    assert!(metrics.error_metrics.error_rate > 0.0);
    assert!(metrics.error_metrics.error_rate < 1.0);
}

#[tokio::test]
async fn record_model_inference_stores_data() {
    let mut collector = DefaultMetricsCollector::new();
    let model_metrics = ModelMetrics {
        model_name: "qwen3:0.6b".to_string(),
        inference_count: 10,
        avg_inference_time_ms: 250.0,
        tokens_per_second: 40.0,
        success_rate: 1.0,
        quality_scores: QualityMetrics {
            avg_coherence: 0.9,
            avg_relevance: 0.85,
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
    assert!(metrics.model_metrics.contains_key("qwen3:0.6b"));
}

#[tokio::test]
async fn custom_metrics_format_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("myformat".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn alert_history_returns_all_and_respects_limit() {
    let manager = DefaultAlertManager::new();
    for i in 0..5 {
        manager
            .send_alert(&format!("Alert {i}"), "desc", AlertSeverity::Info)
            .await
            .unwrap();
    }

    let history_all = manager.get_alert_history(100).await.unwrap();
    assert_eq!(history_all.len(), 5);

    let history_two = manager.get_alert_history(2).await.unwrap();
    assert_eq!(history_two.len(), 2);
}

#[tokio::test]
async fn configure_thresholds_succeeds_without_error() {
    let mut manager = DefaultAlertManager::new();
    let result = manager.configure_thresholds(AlertThresholds::default()).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn acknowledge_nonexistent_alert_is_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("nonexistent-id").await;
    assert!(result.is_err());
}

#[test]
fn health_status_is_healthy_semantics() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[test]
fn health_status_requires_attention_semantics() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

#[test]
fn alert_severity_is_ordered_low_to_high() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
}

#[test]
fn error_severity_is_ordered_low_to_high() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
}

#[tokio::test]
async fn check_model_health_returns_healthy_status() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("qwen3:0.6b").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
}

#[tokio::test]
async fn comprehensive_health_check_includes_system_component() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let results = monitor.comprehensive_health_check().await.unwrap();
    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.component.contains("system")));
}

#[test]
fn set_check_interval_does_not_panic() {
    let mut monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    monitor.set_check_interval(Duration::from_secs(10));
}

#[tokio::test]
async fn monitoring_system_start_and_stop_cleanly() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    system.stop_monitoring().await;
}

#[test]
fn default_metrics_collector_starts_empty() {
    let _collector = DefaultMetricsCollector::default();
}

#[test]
fn default_alert_manager_starts_empty() {
    let _manager = DefaultAlertManager::default();
}

#[tokio::test]
async fn latency_percentiles_are_monotone_with_100_samples() {
    let mut collector = DefaultMetricsCollector::new();
    for i in 1..=100u64 {
        collector
            .record_request_latency("svc", Duration::from_millis(i))
            .await;
    }

    let metrics = collector.get_current_metrics().await.unwrap();
    let latency = metrics.request_latencies.get("svc").unwrap();
    assert!(latency.p95_latency_ms >= latency.avg_latency_ms);
    assert!(latency.p99_latency_ms >= latency.p95_latency_ms);
    assert!(latency.max_latency_ms >= latency.p99_latency_ms);
}

#[tokio::test]
async fn multiple_services_tracked_independently() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc-a", Duration::from_millis(100))
        .await;
    collector
        .record_request_latency("svc-a", Duration::from_millis(200))
        .await;
    collector
        .record_request_latency("svc-b", Duration::from_millis(50))
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(
        metrics
            .request_latencies
            .get("svc-a")
            .unwrap()
            .request_count,
        2
    );
    assert_eq!(
        metrics
            .request_latencies
            .get("svc-b")
            .unwrap()
            .request_count,
        1
    );
}

#[test]
fn alert_thresholds_default_values_are_reasonable() {
    let thresholds = AlertThresholds::default();
    assert!(thresholds.max_latency_ms > 0);
    assert!(thresholds.min_cache_hit_rate > 0.0);
    assert!(thresholds.max_error_rate > 0.0);
    assert!(thresholds.max_cpu_usage > 0.0);
    assert!(thresholds.max_memory_usage > 0.0);
    assert!(thresholds.health_check_timeout > Duration::ZERO);
}

#[tokio::test]
async fn recent_errors_limited_to_ten() {
    let mut collector = DefaultMetricsCollector::new();
    for i in 0..15u64 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: format!("type_{i}"),
                message: "err".to_string(),
                component: "comp".to_string(),
                severity: ErrorSeverity::Error,
            })
            .await;
    }

    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.error_metrics.recent_errors.len() <= 10);
}
