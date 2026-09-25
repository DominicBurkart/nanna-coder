//! HTTP endpoint probing: the first real [`HealthSource`].
//!
//! [`EndpointHealthSource`] probes a fixed set of paths against a base URL
//! once per [`sample`](HealthSource::sample) call and keeps a rolling
//! buffer, stamped by an injected [`Clock`], so `error_rate` and
//! `p99_latency_ms` are computed over the trailing window rather than a
//! single poll. The transport is an injectable [`HttpProbe`]:
//! [`FakeHttpProbe`] scripts responses for tests, [`ReqwestProbe`] is the
//! real implementation.

use super::adapter::Slot;
use super::health::{HealthError, HealthSample, HealthSource};
use crate::leases::Clock;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// One probe's outcome: the status answered and how long it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeResponse {
    /// HTTP status code.
    pub status: u16,
    /// Round-trip time in milliseconds.
    pub latency_ms: u32,
}

/// Fetches one URL. Real endpoint probing goes through [`ReqwestProbe`];
/// [`FakeHttpProbe`] scripts responses so [`EndpointHealthSource`] can be
/// tested without a server.
#[async_trait]
pub trait HttpProbe: Send + Sync {
    /// Probe `url`, or fail if it could not be reached at all (a non-2xx
    /// status is still `Ok`; only a transport failure is `Err`).
    async fn probe(&self, url: &str) -> Result<ProbeResponse, HealthError>;
}

/// A single entry in [`EndpointHealthSource`]'s rolling buffer.
#[derive(Debug, Clone)]
struct ProbeRecord {
    at: DateTime<Utc>,
    endpoint: String,
    status: Option<u16>,
    latency_ms: u32,
}

/// Probes a fixed set of paths against `base_url` and aggregates the
/// trailing [`sample`](HealthSource::sample) window into error rate, p99
/// latency and per-endpoint status, the shape [`HealthSource`] needs.
///
/// Each call probes every endpoint once, appends the outcomes (stamped by
/// the injected [`Clock`]) to a rolling buffer, drops entries older than
/// `now - window`, and aggregates what remains. A transport failure counts
/// toward `error_rate` but leaves the endpoint out of `endpoint_statuses`
/// (it reads as [`HealthObservation::EndpointMissing`](super::health::HealthObservation::EndpointMissing)
/// downstream rather than inventing a status code).
///
/// ```
/// use chrono::Duration;
/// use harness::leases::SystemClock;
/// use harness::rollout::{EndpointHealthSource, FakeHttpProbe, HealthSource, Slot};
/// use std::sync::Arc;
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let probe = Arc::new(FakeHttpProbe::healthy());
/// let source = EndpointHealthSource::new(
///     "http://localhost:8080",
///     vec!["/health/v1".to_string()],
///     probe.clone(),
///     Arc::new(SystemClock),
/// );
/// let sample = source.sample(&Slot::new("slot-0"), Duration::minutes(1)).await.unwrap();
/// assert_eq!(sample.endpoint_statuses["/health/v1"], 200);
/// assert_eq!(sample.error_rate, 0.0);
/// assert_eq!(probe.calls(), vec!["http://localhost:8080/health/v1".to_string()]);
/// # });
/// ```
pub struct EndpointHealthSource {
    base_url: String,
    endpoints: Vec<String>,
    probe: Arc<dyn HttpProbe>,
    clock: Arc<dyn Clock>,
    history: Mutex<VecDeque<ProbeRecord>>,
}

impl EndpointHealthSource {
    /// Probe `endpoints` (paths, joined onto `base_url`) through `probe`,
    /// stamping the rolling buffer with `clock`.
    pub fn new(
        base_url: impl Into<String>,
        endpoints: Vec<String>,
        probe: Arc<dyn HttpProbe>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            endpoints,
            probe,
            clock,
            history: Mutex::new(VecDeque::new()),
        }
    }
}

#[async_trait]
impl HealthSource for EndpointHealthSource {
    async fn sample(&self, _slot: &Slot, window: Duration) -> Result<HealthSample, HealthError> {
        let now = self.clock.now();
        for endpoint in &self.endpoints {
            let url = format!("{}{endpoint}", self.base_url);
            let record = match self.probe.probe(&url).await {
                Ok(response) => ProbeRecord {
                    at: now,
                    endpoint: endpoint.clone(),
                    status: Some(response.status),
                    latency_ms: response.latency_ms,
                },
                Err(_) => ProbeRecord {
                    at: now,
                    endpoint: endpoint.clone(),
                    status: None,
                    latency_ms: 0,
                },
            };
            self.history.lock().unwrap().push_back(record);
        }
        let mut history = self.history.lock().unwrap();
        let cutoff = now - window;
        history.retain(|r| r.at >= cutoff);
        let mut statuses: BTreeMap<String, u16> = BTreeMap::new();
        let mut latencies: Vec<u32> = Vec::new();
        let mut total = 0usize;
        let mut errors = 0usize;
        for record in history.iter() {
            total += 1;
            match record.status {
                Some(status) => {
                    if !(200..300).contains(&status) {
                        errors += 1;
                    }
                    latencies.push(record.latency_ms);
                    statuses.insert(record.endpoint.clone(), status);
                }
                None => errors += 1,
            }
        }
        latencies.sort_unstable();
        let p99_latency_ms = percentile(&latencies, 0.99);
        let error_rate = if total == 0 {
            0.0
        } else {
            errors as f64 / total as f64
        };
        Ok(HealthSample {
            error_rate,
            p99_latency_ms,
            endpoint_statuses: statuses,
        })
    }
}

/// Nearest-rank percentile of an already-sorted, non-empty-or-empty slice.
fn percentile(sorted: &[u32], p: f64) -> u32 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// Answers scripted per URL, falling back to a default; every call is
/// recorded.
#[derive(Debug)]
pub struct FakeHttpProbe {
    default: Result<ProbeResponse, ()>,
    scripted: Mutex<HashMap<String, VecDeque<Result<ProbeResponse, ()>>>>,
    calls: Mutex<Vec<String>>,
}

impl FakeHttpProbe {
    /// A probe that answers `default` for any unscripted URL.
    pub fn new(default: ProbeResponse) -> Self {
        Self {
            default: Ok(default),
            scripted: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// A probe that answers `200` with a `5ms` latency everywhere.
    pub fn healthy() -> Self {
        Self::new(ProbeResponse {
            status: 200,
            latency_ms: 5,
        })
    }

    /// Queue `response` as the next answer for `url`.
    pub fn push(&self, url: &str, response: ProbeResponse) {
        self.scripted
            .lock()
            .unwrap()
            .entry(url.to_string())
            .or_default()
            .push_back(Ok(response));
    }

    /// Queue a transport failure as the next answer for `url`.
    pub fn push_failure(&self, url: &str) {
        self.scripted
            .lock()
            .unwrap()
            .entry(url.to_string())
            .or_default()
            .push_back(Err(()));
    }

    /// Every URL probed so far, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpProbe for FakeHttpProbe {
    async fn probe(&self, url: &str) -> Result<ProbeResponse, HealthError> {
        self.calls.lock().unwrap().push(url.to_string());
        let next = self
            .scripted
            .lock()
            .unwrap()
            .get_mut(url)
            .and_then(VecDeque::pop_front);
        next.unwrap_or(self.default)
            .map_err(|()| HealthError(format!("probe failed: {url}")))
    }
}

/// Probes over a real HTTP connection with [`reqwest`].
#[derive(Debug, Clone)]
pub struct ReqwestProbe {
    client: reqwest::Client,
}

impl ReqwestProbe {
    /// A probe with a `5s` request timeout.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for ReqwestProbe {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpProbe for ReqwestProbe {
    async fn probe(&self, url: &str) -> Result<ProbeResponse, HealthError> {
        let start = std::time::Instant::now();
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| HealthError(format!("{url}: {e}")))?;
        let latency_ms = u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX);
        Ok(ProbeResponse {
            status: response.status().as_u16(),
            latency_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leases::SimulatedClock;

    fn source(
        probe: Arc<FakeHttpProbe>,
        clock: Arc<SimulatedClock>,
        endpoints: &[&str],
    ) -> EndpointHealthSource {
        EndpointHealthSource::new(
            "http://fixture.invalid",
            endpoints.iter().map(|e| e.to_string()).collect(),
            probe,
            clock,
        )
    }

    fn slot() -> Slot {
        Slot::new("slot-0")
    }

    #[tokio::test]
    async fn healthy_probe_reports_zero_error_rate_and_every_status() {
        let clock = Arc::new(SimulatedClock::new(Utc::now()));
        let probe = Arc::new(FakeHttpProbe::healthy());
        let source = source(probe.clone(), clock.clone(), &["/health/v1", "/ready"]);
        let sample = source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        assert_eq!(sample.error_rate, 0.0);
        assert_eq!(sample.p99_latency_ms, 5);
        assert_eq!(sample.endpoint_statuses.len(), 2);
        assert_eq!(sample.endpoint_statuses["/health/v1"], 200);
        assert_eq!(sample.endpoint_statuses["/ready"], 200);
        assert_eq!(
            probe.calls(),
            vec![
                "http://fixture.invalid/health/v1".to_string(),
                "http://fixture.invalid/ready".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn degraded_latency_raises_the_windowed_p99() {
        let clock = Arc::new(SimulatedClock::new(Utc::now()));
        let probe = Arc::new(FakeHttpProbe::healthy());
        let source = source(probe.clone(), clock.clone(), &["/health/v1"]);
        source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        probe.push(
            "http://fixture.invalid/health/v1",
            ProbeResponse {
                status: 200,
                latency_ms: 900,
            },
        );
        let sample = source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        assert_eq!(sample.p99_latency_ms, 900);
        assert_eq!(sample.error_rate, 0.0);
    }

    #[tokio::test]
    async fn one_endpoint_returning_500_is_isolated_in_the_statuses_and_error_rate() {
        let clock = Arc::new(SimulatedClock::new(Utc::now()));
        let probe = Arc::new(FakeHttpProbe::healthy());
        let source = source(probe.clone(), clock.clone(), &["/health/v1", "/ready"]);
        probe.push(
            "http://fixture.invalid/health/v1",
            ProbeResponse {
                status: 500,
                latency_ms: 12,
            },
        );
        let sample = source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        assert_eq!(sample.endpoint_statuses["/health/v1"], 500);
        assert_eq!(sample.endpoint_statuses["/ready"], 200);
        assert_eq!(sample.error_rate, 0.5);
    }

    #[tokio::test]
    async fn a_transport_failure_counts_as_an_error_and_is_left_out_of_statuses() {
        let clock = Arc::new(SimulatedClock::new(Utc::now()));
        let probe = Arc::new(FakeHttpProbe::healthy());
        let source = source(probe.clone(), clock.clone(), &["/health/v1"]);
        probe.push_failure("http://fixture.invalid/health/v1");
        let sample = source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        assert!(!sample.endpoint_statuses.contains_key("/health/v1"));
        assert_eq!(sample.error_rate, 1.0);
        assert_eq!(sample.p99_latency_ms, 0);
    }

    #[tokio::test]
    async fn entries_older_than_the_window_are_pruned() {
        let clock = Arc::new(SimulatedClock::new(Utc::now()));
        let probe = Arc::new(FakeHttpProbe::healthy());
        let source = source(probe.clone(), clock.clone(), &["/health/v1"]);
        probe.push(
            "http://fixture.invalid/health/v1",
            ProbeResponse {
                status: 500,
                latency_ms: 1,
            },
        );
        source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        clock.advance(Duration::minutes(5));
        let sample = source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        assert_eq!(sample.endpoint_statuses["/health/v1"], 200);
        assert_eq!(sample.error_rate, 0.0);
    }

    #[test]
    fn percentile_of_an_empty_slice_is_zero() {
        assert_eq!(percentile(&[], 0.99), 0);
        assert_eq!(percentile(&[10, 20, 30], 0.99), 30);
        assert_eq!(percentile(&[10], 0.99), 10);
    }

    #[tokio::test]
    async fn no_endpoints_configured_reports_a_healthy_empty_sample() {
        let clock = Arc::new(SimulatedClock::new(Utc::now()));
        let probe = Arc::new(FakeHttpProbe::healthy());
        let source = source(probe, clock, &[]);
        let sample = source.sample(&slot(), Duration::minutes(1)).await.unwrap();
        assert_eq!(sample.error_rate, 0.0);
        assert_eq!(sample.p99_latency_ms, 0);
        assert!(sample.endpoint_statuses.is_empty());
    }

    #[tokio::test]
    async fn reqwest_probe_default_builds_a_usable_client() {
        let probe = ReqwestProbe::default();
        let err = probe.probe("http://127.0.0.1:1").await.unwrap_err();
        assert!(err.to_string().contains("127.0.0.1:1"));
    }

    #[tokio::test]
    async fn reqwest_probe_reports_status_and_a_measured_latency() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let response =
                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        let probe = ReqwestProbe::new();
        let response = probe
            .probe(&format!("http://{addr}/health/v1"))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
    }

    #[tokio::test]
    async fn reqwest_probe_reports_a_transport_error_when_nothing_is_listening() {
        let probe = ReqwestProbe::new();
        let err = probe.probe("http://127.0.0.1:1").await.unwrap_err();
        assert!(err.to_string().contains("127.0.0.1:1"));
    }
}
