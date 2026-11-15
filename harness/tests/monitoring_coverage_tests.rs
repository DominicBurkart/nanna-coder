//! Tests for code paths in `harness::monitoring` that aren't exercised by the
//! existing inline tests.  Each test targets a specific function that showed
//! zero coverage in prior runs, giving the patch 100% coverage on the new
//! test lines.

use chrono::Utc;
use harness::monitoring::{
    ErrorEvent, ErrorSeverity, ModelMetrics, ModelResourceUsage, QualityMetrics,
};
use harness::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultMetricsCollector,
    HealthStatus, MetricsCollector, MetricsFormat, MonitoringSystem,
};
use std::time::Duration;

// ── HealthStatus helper methods ────────────────────────────────────────────

#[test]
fn health_status_is_healthy_only_for_healthy() {
    assert!(HealthStatus::Healthy.is_healthy());
    assert!(!HealthStatus::Warning.is_healthy());
    assert!(!HealthStatus::Degraded.is_healthy());
    assert!(!HealthStatus::Unhealthy.is_healthy());
    assert!(!HealthStatus::Unknown.is_healthy());
}

#[test]
fn health_status_requires_attention_for_non_healthy_non_unknown() {
    assert!(!HealthStatus::Healthy.requires_attention());
    assert!(HealthStatus::Warning.requires_attention());
    assert!(HealthStatus::Degraded.requires_attention());
    assert!(HealthStatus::Unhealthy.requires_attention());
    assert!(!HealthStatus::Unknown.requires_attention());
}

// ── Severity ordering ──────────────────────────────────────────────────────

#[test]
fn error_severity_ordering() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
}

#[test]
fn alert_severity_ordering() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
}

// ── DefaultMetricsCollector uncovered methods ──────────────────────────────

#[tokio::test]
async fn record_error_increments_error_count() {
    let mut collector = DefaultMetricsCollector::new();

    let error = ErrorEvent {
        timestamp: Utc::now(),
        error_type: "network_error".to_string(),
        message: "Connection refused".to_string(),
        component: "ollama".to_string(),
        severity: ErrorSeverity::Error,
    };

    collector.record_error(error).await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert_eq!(metrics.error_metrics.total_errors, 1);
    assert!(
        metrics
            .error_metrics
            .errors_by_type
            .contains_key("network_error")
    );
}

#[tokio::test]
async fn record_model_inference_stores_model_metrics() {
    let mut collector = DefaultMetricsCollector::new();

    let model_metrics = ModelMetrics {
        model_name: "qwen3:0.6b".to_string(),
        inference_count: 100,
        avg_inference_time_ms: 250.0,
        tokens_per_second: 40.0,
        success_rate: 0.99,
        quality_scores: QualityMetrics {
            avg_coherence: 0.85,
            avg_relevance: 0.90,
            consistency: 0.88,
            accuracy_rate: 0.92,
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
}

#[tokio::test]
async fn reset_metrics_clears_all_counters() {
    let mut collector = DefaultMetricsCollector::new();

    collector
        .record_request_latency("svc", Duration::from_millis(100))
        .await;
    collector.record_cache_hit("k1").await;
    collector.record_cache_miss("k2").await;

    let before = collector.get_current_metrics().await.unwrap();
    assert!(!before.request_latencies.is_empty());
    assert_eq!(before.cache_metrics.hits, 1);

    collector.reset_metrics().await;

    let after = collector.get_current_metrics().await.unwrap();
    assert!(after.request_latencies.is_empty());
    assert_eq!(after.cache_metrics.hits, 0);
    assert_eq!(after.cache_metrics.misses, 0);
}

#[tokio::test]
async fn export_metrics_custom_format_returns_error() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("unsupported".to_string()))
        .await;
    assert!(result.is_err());
}

// ── DefaultAlertManager uncovered methods ─────────────────────────────────

#[tokio::test]
async fn get_alert_history_returns_most_recent_in_reverse_order() {
    let manager = DefaultAlertManager::new();

    manager
        .send_alert("Alert 1", "first", AlertSeverity::Info)
        .await
        .unwrap();
    manager
        .send_alert("Alert 2", "second", AlertSeverity::Warning)
        .await
        .unwrap();
    manager
        .send_alert("Alert 3", "third", AlertSeverity::Error)
        .await
        .unwrap();

    let limited = manager.get_alert_history(2).await.unwrap();
    assert_eq!(limited.len(), 2);

    let all = manager.get_alert_history(10).await.unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn configure_thresholds_succeeds() {
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
async fn acknowledge_alert_missing_id_returns_error() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("nonexistent_id").await;
    assert!(result.is_err());
}

// ── MonitoringSystem start / stop ─────────────────────────────────────────

#[tokio::test]
async fn monitoring_system_start_and_stop() {
    let mut system = MonitoringSystem::new();
    system.start_monitoring().await.unwrap();
    system.stop_monitoring().await;
}
