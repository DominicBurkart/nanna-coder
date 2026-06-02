//! Unit tests for observability module covering previously uncovered code paths.
//!
//! These tests cover:
//! - Builder pattern methods
//! - `get_uptime()` tracking
//! - `start_monitoring()` / `stop_monitoring()` lifecycle
//! - SLA status computation (AtRisk path with 99.5% availability)
//! - TrendDirection::Degrading via empty cache scenario
//! - performance_score calculation
//! - Enum variant coverage for TrendDirection, SlaStatus

use harness::monitoring::AlertSeverity;
use harness::observability::{
    AlertPolicy, HealthThreshold, ObservabilitySystem, SlaStatus, TrendDirection,
};
use std::time::Duration;

#[tokio::test]
async fn builder_with_health_thresholds_does_not_panic() {
    let thresholds = HealthThreshold {
        cpu_threshold: 75.0,
        memory_threshold: 80.0,
        disk_threshold: 85.0,
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.85,
        max_error_rate: 0.02,
        container_timeout: Duration::from_secs(15),
    };
    let _system = ObservabilitySystem::new().with_health_thresholds(thresholds);
}

#[tokio::test]
async fn builder_with_alert_policy_does_not_panic() {
    let policy = AlertPolicy::immediate_critical();
    let _system = ObservabilitySystem::new().with_alert_policy(policy);
}

#[tokio::test]
async fn builder_with_health_check_interval_does_not_panic() {
    let _system =
        ObservabilitySystem::new().with_health_check_interval(Duration::from_secs(15));
}

#[tokio::test]
async fn get_uptime_increases_over_time() {
    let system = ObservabilitySystem::new();
    let t1 = system.get_uptime();
    tokio::time::sleep(Duration::from_millis(15)).await;
    let t2 = system.get_uptime();
    assert!(t2 > t1);
}

#[tokio::test]
async fn start_and_stop_monitoring_does_not_hang() {
    let mut system = ObservabilitySystem::new();
    system.start_monitoring().await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    system.stop_monitoring().await;
}

#[tokio::test]
async fn comprehensive_status_sla_is_at_risk_with_default_system() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await; // May fail in test env if subscriber already set
    let status = system.get_comprehensive_status().await.unwrap();
    // Default system hardcodes 99.5% availability < 99.9% target → AtRisk
    assert_eq!(
        status.availability_metrics.sla_compliance.status,
        SlaStatus::AtRisk
    );
}

#[tokio::test]
async fn comprehensive_status_cache_trend_degrading_when_no_activity() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    // Empty system: hit_rate = 0.0 < min_cache_hit_rate = 0.8 → Degrading
    assert_eq!(
        status.performance_trends.cache_performance_trend,
        TrendDirection::Degrading
    );
}

#[tokio::test]
async fn comprehensive_status_performance_score_reflects_degrading_cache() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    // Only cache is degrading (-15 pts from 100): score = 85.0
    assert!((status.performance_trends.performance_score - 85.0).abs() < 1e-9);
}

#[tokio::test]
async fn comprehensive_status_latency_trend_stable_with_no_requests() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    assert_eq!(
        status.performance_trends.latency_trend,
        TrendDirection::Stable
    );
}

#[tokio::test]
async fn sla_status_variants_are_distinct() {
    assert_ne!(SlaStatus::Breached, SlaStatus::Compliant);
    assert_ne!(SlaStatus::Breached, SlaStatus::AtRisk);
    assert_ne!(SlaStatus::AtRisk, SlaStatus::Compliant);
}

#[tokio::test]
async fn trend_direction_all_variants_are_distinct() {
    assert_ne!(TrendDirection::Improving, TrendDirection::Stable);
    assert_ne!(TrendDirection::Stable, TrendDirection::Degrading);
    assert_ne!(TrendDirection::Degrading, TrendDirection::Unknown);
    assert_ne!(TrendDirection::Improving, TrendDirection::Unknown);
}

#[tokio::test]
async fn alert_policy_immediate_critical_has_escalation_rules_for_critical() {
    let policy = AlertPolicy::immediate_critical();
    assert_eq!(policy.escalation_rules.len(), 2);
    let has_critical = policy
        .escalation_rules
        .iter()
        .any(|r| r.severity == AlertSeverity::Critical);
    assert!(has_critical);
}

#[tokio::test]
async fn alert_policy_balanced_has_grouping_rules() {
    let policy = AlertPolicy::balanced();
    assert!(!policy.grouping_rules.is_empty());
    assert_eq!(policy.grouping_rules[0].name, "container-alerts");
}

#[tokio::test]
async fn health_threshold_default_values_are_sensible() {
    let t = HealthThreshold::default();
    assert!(t.cpu_threshold > 0.0 && t.cpu_threshold < 100.0);
    assert!(t.memory_threshold > 0.0 && t.memory_threshold < 100.0);
    assert!(t.disk_threshold > 0.0 && t.disk_threshold < 100.0);
    assert!(t.max_latency_ms > 0);
    assert!(t.min_cache_hit_rate > 0.0 && t.min_cache_hit_rate <= 1.0);
    assert!(t.max_error_rate > 0.0 && t.max_error_rate < 1.0);
}

#[tokio::test]
async fn comprehensive_status_model_summary_empty_initially() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    assert_eq!(status.model_summary.total_models, 0);
}

#[tokio::test]
async fn comprehensive_status_availability_uptime_small_after_creation() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    assert!(status.availability_metrics.uptime < Duration::from_secs(10));
}

#[tokio::test]
async fn comprehensive_status_mtbf_and_mttr_are_set() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    assert!(status.availability_metrics.mtbf.is_some());
    assert!(status.availability_metrics.mttr.is_some());
}

#[tokio::test]
async fn observability_system_default_is_usable() {
    let system = ObservabilitySystem::default();
    assert!(system.get_uptime() < Duration::from_secs(5));
}
