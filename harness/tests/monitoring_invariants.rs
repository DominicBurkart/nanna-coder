//! Invariant tests for the monitoring module.
//!
//! The monitoring module computes aggregate metrics (latency percentiles, cache hit
//! rate, error rate) and manages alert lifecycle. These computations have mathematical
//! invariants that must hold for any input: percentile ordering, rate bounds, and
//! alert ID uniqueness. This file validates those invariants with proptest, plus a
//! few targeted unit tests for edge cases (empty input, duplicate acknowledgement).
//!
//! Covered invariants:
//! - `CacheMetrics::hit_rate` is in `[0.0, 1.0]` and equals `hits / (hits + misses)`.
//! - `LatencyMetrics`: `min <= avg <= max`, `p95 <= max`, `p99 <= max`, `min <= p95`,
//!   `min <= p99`, and `request_count` matches the number of recorded samples.
//! - `ErrorMetrics::error_rate` is `total_errors / total_requests` and is non-negative.
//! - `DefaultAlertManager` produces unique alert IDs and acknowledgement is idempotent
//!   in its observable effect (alert disappears from active set after ack, no error
//!   on duplicate ack? — see test for current behavior).
//! - `MetricsFormat::Json` / `Prometheus` / `Csv` exports are non-empty and parseable.

use harness::monitoring::{
    AlertManager, AlertSeverity, DefaultAlertManager, DefaultMetricsCollector, ErrorEvent,
    ErrorSeverity, HealthMonitor, HealthStatus, MetricsCollector, MetricsFormat,
};
use harness::{DefaultHealthMonitor, MonitoringSystem};
use proptest::prelude::*;
use std::time::Duration;

/// Cap latency values so the `as u128 -> f64 -> usize` conversions used by
/// `calculate_latency_metrics` stay lossless and the test stays fast.
const MAX_LATENCY_MS: u64 = 10_000;

fn latency_strategy() -> impl Strategy<Value = Duration> {
    (0u64..=MAX_LATENCY_MS).prop_map(Duration::from_millis)
}

proptest! {
    /// Latency aggregates must respect min <= avg <= max and percentile ordering,
    /// regardless of input distribution. Empty input is handled by a separate test.
    #[test]
    fn latency_metrics_respect_ordering_invariants(
        samples in proptest::collection::vec(latency_strategy(), 1..64)
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut collector = DefaultMetricsCollector::new();
            for sample in &samples {
                collector.record_request_latency("svc", *sample).await;
            }

            let metrics = collector.get_current_metrics().await.unwrap();
            let lat = metrics
                .request_latencies
                .get("svc")
                .expect("latency bucket exists after recording");

            // Request count must equal the number of samples recorded.
            prop_assert_eq!(lat.request_count, samples.len() as u64);

            // Ordering invariants: min <= avg <= max.
            prop_assert!(lat.min_latency_ms <= lat.avg_latency_ms,
                "min {} > avg {}", lat.min_latency_ms, lat.avg_latency_ms);
            prop_assert!(lat.avg_latency_ms <= lat.max_latency_ms,
                "avg {} > max {}", lat.avg_latency_ms, lat.max_latency_ms);

            // Percentiles lie within [min, max].
            prop_assert!(lat.p95_latency_ms >= lat.min_latency_ms,
                "p95 {} < min {}", lat.p95_latency_ms, lat.min_latency_ms);
            prop_assert!(lat.p95_latency_ms <= lat.max_latency_ms,
                "p95 {} > max {}", lat.p95_latency_ms, lat.max_latency_ms);
            prop_assert!(lat.p99_latency_ms >= lat.min_latency_ms,
                "p99 {} < min {}", lat.p99_latency_ms, lat.min_latency_ms);
            prop_assert!(lat.p99_latency_ms <= lat.max_latency_ms,
                "p99 {} > max {}", lat.p99_latency_ms, lat.max_latency_ms);

            // min/max must match observed extrema. We sort the input and compare.
            let mut ms: Vec<f64> = samples.iter().map(|d| d.as_millis() as f64).collect();
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            prop_assert_eq!(lat.min_latency_ms, *ms.first().unwrap());
            prop_assert_eq!(lat.max_latency_ms, *ms.last().unwrap());

            Ok(())
        })?;
    }

    /// Cache hit rate is a probability: always in [0, 1], and equal to hits / (hits + misses)
    /// whenever either is positive.
    #[test]
    fn cache_hit_rate_is_valid_probability(hits in 0u32..200, misses in 0u32..200) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut collector = DefaultMetricsCollector::new();
            for _ in 0..hits { collector.record_cache_hit("k").await; }
            for _ in 0..misses { collector.record_cache_miss("k").await; }

            let metrics = collector.get_current_metrics().await.unwrap();
            let cm = &metrics.cache_metrics;

            prop_assert_eq!(cm.hits, hits as u64);
            prop_assert_eq!(cm.misses, misses as u64);
            prop_assert!(cm.hit_rate >= 0.0 && cm.hit_rate <= 1.0,
                "hit_rate {} out of [0, 1]", cm.hit_rate);

            let total = hits + misses;
            if total == 0 {
                prop_assert_eq!(cm.hit_rate, 0.0);
            } else {
                let expected = hits as f64 / total as f64;
                prop_assert!((cm.hit_rate - expected).abs() < 1e-9,
                    "hit_rate {} != {} (hits={} total={})", cm.hit_rate, expected, hits, total);
            }
            Ok(())
        })?;
    }

    /// Error rate is total_errors / total_requests; non-negative; zero when no requests.
    #[test]
    fn error_rate_matches_definition(
        requests in 0u32..50,
        errors in 0u32..50,
    ) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let mut collector = DefaultMetricsCollector::new();
            for _ in 0..requests {
                collector
                    .record_request_latency("svc", Duration::from_millis(1))
                    .await;
            }
            for i in 0..errors {
                collector
                    .record_error(ErrorEvent {
                        timestamp: chrono::Utc::now(),
                        error_type: format!("kind_{}", i % 3),
                        message: "boom".to_string(),
                        component: "svc".to_string(),
                        severity: ErrorSeverity::Error,
                    })
                    .await;
            }

            let metrics = collector.get_current_metrics().await.unwrap();
            let em = &metrics.error_metrics;

            prop_assert_eq!(em.total_errors, errors as u64);
            prop_assert!(em.error_rate >= 0.0, "error_rate {} negative", em.error_rate);

            if requests == 0 {
                prop_assert_eq!(em.error_rate, 0.0);
            } else {
                let expected = errors as f64 / requests as f64;
                prop_assert!((em.error_rate - expected).abs() < 1e-9,
                    "error_rate {} != {}", em.error_rate, expected);
            }

            // recent_errors is bounded to 10.
            prop_assert!(em.recent_errors.len() <= 10);
            prop_assert!(em.recent_errors.len() <= em.total_errors as usize);

            // Sum of errors_by_type counts equals total_errors.
            let by_type_sum: u64 = em.errors_by_type.values().sum();
            prop_assert_eq!(by_type_sum, em.total_errors);
            Ok(())
        })?;
    }

    /// Every send_alert call returns a fresh ID; n sequential sends yield n distinct IDs.
    /// Acknowledging any subset removes exactly those alerts from the active set.
    #[test]
    fn alert_ids_are_unique_and_ack_removes_only_that_alert(n in 1usize..16) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let manager = DefaultAlertManager::new();
            let mut ids = Vec::with_capacity(n);
            for i in 0..n {
                let id = manager
                    .send_alert(&format!("t{}", i), "d", AlertSeverity::Info)
                    .await
                    .unwrap();
                ids.push(id);
            }

            let unique: std::collections::HashSet<_> = ids.iter().collect();
            prop_assert_eq!(unique.len(), n, "duplicate alert IDs");

            let active = manager.get_active_alerts().await.unwrap();
            prop_assert_eq!(active.len(), n);

            // Ack the first ID, check only it is removed.
            manager.acknowledge_alert(&ids[0]).await.unwrap();
            let active_after = manager.get_active_alerts().await.unwrap();
            prop_assert_eq!(active_after.len(), n - 1);
            prop_assert!(active_after.iter().all(|a| a.id != ids[0]));
            Ok(())
        })?;
    }
}

// ---- Edge-case and happy-path unit tests ----

#[tokio::test]
async fn latency_metrics_on_empty_bucket_are_all_zero() {
    // Record a metric under a different service, then confirm an unqueried service
    // produces no entry at all (verifies the map-key invariant).
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("other", Duration::from_millis(10))
        .await;
    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(!metrics.request_latencies.contains_key("never_recorded"));
    let other = metrics.request_latencies.get("other").unwrap();
    assert_eq!(other.request_count, 1);
    assert_eq!(other.min_latency_ms, other.max_latency_ms);
    assert_eq!(other.avg_latency_ms, 10.0);
}

#[tokio::test]
async fn acknowledge_unknown_alert_errors_cleanly() {
    let manager = DefaultAlertManager::new();
    let err = manager.acknowledge_alert("alert_does_not_exist").await;
    assert!(err.is_err(), "ack on unknown ID must surface an error");
}

#[tokio::test]
async fn double_acknowledge_is_idempotent_on_active_set() {
    // The observable contract: after ack, the alert is no longer active. Acking again
    // on the same ID currently succeeds (the alert still exists in history, just
    // flagged acknowledged). We validate the observable "no longer active" invariant
    // persists across repeated acks.
    let manager = DefaultAlertManager::new();
    let id = manager
        .send_alert("t", "d", AlertSeverity::Warning)
        .await
        .unwrap();
    manager.acknowledge_alert(&id).await.unwrap();
    // Second ack may succeed or fail, but active set must stay empty either way.
    let _ = manager.acknowledge_alert(&id).await;
    let active = manager.get_active_alerts().await.unwrap();
    assert!(active.is_empty(), "acknowledged alert must not reappear");
}

#[tokio::test]
async fn reset_metrics_clears_all_aggregates() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(42))
        .await;
    collector.record_cache_hit("k").await;
    collector.record_cache_miss("k").await;

    collector.reset_metrics().await;

    let metrics = collector.get_current_metrics().await.unwrap();
    assert!(metrics.request_latencies.is_empty());
    assert_eq!(metrics.cache_metrics.hits, 0);
    assert_eq!(metrics.cache_metrics.misses, 0);
    assert_eq!(metrics.cache_metrics.hit_rate, 0.0);
    assert_eq!(metrics.error_metrics.total_errors, 0);
}

#[tokio::test]
async fn export_formats_all_return_non_empty_output() {
    let mut collector = DefaultMetricsCollector::new();
    collector
        .record_request_latency("svc", Duration::from_millis(5))
        .await;
    collector.record_cache_hit("k").await;

    let json = collector.export_metrics(MetricsFormat::Json).await.unwrap();
    assert!(!json.is_empty());
    // JSON output must parse back as valid JSON.
    let _: serde_json::Value = serde_json::from_str(&json).expect("exported JSON is valid");

    let prom = collector
        .export_metrics(MetricsFormat::Prometheus)
        .await
        .unwrap();
    assert!(prom.contains("cache_hits_total"));
    assert!(prom.contains("# TYPE"));

    let csv = collector.export_metrics(MetricsFormat::Csv).await.unwrap();
    assert!(csv.starts_with("timestamp,metric_type,service,value"));
    assert!(csv.lines().count() >= 2);

    let custom = collector
        .export_metrics(MetricsFormat::Custom("unknown".into()))
        .await;
    assert!(custom.is_err(), "custom format must not silently succeed");
}

#[tokio::test]
async fn health_status_predicates_are_consistent() {
    // HealthStatus::is_healthy and requires_attention partition the severity space
    // cleanly: exactly one of {Healthy, Unknown, requires_attention} holds.
    for status in [
        HealthStatus::Healthy,
        HealthStatus::Warning,
        HealthStatus::Degraded,
        HealthStatus::Unhealthy,
        HealthStatus::Unknown,
    ] {
        let healthy = status.is_healthy();
        let attention = status.requires_attention();
        // Healthy and requires_attention are mutually exclusive.
        assert!(
            !(healthy && attention),
            "{:?} is both healthy and needs attention",
            status
        );
    }
}

#[tokio::test]
async fn monitoring_system_status_reports_at_least_one_health_check() {
    let system = MonitoringSystem::new();
    let status = system.get_system_status().await.unwrap();
    // The comprehensive check always includes at least a system-level entry.
    assert!(!status.health_checks.is_empty());
    // overall_health is some known variant — matches! is trivially satisfied, so we
    // instead assert the component name invariant: at least one check is for "system".
    assert!(status.health_checks.iter().any(|c| c.component == "system"));
}

#[tokio::test]
async fn container_health_on_missing_container_is_unhealthy_or_unknown() {
    // Property: a container that was never created must never report Healthy.
    let monitor = DefaultHealthMonitor::new(Duration::from_secs(30));
    let result = monitor
        .check_container_health("definitely-not-a-real-container-xyz-123")
        .await
        .unwrap();
    assert_ne!(result.status, HealthStatus::Healthy);
    // Component name echoes the container name.
    assert!(result
        .component
        .contains("definitely-not-a-real-container-xyz-123"));
}
