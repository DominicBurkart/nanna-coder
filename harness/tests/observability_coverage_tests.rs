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
fn observability_system_with_health_thresholds() {
    let threshold = HealthThreshold {
        metric_name: "error_rate".to_string(),
        warning_value: 0.05,
        critical_value: 0.15,
    };
    let system = ObservabilitySystem::new().with_health_thresholds(vec![threshold]);
    // just verifies construction doesn't panic
    drop(system);
}

#[test]
fn observability_system_with_immediate_critical_alert_policy() {
    let policy = AlertPolicy::immediate_critical();
    let system = ObservabilitySystem::new().with_alert_policy(policy);
    drop(system);
}

#[test]
fn observability_system_start_and_stop_monitoring() {
    let mut system = ObservabilitySystem::new();
    assert!(system.start_monitoring().is_ok());
    assert!(system.stop_monitoring().is_ok());
}

#[test]
fn observability_system_stop_monitoring_twice_is_safe() {
    let mut system = ObservabilitySystem::new();
    let _ = system.start_monitoring();
    let _ = system.stop_monitoring();
    // second stop should not panic
    let _ = system.stop_monitoring();
}

#[test]
fn health_threshold_default_is_well_formed() {
    let t = HealthThreshold::default();
    assert!(t.warning_value < t.critical_value);
    assert!(!t.metric_name.is_empty());
}
