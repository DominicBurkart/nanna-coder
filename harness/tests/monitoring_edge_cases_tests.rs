use harness::monitoring::{
    AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
    DefaultMetricsCollector, ErrorSeverity, HealthMonitor, HealthStatus, MetricsCollector,
    MetricsFormat, MonitoringSystem,
};
use std::time::Duration;

// ──────────────────────────────────────────────
// ErrorSeverity ordering
// ──────────────────────────────────────────────

#[test]
fn test_error_severity_ordering() {
    assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
    assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
    assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
    assert!(ErrorSeverity::Info < ErrorSeverity::Critical);
}

#[test]
fn test_error_severity_eq() {
    assert_eq!(ErrorSeverity::Info, ErrorSeverity::Info);
    assert_eq!(ErrorSeverity::Critical, ErrorSeverity::Critical);
    assert_ne!(ErrorSeverity::Warning, ErrorSeverity::Error);
}

// ──────────────────────────────────────────────
// AlertSeverity ordering
// ──────────────────────────────────────────────

#[test]
fn test_alert_severity_ordering() {
    assert!(AlertSeverity::Info < AlertSeverity::Warning);
    assert!(AlertSeverity::Warning < AlertSeverity::Error);
    assert!(AlertSeverity::Error < AlertSeverity::Critical);
}

#[test]
fn test_alert_severity_eq() {
    assert_eq!(AlertSeverity::Warning, AlertSeverity::Warning);
    assert_ne!(AlertSeverity::Info, AlertSeverity::Critical);
}

// ──────────────────────────────────────────────
// HealthStatus methods
// ──────────────────────────────────────────────

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

// ──────────────────────────────────────────────
// DefaultMetricsCollector::reset_metrics
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_reset_metrics() {
    let mut collector = DefaultMetricsCollector::new();

    collector
        .record_request_latency("api", Duration::from_millis(100))
        .await;
    collector.record_cache_hit("k1").await;
    collector.record_cache_miss("k2").await;

    let before = collector.get_current_metrics().await.unwrap();
    assert_eq!(before.cache_metrics.hits, 1);
    assert_eq!(before.cache_metrics.misses, 1);
    assert!(!before.request_latencies.is_empty());

    collector.reset_metrics().await;

    let after = collector.get_current_metrics().await.unwrap();
    assert_eq!(after.cache_metrics.hits, 0);
    assert_eq!(after.cache_metrics.misses, 0);
    assert!(after.request_latencies.is_empty());
}

// ──────────────────────────────────────────────
// MetricsFormat::Custom error path
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_export_metrics_custom_format_returns_err() {
    let collector = DefaultMetricsCollector::new();
    let result = collector
        .export_metrics(MetricsFormat::Custom("my-format".to_string()))
        .await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("my-format"));
}

// ──────────────────────────────────────────────
// DefaultAlertManager – configure_thresholds
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_configure_thresholds() {
    let mut manager = DefaultAlertManager::new();

    let thresholds = AlertThresholds {
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.9,
        max_error_rate: 0.01,
        max_cpu_usage: 0.8,
        max_memory_usage: 0.85,
        health_check_timeout: Duration::from_secs(10),
    };

    manager.configure_thresholds(thresholds).await.unwrap();
    // No observable state change other than success
}

// ──────────────────────────────────────────────
// DefaultAlertManager – get_alert_history limit
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_get_alert_history_limit() {
    let manager = DefaultAlertManager::new();

    // Send 5 alerts
    for i in 0..5 {
        manager
            .send_alert(
                &format!("Alert {i}"),
                "desc",
                AlertSeverity::Info,
            )
            .await
            .unwrap();
    }

    let history = manager.get_alert_history(3).await.unwrap();
    assert_eq!(history.len(), 3);
}

#[tokio::test]
async fn test_get_alert_history_all() {
    let manager = DefaultAlertManager::new();

    for i in 0..4 {
        manager
            .send_alert(&format!("A{i}"), "d", AlertSeverity::Warning)
            .await
            .unwrap();
    }

    let history = manager.get_alert_history(100).await.unwrap();
    assert_eq!(history.len(), 4);
}

// ──────────────────────────────────────────────
// DefaultAlertManager – acknowledge_alert with nonexistent ID
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_acknowledge_nonexistent_alert_returns_err() {
    let manager = DefaultAlertManager::new();
    let result = manager.acknowledge_alert("alert_9999").await;
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("alert_9999") || msg.contains("not found"));
}

// ──────────────────────────────────────────────
// DefaultAlertManager – get_active_alerts filtering
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_get_active_alerts_filters_acknowledged() {
    let manager = DefaultAlertManager::new();

    let id1 = manager
        .send_alert("First", "desc", AlertSeverity::Error)
        .await
        .unwrap();
    let _id2 = manager
        .send_alert("Second", "desc", AlertSeverity::Warning)
        .await
        .unwrap();

    // Acknowledge the first one
    manager.acknowledge_alert(&id1).await.unwrap();

    let active = manager.get_active_alerts().await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].title, "Second");
}

#[tokio::test]
async fn test_get_active_alerts_all_acknowledged() {
    let manager = DefaultAlertManager::new();

    let id = manager
        .send_alert("Only", "desc", AlertSeverity::Critical)
        .await
        .unwrap();
    manager.acknowledge_alert(&id).await.unwrap();

    let active = manager.get_active_alerts().await.unwrap();
    assert!(active.is_empty());
}

#[tokio::test]
async fn test_active_alerts_all_severities() {
    let manager = DefaultAlertManager::new();

    manager
        .send_alert("info", "d", AlertSeverity::Info)
        .await
        .unwrap();
    manager
        .send_alert("warn", "d", AlertSeverity::Warning)
        .await
        .unwrap();
    manager
        .send_alert("err", "d", AlertSeverity::Error)
        .await
        .unwrap();
    manager
        .send_alert("crit", "d", AlertSeverity::Critical)
        .await
        .unwrap();

    let active = manager.get_active_alerts().await.unwrap();
    assert_eq!(active.len(), 4);
}

// ──────────────────────────────────────────────
// DefaultHealthMonitor
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_comprehensive_health_check() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let results = monitor.comprehensive_health_check().await.unwrap();
    // At a minimum, system health check should be included
    assert!(!results.is_empty());
    let has_system = results.iter().any(|r| r.component == "system");
    assert!(has_system);
}

#[tokio::test]
async fn test_check_model_health() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("qwen3:0.6b").await.unwrap();
    assert_eq!(result.status, HealthStatus::Healthy);
    assert!(result.component.contains("qwen3:0.6b"));
}

#[tokio::test]
async fn test_check_model_health_details_populated() {
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor.check_model_health("llama3:8b").await.unwrap();
    assert!(result.details.contains_key("model"));
}

#[tokio::test]
async fn test_set_check_interval() {
    let mut monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    // Just verify the method doesn't panic
    monitor.set_check_interval(Duration::from_secs(60));
}

// ──────────────────────────────────────────────
// MonitoringSystem::start_monitoring / stop_monitoring
// ──────────────────────────────────────────────

#[tokio::test]
async fn test_start_and_stop_monitoring() {
    let mut system = MonitoringSystem::new();

    // Start the background task
    system.start_monitoring().await.unwrap();

    // Immediately stop it – task should be aborted cleanly
    system.stop_monitoring().await;
}

#[tokio::test]
async fn test_stop_monitoring_when_not_started() {
    let mut system = MonitoringSystem::new();
    // Stopping without starting should be a no-op
    system.stop_monitoring().await;
}

#[tokio::test]
async fn test_start_monitoring_twice() {
    let mut system = MonitoringSystem::new();

    system.start_monitoring().await.unwrap();
    // Starting again replaces the existing task handle
    system.start_monitoring().await.unwrap();

    system.stop_monitoring().await;
}
