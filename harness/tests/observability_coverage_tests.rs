use harness::observability::{AlertPolicy, HealthThreshold, ObservabilitySystem};
use std::time::Duration;

#[test]
fn observability_system_uptime_increases() {
    let system = ObservabilitySystem::new();
    let t1 = system.get_uptime();
    std::thread::sleep(Duration::from_millis(5));
    let t2 = system.get_uptime();
    assert!(t2 >= t1);
}

#[test]
fn observability_system_with_custom_health_threshold() {
    let threshold = HealthThreshold {
        cpu_threshold: 70.0,
        memory_threshold: 80.0,
        disk_threshold: 85.0,
        max_latency_ms: 1000,
        min_cache_hit_rate: 0.7,
        max_error_rate: 0.1,
        container_timeout: Duration::from_secs(15),
    };
    let system = ObservabilitySystem::new().with_health_thresholds(threshold);
    drop(system);
}

#[test]
fn observability_system_with_immediate_critical_alert_policy() {
    let policy = AlertPolicy::immediate_critical();
    let system = ObservabilitySystem::new().with_alert_policy(policy);
    drop(system);
}

#[tokio::test]
async fn observability_system_start_and_stop_monitoring() {
    let mut system = ObservabilitySystem::new();
    assert!(system.start_monitoring().await.is_ok());
    system.stop_monitoring().await;
}

#[tokio::test]
async fn observability_system_stop_without_start_is_safe() {
    let mut system = ObservabilitySystem::new();
    system.stop_monitoring().await;
}

#[test]
fn health_threshold_default_is_well_formed() {
    let t = HealthThreshold::default();
    assert!(t.cpu_threshold > 0.0 && t.cpu_threshold <= 100.0);
    assert!(t.memory_threshold > 0.0 && t.memory_threshold <= 100.0);
    assert!(t.max_latency_ms > 0);
    assert!(t.max_error_rate > 0.0 && t.max_error_rate < 1.0);
}

#[test]
fn alert_policy_balanced_has_grouping_rules() {
    let policy = AlertPolicy::balanced();
    assert!(!policy.escalation_rules.is_empty());
    assert!(!policy.grouping_rules.is_empty());
}

#[test]
fn alert_policy_immediate_critical_has_escalation_rules() {
    let policy = AlertPolicy::immediate_critical();
    assert!(!policy.escalation_rules.is_empty());
    assert!(!policy.notification_channels.is_empty());
}
