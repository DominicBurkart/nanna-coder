//! Targeted unit tests covering branches in `monitoring.rs` that have no
//! existing test.  Every test here exercises a specific gap identified by
//! manual inspection; none of them require a live container or Ollama.

#[cfg(test)]
#[allow(clippy::module_inception)]
mod monitoring_gap_tests {
    use crate::monitoring::{
        AlertManager, AlertSeverity, AlertThresholds, DefaultAlertManager, DefaultHealthMonitor,
        DefaultMetricsCollector, ErrorEvent, ErrorSeverity, HealthStatus, MetricsCollector,
        MetricsFormat, MonitoringSystem,
    };
    use chrono::Utc;
    use std::time::Duration;

    // ── HealthStatus helpers ──────────────────────────────────────────────────

    #[test]
    fn health_status_is_healthy_only_for_healthy_variant() {
        assert!(HealthStatus::Healthy.is_healthy());
        assert!(!HealthStatus::Warning.is_healthy());
        assert!(!HealthStatus::Degraded.is_healthy());
        assert!(!HealthStatus::Unhealthy.is_healthy());
        assert!(!HealthStatus::Unknown.is_healthy());
    }

    #[test]
    fn health_status_requires_attention_for_non_healthy_variants() {
        assert!(!HealthStatus::Healthy.requires_attention());
        assert!(HealthStatus::Warning.requires_attention());
        assert!(HealthStatus::Degraded.requires_attention());
        assert!(HealthStatus::Unhealthy.requires_attention());
        // Unknown is NOT in the requires_attention match — verify that contract.
        assert!(!HealthStatus::Unknown.requires_attention());
    }

    // ── ErrorSeverity ordering ────────────────────────────────────────────────

    #[test]
    fn error_severity_ordering_is_ascending() {
        assert!(ErrorSeverity::Info < ErrorSeverity::Warning);
        assert!(ErrorSeverity::Warning < ErrorSeverity::Error);
        assert!(ErrorSeverity::Error < ErrorSeverity::Critical);
        assert!(ErrorSeverity::Info < ErrorSeverity::Critical);
    }

    // ── AlertSeverity ordering ────────────────────────────────────────────────

    #[test]
    fn alert_severity_ordering_is_ascending() {
        assert!(AlertSeverity::Info < AlertSeverity::Warning);
        assert!(AlertSeverity::Warning < AlertSeverity::Error);
        assert!(AlertSeverity::Error < AlertSeverity::Critical);
    }

    // ── AlertThresholds::default ──────────────────────────────────────────────

    #[test]
    fn alert_thresholds_default_values_are_sane() {
        let t = AlertThresholds::default();
        assert_eq!(t.max_latency_ms, 5_000);
        assert!((t.min_cache_hit_rate - 0.8).abs() < f64::EPSILON);
        assert!((t.max_error_rate - 0.05).abs() < f64::EPSILON);
        assert!((t.max_cpu_usage - 0.9).abs() < f64::EPSILON);
        assert!((t.max_memory_usage - 0.9).abs() < f64::EPSILON);
        assert_eq!(t.health_check_timeout, Duration::from_secs(30));
    }

    // ── DefaultMetricsCollector::reset_metrics ────────────────────────────────

    #[tokio::test]
    async fn reset_metrics_clears_all_counters() {
        let mut collector = DefaultMetricsCollector::new();
        collector
            .record_request_latency("svc", Duration::from_millis(50))
            .await;
        collector.record_cache_hit("k1").await;
        collector.record_cache_miss("k2").await;

        // Sanity: data is present before reset.
        let before = collector.get_current_metrics().await.unwrap();
        assert_eq!(before.cache_metrics.hits, 1);
        assert_eq!(before.cache_metrics.misses, 1);
        assert!(!before.request_latencies.is_empty());

        collector.reset_metrics().await;

        let after = collector.get_current_metrics().await.unwrap();
        assert_eq!(after.cache_metrics.hits, 0);
        assert_eq!(after.cache_metrics.misses, 0);
        assert!(after.request_latencies.is_empty());
        // hit_rate must be 0 when there are no observations.
        assert!((after.cache_metrics.hit_rate).abs() < f64::EPSILON);
    }

    // ── MetricsFormat::Custom error path ─────────────────────────────────────

    #[tokio::test]
    async fn export_metrics_custom_format_returns_error() {
        let collector = DefaultMetricsCollector::new();
        let result = collector
            .export_metrics(MetricsFormat::Custom("parquet".to_string()))
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("parquet"),
            "error should name the format: {msg}"
        );
    }

    // ── DefaultAlertManager: history and thresholds ───────────────────────────

    #[tokio::test]
    async fn alert_history_is_bounded_by_limit() {
        let manager = DefaultAlertManager::new();

        for i in 0..5u32 {
            manager
                .send_alert(&format!("Alert {i}"), "description", AlertSeverity::Info)
                .await
                .unwrap();
        }

        // limit=3 must return at most 3 entries (most-recent first).
        let history = manager.get_alert_history(3).await.unwrap();
        assert_eq!(history.len(), 3);
        // The returned slice is in reverse insertion order: first item is Alert 4.
        assert!(history[0].title.contains('4'));
    }

    #[tokio::test]
    async fn configure_thresholds_replaces_existing_thresholds() {
        let mut manager = DefaultAlertManager::new();
        let custom = AlertThresholds {
            max_latency_ms: 1_000,
            min_cache_hit_rate: 0.5,
            max_error_rate: 0.1,
            max_cpu_usage: 0.8,
            max_memory_usage: 0.8,
            health_check_timeout: Duration::from_secs(10),
        };
        // configure_thresholds must succeed and not panic.
        manager.configure_thresholds(custom).await.unwrap();
    }

    // ── DefaultAlertManager: acknowledge non-existent alert returns error ─────

    #[tokio::test]
    async fn acknowledge_missing_alert_returns_error() {
        let manager = DefaultAlertManager::new();
        let result = manager.acknowledge_alert("nonexistent").await;
        assert!(result.is_err());
    }

    // ── MonitoringSystem start/stop lifecycle ─────────────────────────────────

    #[tokio::test]
    async fn monitoring_system_start_and_stop_lifecycle() {
        let mut system = MonitoringSystem::new();
        // start_monitoring must succeed.
        system.start_monitoring().await.unwrap();
        // stop_monitoring must not panic even when called once.
        system.stop_monitoring().await;
        // A second stop must be idempotent (no task to abort).
        system.stop_monitoring().await;
    }

    // ── record_error flows through to error_metrics ───────────────────────────

    #[tokio::test]
    async fn record_error_increments_error_count_and_categorises() {
        let mut collector = DefaultMetricsCollector::new();
        let ev = ErrorEvent {
            timestamp: Utc::now(),
            error_type: "timeout".to_string(),
            message: "connection timed out".to_string(),
            component: "ollama".to_string(),
            severity: ErrorSeverity::Error,
        };
        collector.record_error(ev).await;

        let metrics = collector.get_current_metrics().await.unwrap();
        assert_eq!(metrics.error_metrics.total_errors, 1);
        assert_eq!(
            metrics.error_metrics.errors_by_type.get("timeout").copied(),
            Some(1)
        );
        assert_eq!(metrics.error_metrics.recent_errors.len(), 1);
    }

    // ── DefaultHealthMonitor: check_model_health always returns Healthy ────────

    #[tokio::test]
    async fn model_health_check_returns_healthy() {
        use crate::monitoring::HealthMonitor;
        let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
        let result = monitor.check_model_health("qwen3:0.6b").await.unwrap();
        assert_eq!(result.status, HealthStatus::Healthy);
        assert!(result.component.starts_with("model:"));
    }
}
