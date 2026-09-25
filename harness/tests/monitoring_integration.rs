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
//! `harness/src/eval/runner.rs`. The stub answers `/health/v1` with `200`
//! for the first request and `500` (playing the role of
//! `FIXTURE_BREAK_ROUTE=1`) for every one after that, so the rollout is
//! seen healthy on its first poll and only regresses on the next one, the
//! way a real deploy-then-break would look.

use chrono::{Duration, Utc};
use harness::deploy::DeployTemplate;
use harness::leases::{InMemoryLeaseStore, SimulatedClock};
use harness::rollout::{
    EndpointHealthSource, FakeAdapter, ReqwestProbe, RolloutConfig, RolloutExecutor, RolloutLog,
    RolloutState,
};
use harness::windows::WindowSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PLAN: &str = "[target]\nkind = \"container-registry+serverless\"\nregistry = \"registry.example.invalid/ns\"\nimage = \"app\"\nenvironments = [\"sandbox\"]\n[risk]\nclass = \"internal\"\n[rollout]\nstrategy = \"gradual\"\nsteps = [100]\nmin_step_duration = \"1m\"\n[health]\nendpoints = [\"/health/v1\"]\nerror_rate_max = 0.5\nlatency_p99_max_ms = 5000\nbake_time = \"5m\"\n[rollback]\nautomatic = true\non_breach = \"rollback\"\n";

/// Answers `GET /health/v1` (and anything else) with `200` for the first
/// request, `500` after that.
async fn spawn_stub_that_breaks_after_the_first_request() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let requests = requests.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let response = if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_broken_endpoint_breaches_health_within_one_poll_interval_and_rolls_back() {
    let addr = spawn_stub_that_breaks_after_the_first_request().await;
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
    let poll_interval = Duration::minutes(1);
    let executor = RolloutExecutor::new(
        log,
        leases,
        WindowSet::default(),
        clock.clone(),
        adapter.clone(),
        health,
    )
    .with_config(RolloutConfig {
        poll_interval,
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
    assert_eq!(
        clock.sleeps(),
        vec![poll_interval],
        "the breach must be caught on the poll right after the one poll interval \
         that followed the healthy first poll; sleeps recorded: {:?}",
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
