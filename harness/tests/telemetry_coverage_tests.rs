//! Tests for code paths in `harness::telemetry` that aren't exercised by the
//! existing inline tests.

use chrono::Utc;
use harness::telemetry::{MetricPoint, MetricType};
use harness::{PrometheusExporter, SpanStatus, TelemetryConfig, TelemetrySystem, TraceContext, TraceGuard};
use std::collections::HashMap;
use std::time::Duration;

// ── TraceContext builder / mutator methods ─────────────────────────────────

#[test]
fn trace_context_with_attribute_stores_values() {
    let trace = TraceContext::new("op")
        .with_attribute("key1", "val1")
        .with_attribute("key2", "val2");

    assert_eq!(trace.attributes.get("key1"), Some(&"val1".to_string()));
    assert_eq!(trace.attributes.get("key2"), Some(&"val2".to_string()));
}

#[test]
fn trace_context_set_status_updates_status() {
    let mut trace = TraceContext::new("op");
    assert_eq!(trace.status, SpanStatus::InProgress);

    trace.set_status(SpanStatus::Ok);
    assert_eq!(trace.status, SpanStatus::Ok);

    trace.set_status(SpanStatus::Error);
    assert_eq!(trace.status, SpanStatus::Error);

    trace.set_status(SpanStatus::Cancelled);
    assert_eq!(trace.status, SpanStatus::Cancelled);
}

// ── PrometheusExporter: clear_buffer ──────────────────────────────────────

#[tokio::test]
async fn prometheus_exporter_clear_buffer_empties_metrics() {
    let exporter = PrometheusExporter::new(None);

    let metric = MetricPoint {
        name: "test_metric".to_string(),
        metric_type: MetricType::Counter,
        value: 42.0,
        timestamp: Utc::now(),
        labels: HashMap::new(),
        unit: None,
        description: Some("Test".to_string()),
    };
    exporter.add_metric(metric);

    let before = exporter.export_prometheus().await.unwrap();
    assert!(before.contains("test_metric"));

    exporter.clear_buffer();

    let after = exporter.export_prometheus().await.unwrap();
    assert!(!after.contains("test_metric"));
}

// ── TelemetrySystem builder methods ───────────────────────────────────────

#[test]
fn telemetry_system_builder_chain_does_not_panic() {
    let config = TelemetryConfig {
        trace_sample_rate: 0.5,
        ..Default::default()
    };

    let _sys = TelemetrySystem::new()
        .with_service_name("test-svc")
        .with_version("1.2.3")
        .with_environment("staging")
        .with_global_attribute("region", "eu-west-1")
        .with_config(config);
}

#[test]
fn telemetry_system_get_uptime_is_small_for_new_instance() {
    let sys = TelemetrySystem::new();
    assert!(
        sys.get_uptime() < Duration::from_secs(5),
        "uptime of a freshly created system should be well under 5 s"
    );
}

#[test]
fn telemetry_system_get_prometheus_exporter_returns_none_by_default() {
    let sys = TelemetrySystem::new();
    assert!(
        sys.get_prometheus_exporter().is_none(),
        "no prometheus exporter is registered by default"
    );
}

#[tokio::test]
async fn telemetry_system_export_all_with_no_exporters_succeeds() {
    let sys = TelemetrySystem::new();
    sys.record_counter("c", 1.0, vec![]);
    sys.record_gauge("g", 2.0, vec![]);
    sys.export_all().await.unwrap();
}

// ── TraceGuard RAII behaviour ──────────────────────────────────────────────
// finish_trace internally calls tokio::spawn, so these tests must be async.

#[tokio::test]
async fn trace_guard_trace_returns_inner_context() {
    let sys = TelemetrySystem::new();
    let trace = sys.start_trace("guarded_op");
    let guard = TraceGuard::new(&sys, trace);

    let ctx = guard.trace().expect("trace should be Some inside guard");
    assert_eq!(ctx.operation_name, "guarded_op");
    // guard drops here, finish_trace spawns a task that completes in the runtime
}

#[tokio::test]
async fn trace_guard_record_error_and_set_status_do_not_panic() {
    let sys = TelemetrySystem::new();
    let trace = sys.start_trace("error_op");
    let mut guard = TraceGuard::new(&sys, trace);

    guard.record_error("something went wrong");
    guard.set_status(SpanStatus::Error);
    // Guard drops here, which exercises the Drop impl (and spawns a task)
}

#[tokio::test]
async fn trace_guard_drop_finishes_trace_and_decrements_active_count() {
    let sys = TelemetrySystem::new();
    assert_eq!(sys.get_active_trace_count(), 0);

    {
        let trace = sys.start_trace("auto_finish");
        let _guard = TraceGuard::new(&sys, trace);
        assert_eq!(sys.get_active_trace_count(), 1);
    } // _guard drops here → finish_trace is called (spawns task)

    assert_eq!(sys.get_active_trace_count(), 0);
}
