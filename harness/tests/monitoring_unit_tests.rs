//! Unit tests for monitoring module covering previously uncovered code paths.
//!
//! These tests focus on:
//! - `reset_metrics()` clearing all state
//! - Error recording and rate calculation
//! - Model inference metric recording
//! - Custom metrics format error path
//! - Alert history retrieval
//! - Alert threshold configuration
//! - `HealthStatus` helper methods
//! - `MonitoringSystem` lifecycle (start/stop)
//! - Percentile calculations with many data points

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
    collector.record_cache_hit("key1").await;
    collector.record_cache_miss("key2").await;

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

    let error = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "NetworkError".to_string(),
        message: "Connection refused".to_string(),
        component: "ollama".to_string(),
        severity: ErrorSeverity::Error,
    };
    collector.record_error(error).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert_eq!(
        metrics
            .error_metrics
            .errors_by_type
            .get("NetworkError")
            .copied()
            .unwrap_or(0),
        1
    );
    assert!(!metrics.error_metrics.recent_errors.is_empty());
}

#[tokio::test]
async fn error_rate_is_fraction_of_total_requests() {
    let mut collector = DefaultMetricsCollector::new();
    for _ in 0..4 {
        collector
            .record_request_latency("svc", Duration::from_millis(50))
            .await;
    }
    collector
        .record_error(ErrorEvent {
            timestamp: Utc::now(),
            error_type: "Timeout".to_string(),
            message: "timed out".to_string(),
            component: "svc".to_string(),
            severity: ErrorSeverity::Warning,
        })
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    // 1 error / 4 requests = 0.25
    assert!((metrics.error_metrics.error_rate - 0.25).abs() < 1e-9);
}

#[tokio::test]
async fn record_model_inference_stores_data() {
    let mut collector = DefaultMetricsCollector::new();
    let model_metrics = ModelMetrics {
        model_name: "qwen3:0.6b".to_string(),
        inference_count: 10,
        avg_inference_time_ms: 250.0,
        tokens_per_second: 40.0,
        success_rate: 0.95,
        quality_scores: QualityMetrics {
            avg_coherence: 0.85,
            avg_relevance: 0.90,
            consistency: 0.88,
            accuracy_rate: 0.92,
        },
        resource_usage: ModelResourceUsage {
            peak_memory_mb: 512.0,
            avg_cpu_percent: 60.0,
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
async fn custom_metrics_format_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("unsupported".to_string()))
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn alert_history_returns_all_and_respects_limit() {
    let manager = DefaultAlertManager::new();
    manager
        .send_alert("A1", "first", AlertSeverity::Info)
        .await
        .unwrap();
    manager
        .send_alert("A2", "second", AlertSeverity::Warning)
        .await
        .unwrap();
    manager
        .send_alert("A3", "third", AlertSeverity::Error)
        .await
        .unwrap();

    let history = manager.get_alert_history(10).await.unwrap();
    assert_eq!(history.len(), 3);

    let limited = manager.get_alert_history(2).await.unwrap();
    assert_eq!(limited.len(), 2);
}

#[tokio::test]
async fn configure_thresholds_succeeds_without_error() {
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

#[tokio::test]
async fn acknowledge_nonexistent_alert_is_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("no_such_id").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn health_status_is_healthy_semantics() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[tokio::test]
async fn health_status_requires_attention_semantics() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

#[tokio::test]
async fn alert_severity_is_ordered_low_to_high() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
}

#[tokio::test]
async fn error_severity_is_ordered_low_to_high() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
}

#[tokio::test]
async fn check_model_health_returns_healthy_status() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("qwen3:0.6b").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
    assert!(result.details.contains_key("model"));
}

#[tokio::test]
async fn comprehensive_health_check_includes_system_component() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let results = monitor.comprehensive_health_check().await.unwrap();
    assert!(!results.is_empty());
    let has_system = results.iter().any(|r| r.component == "system");
    assert!(has_system);
}

#[tokio::test]
async fn set_check_interval_does_not_panic() {
    let mut monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    monitor.set_check_interval(Duration::from_secs(120));
}

#[tokio::test]
async fn monitoring_system_start_and_stop_cleanly() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    system.stop_monitoring().await;
}

#[tokio::test]
async fn default_metrics_collector_starts_empty() {
    let collector = DefaultMetricsCollector::default();
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
    assert!(metrics.request_latencies.is_empty());
    assert_eq!(metrics.error_metrics.total_errors, 0);
}

#[tokio::test]
async fn default_alert_manager_starts_empty() {
    let manager = DefaultAlertManager::default();
    let alerts = manager.get_active_alerts().await.unwrap();
    assert!(alerts.is_empty());
    let history = manager.get_alert_history(10).await.unwrap();
    assert!(history.is_empty());
}

#[tokio::test]
async fn latency_percentiles_are_monotone_with_100_samples() {
    let mut collector = DefaultMetricsCollector::new();
    for i in 1_u64..=100 {
        collector
            .record_request_latency("svc", Duration::from_millis(i))
            .await;
    }
    let metrics = collector.get_current_metrics().await.unwrap();
    let lat = &metrics.request_latencies["svc"];
    assert_eq!(lat.request_count, 100);
    assert!(lat.min_latency_ms <= lat.avg_latency_ms);
    assert!(lat.avg_latency_ms <= lat.p95_latency_ms);
    assert!(lat.p95_latency_ms <= lat.p99_latency_ms);
    assert!(lat.p99_latency_ms <= lat.max_latency_ms);
}

#[tokio::test]
async fn multiple_services_tracked_independently() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc_a", Duration::from_millis(100))
        .await;
    collector
        .record_request_latency("svc_b", Duration::from_millis(200))
        .await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.request_latencies.len(), 2);
    assert!(metrics.request_latencies.contains_key("svc_a"));
    assert!(metrics.request_latencies.contains_key("svc_b"));
}

#[tokio::test]
async fn cache_hit_rate_is_zero_with_no_activity() {
    let collector = DefaultMetricsCollector::new();
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.cache_metrics.hit_rate, 0.0);
}

#[tokio::test]
async fn alert_thresholds_default_values_are_reasonable() {
    let thresholds = AlertThresholds::default();
    assert_eq!(thresholds.max_latency_ms, 5000);
    assert!((thresholds.min_cache_hit_rate - 0.8).abs() < 1e-10);
    assert!((thresholds.max_error_rate - 0.05).abs() < 1e-10);
    assert!(thresholds.health_check_timeout > Duration::ZERO);
}

#[tokio::test]
async fn multiple_errors_aggregated_by_type() {
    let mut collector = DefaultMetricsCollector::new();
    for _ in 0..3 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: "NetworkError".to_string(),
                message: "failed".to_string(),
                component: "svc".to_string(),
                severity: ErrorSeverity::Error,
            })
            .await;
    }
    for _ in 0..2 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: "ParseError".to_string(),
                message: "parse failed".to_string(),
                component: "svc".to_string(),
                severity: ErrorSeverity::Warning,
            })
            .await;
    }

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 5);
    assert_eq!(metrics.error_metrics.errors_by_type["NetworkError"], 3);
    assert_eq!(metrics.error_metrics.errors_by_type["ParseError"], 2);
}

#[tokio::test]
async fn recent_errors_limited_to_ten() {
    let mut collector = DefaultMetricsCollector::new();
    for i in 0..15 {
        collector
            .record_error(ErrorEvent {
                timestamp: Utc::now(),
                error_type: format!("Error{}", i),
                message: format!("msg {}", i),
                component: "svc".to_string(),
                severity: ErrorSeverity::Error,
            })
            .await;
    }
    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 15);
    assert_eq!(metrics.error_metrics.recent_errors.len(), 10);
}
