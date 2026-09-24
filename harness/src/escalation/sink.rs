use super::Escalation;
use crate::backlog::BacklogError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What a sink did with an escalation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryOutcome {
    /// A new issue (or equivalent) was created.
    Created,
    /// An existing open issue for the same title was updated with a comment.
    Updated,
    /// A one-shot message was posted (webhook).
    Posted,
    /// Every child sink of a [`FanoutSink`] delivered.
    Fanout { receipts: Vec<DeliveryReceipt> },
}

/// Proof of delivery returned by [`EscalationSink::deliver`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryReceipt {
    /// [`EscalationSink::name`] of the sink that delivered.
    pub sink: String,
    /// Where the escalation landed: an issue URL, a webhook URL, and so on.
    pub reference: String,
    pub outcome: DeliveryOutcome,
}

/// One child sink's failure inside a [`FanoutSink`] delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinkFailure {
    pub sink: String,
    pub error: String,
}

fn describe(failed: &[SinkFailure]) -> String {
    failed
        .iter()
        .map(|f| format!("{}: {}", f.sink, f.error))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Why an escalation could not be delivered or recorded.
#[derive(Debug, Error)]
pub enum EscalationError {
    #[error("GitHub delivery failed: {0}")]
    Github(#[from] BacklogError),
    #[error("webhook request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("webhook {url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("escalation log I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("escalation log record is not valid JSON: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("no incident hold with id `{0}`")]
    UnknownHold(String),
    /// Some sinks of a [`FanoutSink`] failed; every sink was still attempted
    /// and `delivered` lists the ones that succeeded.
    #[error("{} of {} sinks failed: {}", failed.len(), failed.len() + delivered.len(), describe(failed))]
    Partial {
        delivered: Vec<DeliveryReceipt>,
        failed: Vec<SinkFailure>,
    },
}

/// A destination for escalations. Implementations must not send secrets:
/// use [`Escalation::title`], [`Escalation::body`] and
/// [`Escalation::to_json`], which are redacted, rather than the raw fields.
///
/// ```
/// use harness::escalation::{DeliveryOutcome, DeliveryReceipt, Escalation, EscalationError, EscalationSink, EscalationSource, Severity};
/// use std::sync::Mutex;
///
/// struct Notebook(Mutex<Vec<String>>);
///
/// #[async_trait::async_trait]
/// impl EscalationSink for Notebook {
///     fn name(&self) -> &str {
///         "notebook"
///     }
///     async fn deliver(&self, escalation: &Escalation) -> Result<DeliveryReceipt, EscalationError> {
///         self.0.lock().unwrap().push(escalation.body());
///         Ok(DeliveryReceipt { sink: "notebook".into(), reference: escalation.title(), outcome: DeliveryOutcome::Posted })
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let sink = Notebook(Mutex::new(vec![]));
/// let escalation = Escalation::new(Severity::Info, EscalationSource::Manual, "example/repo", "hello");
/// let receipt = sink.deliver(&escalation).await.unwrap();
/// assert_eq!(receipt.outcome, DeliveryOutcome::Posted);
/// assert!(sink.0.lock().unwrap()[0].contains("## Summary\n\nhello"));
/// # });
/// ```
#[async_trait]
pub trait EscalationSink: Send + Sync {
    /// Short stable name used in receipts and failure reports.
    fn name(&self) -> &str;

    /// Deliver `escalation` and say where it landed.
    async fn deliver(&self, escalation: &Escalation) -> Result<DeliveryReceipt, EscalationError>;
}

/// Deliver to several sinks at once. Every sink is attempted even after one
/// fails; the result is `Ok` with a [`DeliveryOutcome::Fanout`] receipt when
/// all succeed and [`EscalationError::Partial`] otherwise.
pub struct FanoutSink(pub Vec<Box<dyn EscalationSink>>);

#[async_trait]
impl EscalationSink for FanoutSink {
    fn name(&self) -> &str {
        "fanout"
    }

    async fn deliver(&self, escalation: &Escalation) -> Result<DeliveryReceipt, EscalationError> {
        let mut delivered = Vec::new();
        let mut failed = Vec::new();
        for sink in &self.0 {
            match sink.deliver(escalation).await {
                Ok(receipt) => delivered.push(receipt),
                Err(error) => failed.push(SinkFailure {
                    sink: sink.name().to_string(),
                    error: error.to_string(),
                }),
            }
        }
        if !failed.is_empty() {
            return Err(EscalationError::Partial { delivered, failed });
        }
        Ok(DeliveryReceipt {
            sink: self.name().to_string(),
            reference: format!("{} sinks", delivered.len()),
            outcome: DeliveryOutcome::Fanout {
                receipts: delivered,
            },
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::escalation::{EscalationSource, Severity};
    use std::sync::Mutex;

    /// Sink that records every delivery and fails when told to.
    pub(crate) struct RecordingSink {
        pub name: String,
        pub fail: bool,
        pub seen: Mutex<Vec<Escalation>>,
    }

    impl RecordingSink {
        pub(crate) fn new(name: &str, fail: bool) -> Self {
            Self {
                name: name.to_string(),
                fail,
                seen: Mutex::new(vec![]),
            }
        }
    }

    #[async_trait]
    impl EscalationSink for RecordingSink {
        fn name(&self) -> &str {
            &self.name
        }

        async fn deliver(
            &self,
            escalation: &Escalation,
        ) -> Result<DeliveryReceipt, EscalationError> {
            self.seen.lock().unwrap().push(escalation.clone());
            if self.fail {
                return Err(EscalationError::UnknownHold(format!(
                    "{} refused",
                    self.name
                )));
            }
            Ok(DeliveryReceipt {
                sink: self.name.clone(),
                reference: escalation.id.clone(),
                outcome: DeliveryOutcome::Posted,
            })
        }
    }

    fn escalation() -> Escalation {
        Escalation::new(
            Severity::Blocked,
            EscalationSource::Budget,
            "example/repo",
            "out of iterations",
        )
        .with_id("e1")
    }

    #[tokio::test]
    async fn fanout_delivers_to_every_sink_and_reports_all_receipts() {
        let fanout = FanoutSink(vec![
            Box::new(RecordingSink::new("a", false)),
            Box::new(RecordingSink::new("b", false)),
        ]);
        let receipt = fanout.deliver(&escalation()).await.unwrap();
        assert_eq!(receipt.sink, "fanout");
        assert_eq!(receipt.reference, "2 sinks");
        let DeliveryOutcome::Fanout { receipts } = receipt.outcome else {
            panic!("expected fanout")
        };
        assert_eq!(
            receipts.iter().map(|r| r.sink.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(FanoutSink(vec![]).deliver(&escalation()).await.is_ok());
    }

    #[tokio::test]
    async fn fanout_attempts_every_sink_after_a_failure_and_reports_partial() {
        let fanout = FanoutSink(vec![
            Box::new(RecordingSink::new("first", true)),
            Box::new(RecordingSink::new("second", false)),
            Box::new(RecordingSink::new("third", true)),
        ]);
        let err = fanout.deliver(&escalation()).await.unwrap_err();
        let EscalationError::Partial { delivered, failed } = &err else {
            panic!("expected partial: {err}")
        };
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].sink, "second");
        assert_eq!(
            failed,
            &vec![
                SinkFailure {
                    sink: "first".into(),
                    error: "no incident hold with id `first refused`".into()
                },
                SinkFailure {
                    sink: "third".into(),
                    error: "no incident hold with id `third refused`".into()
                }
            ]
        );
        assert_eq!(err.to_string(), "2 of 3 sinks failed: first: no incident hold with id `first refused`; third: no incident hold with id `third refused`");
        let json = serde_json::to_value(delivered).unwrap();
        assert_eq!(json[0]["outcome"]["kind"], "posted");
    }
}
