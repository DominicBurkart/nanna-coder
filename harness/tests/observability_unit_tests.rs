use harness::monitoring::AlertSeverity;
use harness::observability::{
    AlertPolicy, HealthThreshold, ObservabilitySystem, SlaStatus, TrendDirection,
};
use std::time::Duration;

#[test]
fn builder_with_health_thresholds_does_not_panic() {
    let _system = ObservabilitySystem::new().with_health_thresholds(HealthThreshold::default());
}

#[test]
fn builder_with_alert_policy_does_not_panic() {
    let _system = ObservabilitySystem::new().with_alert_policy(AlertPolicy::balanced());
}

#[test]
fn builder_with_health_check_interval_does_not_panic() {
    let _system =
        ObservabilitySystem::new().with_health_check_interval(Duration::from_secs(5));
}

#[test]
fn get_uptime_is_short_after_construction() {
    let system = ObservabilitySystem::new();
    let uptime = system.get_uptime();
    assert!(uptime < Duration::from_secs(5));
}

#[tokio::test]
async fn start_and_stop_monitoring_does_not_hang() {
    let mut system = ObservabilitySystem::new();
    system.start_monitoring().await.unwrap();
    system.stop_monitoring().await;
}

#[tokio::test]
async fn comprehensive_status_sla_is_at_risk_with_default_system() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
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
    assert_eq!(
        status.performance_trends.cache_performance_trend,
        TrendDirection::Degrading
    );
}

#[tokio::test]
async fn comprehensive_status_performance_score_below_100_with_degrading_cache() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    let score = status.performance_trends.performance_score;
    assert!((0.0..100.0).contains(&score));
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

#[test]
fn sla_status_variants_are_distinct() {
    assert_ne!(SlaStatus::Compliant, SlaStatus::AtRisk);
    assert_ne!(SlaStatus::AtRisk, SlaStatus::Breached);
    assert_ne!(SlaStatus::Compliant, SlaStatus::Breached);
}

#[test]
fn trend_direction_all_variants_are_distinct() {
    assert_ne!(TrendDirection::Improving, TrendDirection::Stable);
    assert_ne!(TrendDirection::Stable, TrendDirection::Degrading);
    assert_ne!(TrendDirection::Degrading, TrendDirection::Unknown);
}

#[test]
fn alert_policy_immediate_critical_has_escalation_rules_for_critical() {
    let policy = AlertPolicy::immediate_critical();
    assert_eq!(policy.escalation_rules.len(), 2);
    assert!(policy
        .escalation_rules
        .iter()
        .any(|r| r.severity == AlertSeverity::Critical));
}

#[test]
fn alert_policy_balanced_has_grouping_rules() {
    let policy = AlertPolicy::balanced();
    assert!(!policy.grouping_rules.is_empty());
}

#[test]
fn health_threshold_default_values_are_sensible() {
    let t = HealthThreshold::default();
    assert!(t.cpu_threshold > 0.0);
    assert!(t.memory_threshold > 0.0);
    assert!(t.disk_threshold > 0.0);
    assert!(t.max_latency_ms > 0);
    assert!(t.min_cache_hit_rate > 0.0);
    assert!(t.max_error_rate > 0.0);
    assert!(t.container_timeout > Duration::ZERO);
}

#[tokio::test]
async fn comprehensive_status_model_summary_empty_initially() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    assert_eq!(status.model_summary.total_models, 0);
}

#[tokio::test]
async fn comprehensive_status_mtbf_and_mttr_are_set() {
    let mut system = ObservabilitySystem::new();
    let _ = system.initialize().await;
    let status = system.get_comprehensive_status().await.unwrap();
    assert!(status.availability_metrics.mtbf.is_some());
    assert!(status.availability_metrics.mttr.is_some());
}

#[test]
fn observability_system_default_is_usable() {
    let _system = ObservabilitySystem::default();
}
