use super::{DeliveryOutcome, DeliveryReceipt, Escalation, EscalationError, EscalationSink};
use async_trait::async_trait;

/// Posts [`Escalation::to_json`] to a configured URL for pager and chat
/// integrations. Any 2xx response is a delivery.
#[derive(Debug, Clone)]
pub struct WebhookSink {
    http: reqwest::Client,
    url: String,
}

impl WebhookSink {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            url: url.into(),
        }
    }

    pub fn url(&self) -> &str {
        &self.url
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
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(EscalationError::Status {
                url: self.url.clone(),
                status: status.as_u16(),
            });
        }
        tracing::info!(url = %self.url, severity = %escalation.severity, "Escalation posted to webhook");
        Ok(DeliveryReceipt {
            sink: self.name().to_string(),
            reference: self.url.clone(),
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
        assert_eq!(sink.url(), format!("{base}/ok"));
        let receipt = sink.deliver(&escalation()).await.unwrap();
        assert_eq!(
            receipt,
            DeliveryReceipt {
                sink: "webhook".into(),
                reference: format!("{base}/ok"),
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
        assert_eq!(
            err.to_string(),
            format!("webhook {base}/boom returned HTTP 500")
        );
        assert!(!bodies.lock().unwrap()[1].contains(TOKEN));
        server.abort();

        let unreachable = WebhookSink::new("http://127.0.0.1:1/hook");
        let err = unreachable.deliver(&escalation()).await.unwrap_err();
        assert!(matches!(err, EscalationError::Http(_)));
        assert!(err.to_string().starts_with("webhook request failed"));
    }
}
