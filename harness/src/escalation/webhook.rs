use super::{DeliveryOutcome, DeliveryReceipt, Escalation, EscalationError, EscalationSink};
use async_trait::async_trait;

/// Posts [`Escalation::to_json`] to a configured URL for pager and chat
/// integrations. Any 2xx response is a delivery. The URL is a credential
/// for most chat and pager hooks, so receipts, errors, logs and `Debug`
/// carry only its scheme, host and port ([`origin`](Self::origin)); every
/// request is bounded by a timeout.
#[derive(Clone)]
pub struct WebhookSink {
    http: reqwest::Client,
    url: String,
}

impl std::fmt::Debug for WebhookSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSink")
            .field("origin", &self.origin())
            .finish_non_exhaustive()
    }
}

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

fn client(timeout: std::time::Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT.min(timeout))
        .timeout(timeout)
        .build()
        .expect("a client with only timeouts configured always builds")
}

impl WebhookSink {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            http: client(REQUEST_TIMEOUT),
            url: url.into(),
        }
    }

    pub fn origin(&self) -> String {
        match reqwest::Url::parse(&self.url) {
            Ok(url) if url.host_str().is_some() => {
                let mut origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
                if let Some(port) = url.port() {
                    origin.push_str(&format!(":{port}"));
                }
                origin
            }
            _ => "<invalid-url>".to_string(),
        }
    }

    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.http = client(timeout);
        self
    }
}

#[async_trait]
impl EscalationSink for WebhookSink {
    fn name(&self) -> &str {
        "webhook"
    }

    async fn deliver(&self, escalation: &Escalation) -> Result<DeliveryReceipt, EscalationError> {
        let payload = escalation.to_json();
        let response = self
            .http
            .post(&self.url)
            .header(reqwest::header::USER_AGENT, "nanna-coder")
            .json(&payload)
            .send()
            .await
            .map_err(|e| EscalationError::Http(e.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(EscalationError::Status {
                origin: self.origin(),
                status: status.as_u16(),
            });
        }
        tracing::info!(origin = %self.origin(), severity = %escalation.severity, "Escalation posted to webhook");
        Ok(DeliveryReceipt {
            sink: self.name().to_string(),
            reference: self.origin(),
            outcome: DeliveryOutcome::Posted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::{EscalationSource, Severity};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Accept one request per connection, capture its body and answer
    /// `status` when the target contains `/ok`, 500 otherwise.
    async fn capture_server() -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&bodies);
        let handle = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, content_length) = loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    raw.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while raw.len() < head_end + content_length {
                    let n = socket.read(&mut chunk).await.unwrap();
                    raw.extend_from_slice(&chunk[..n]);
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let status = if text.split_whitespace().nth(1).unwrap().contains("/ok") {
                    200
                } else {
                    500
                };
                seen.lock().unwrap().push(text[head_end..].to_string());
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        (base, bodies, handle)
    }

    const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

    fn escalation() -> Escalation {
        Escalation::new(
            Severity::Incident,
            EscalationSource::Rollout,
            "example/repo",
            format!("rollout halted; push used {TOKEN}"),
        )
        .with_id("e-1")
        .with_evidence(vec![
            format!("Authorization: Bearer {TOKEN}"),
            "password=hunter2".to_string(),
        ])
        .with_suggested_action(format!("rotate {TOKEN}"))
    }

    #[tokio::test]
    async fn posts_redacted_json_and_reports_non_success() {
        let (base, bodies, server) = capture_server().await;
        let sink = WebhookSink::new(format!("{base}/ok"));
        assert_eq!(sink.origin(), base);
        let receipt = sink.deliver(&escalation()).await.unwrap();
        assert_eq!(
            receipt,
            DeliveryReceipt {
                sink: "webhook".into(),
                reference: base.clone(),
                outcome: DeliveryOutcome::Posted
            }
        );
        let body = bodies.lock().unwrap()[0].clone();
        assert!(!body.contains(TOKEN), "token leaked: {body}");
        assert!(!body.contains("hunter2"));
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["id"], "e-1");
        assert_eq!(json["severity"], "incident");
        assert_eq!(
            json["summary"],
            "rollout halted; push used <redacted:github-token>"
        );
        assert_eq!(
            json["evidence"][0],
            "Authorization: Bearer <redacted:github-token>"
        );
        assert_eq!(json["evidence"][1], "password=<redacted:credential>");
        assert_eq!(json["suggested_action"], "rotate <redacted:github-token>");
        assert!(json["title"]
            .as_str()
            .unwrap()
            .contains("<redacted:github-token>"));
        assert!(json["body"]
            .as_str()
            .unwrap()
            .contains("## Production hold"));

        let failing = WebhookSink::new(format!("{base}/boom"));
        let err = failing.deliver(&escalation()).await.unwrap_err();
        assert!(matches!(err, EscalationError::Status { status: 500, .. }));
        assert_eq!(err.to_string(), format!("webhook {base} returned HTTP 500"));
        assert!(!bodies.lock().unwrap()[1].contains(TOKEN));
        server.abort();

        let unreachable = WebhookSink::new("http://127.0.0.1:1/hook");
        let err = unreachable.deliver(&escalation()).await.unwrap_err();
        assert!(matches!(err, EscalationError::Http(_)));
        assert!(err.to_string().starts_with("webhook request failed"));
    }

    const SECRET_PATH: &str = "T0P-S3CR3T-HOOK-PATH";

    #[tokio::test]
    async fn the_secret_webhook_url_never_reaches_a_receipt_error_or_debug_output() {
        let (base, _bodies, server) = capture_server().await;

        let ok = WebhookSink::new(format!("{base}/ok/{SECRET_PATH}"));
        let receipt = ok.deliver(&escalation()).await.unwrap();
        let rejected = WebhookSink::new(format!("{base}/boom/{SECRET_PATH}"));
        let status = rejected.deliver(&escalation()).await.unwrap_err();
        server.abort();
        let unreachable = WebhookSink::new(format!("http://127.0.0.1:1/hook/{SECRET_PATH}"));
        let transport = unreachable.deliver(&escalation()).await.unwrap_err();

        let everything = format!(
            "{receipt:?} {status} {status:?} {transport} {transport:?} {ok:?} {unreachable:?}"
        );
        assert!(!everything.contains(SECRET_PATH), "{everything}");
        assert!(!everything.contains("/ok/"), "{everything}");
        assert_eq!(receipt.reference, base);
    }

    #[tokio::test]
    async fn a_hung_endpoint_is_cut_off_by_the_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/ok", listener.local_addr().unwrap());
        let hold = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            drop(socket);
        });
        let sink = WebhookSink::new(url).with_timeout(std::time::Duration::from_millis(150));
        let started = std::time::Instant::now();
        let err = sink.deliver(&escalation()).await.unwrap_err();
        hold.abort();
        assert!(matches!(err, EscalationError::Http(_)), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn an_unparsable_url_has_a_placeholder_origin() {
        assert_eq!(WebhookSink::new("not a url").origin(), "<invalid-url>");
        assert_eq!(
            WebhookSink::new("https://hooks.example.com:8443/a/b?c=d").origin(),
            "https://hooks.example.com:8443"
        );
        assert_eq!(
            WebhookSink::new("https://hooks.example.com/a").origin(),
            "https://hooks.example.com"
        );
    }
}
