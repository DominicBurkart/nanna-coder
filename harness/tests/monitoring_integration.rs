//! Integration coverage for `harness::rollout`'s monitoring surface
//! (#652): a real network round trip through [`EndpointHealthSource`] and
//! [`ReqwestProbe`], driving the executor's actual breach and rollback
//! path.
//!
//! The fixture app this issue names (`tests/fixtures/fullstack/`, health
//! path `/health/v1`, `FIXTURE_BREAK_ROUTE=1`) is not part of this branch
//! (it belongs to a different lane's `tests/fixtures/` tree), so this test
//! stands up its own minimal HTTP stub instead, following the same raw
//! `tokio::net::TcpListener` pattern already used for Ollama stubs in
//! `harness/src/eval/runner.rs`. The stub always answers `/health/v1` with
//! `500`, playing the role of `FIXTURE_BREAK_ROUTE=1`.

use chrono::{Duration, Utc};
use harness::deploy::DeployTemplate;
use harness::leases::{InMemoryLeaseStore, SimulatedClock};
use harness::rollout::{
    EndpointHealthSource, FakeAdapter, ReqwestProbe, RolloutConfig, RolloutExecutor, RolloutLog,
    RolloutState,
};
use harness::windows::WindowSet;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PLAN: &str = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"gradual\"\nsteps = [100]\nmin_step_duration = \"1m\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 1.0\nlatency_p99_max_ms = 5000\nbake_time = \"5m\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\n";

/// Always answers `GET /health/v1` (and anything else) with `500`,
/// simulating the fixture's `FIXTURE_BREAK_ROUTE=1` behaviour.
async fn spawn_always_broken_stub() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let response =
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_broken_endpoint_breaches_health_within_one_poll_interval_and_rolls_back() {
    let addr = spawn_always_broken_stub().await;
    let dir = tempfile::tempdir().unwrap();
    let log = RolloutLog::open(&dir.path().join("rollouts.jsonl")).unwrap();
    let clock = Arc::new(SimulatedClock::new(Utc::now()));
    let leases = Arc::new(InMemoryLeaseStore::default());
    let adapter = Arc::new(FakeAdapter::new("registry.example.invalid/ns/app:v1"));
    let health = Arc::new(EndpointHealthSource::new(
        format!("http://{addr}"),
        vec!["/health/v1".to_string()],
        Arc::new(ReqwestProbe::new()),
        clock.clone(),
    ));
    let executor = RolloutExecutor::new(
        log,
        leases,
        WindowSet::default(),
        clock.clone(),
        adapter.clone(),
        health,
    )
    .with_config(RolloutConfig {
        poll_interval: Duration::minutes(1),
        lease_grace: Duration::hours(1),
    });
    let plan = DeployTemplate::parse(PLAN)
        .unwrap()
        .plan("sandbox")
        .unwrap();
    let record = executor
        .start(plan, "registry.example.invalid/ns/app:v2")
        .await
        .unwrap();
    let done = executor.run(&record.id).await.unwrap();

    assert_eq!(done.state, RolloutState::RolledBack);
    assert_eq!(adapter.current(), "registry.example.invalid/ns/app:v1");
    let breach = done.breach.expect("a health breach was recorded");
    assert!(
        clock.sleeps().is_empty(),
        "the breach must be caught on the very first poll, before any poll interval elapses; \
         sleeps recorded: {:?}",
        clock.sleeps()
    );
    assert_eq!(breach.step, 0);
    assert!(
        breach
            .evidence
            .iter()
            .any(|e| e.endpoint == "/health/v1" && e.status == 500),
        "evidence should carry the broken endpoint's status: {:?}",
        breach.evidence
    );
}
