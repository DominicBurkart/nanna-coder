use super::{DeliveryOutcome, DeliveryReceipt, Escalation, EscalationError, EscalationSink};
use crate::backlog::GithubClient;
use async_trait::async_trait;
use std::sync::Arc;

/// Label carried by every issue the [`GithubIssueSink`] files.
pub const ESCALATION_LABEL: &str = "nanna-escalation";

/// Files escalations as GitHub issues in the escalation's `repo`.
///
/// The issue title is [`Escalation::title`]. When an open issue with that
/// title and the [`ESCALATION_LABEL`] already exists, the sink comments on
/// it with the occurrence number instead of opening a duplicate.
pub struct GithubIssueSink {
    client: Arc<dyn GithubClient>,
}

impl GithubIssueSink {
    pub fn new(client: Arc<dyn GithubClient>) -> Self {
        Self { client }
    }

    /// GitHub search query that finds the open issue for `title`, if any.
    pub fn search_query(title: &str) -> String {
        format!(
            "label:{ESCALATION_LABEL} in:title \"{}\"",
            title.replace('"', "")
        )
    }
}

#[async_trait]
impl EscalationSink for GithubIssueSink {
    fn name(&self) -> &str {
        "github"
    }

    async fn deliver(&self, escalation: &Escalation) -> Result<DeliveryReceipt, EscalationError> {
        let title = escalation.title();
        let body = escalation.body();
        let repo = &escalation.repo;
        let open = self
            .client
            .search_open_issues(repo, &Self::search_query(&title))
            .await?;
        if let Some(issue) = open.into_iter().find(|i| i.title == title) {
            let comment = format!(
                "Occurrence #{} of this escalation.\n\n{body}",
                escalation.occurrence
            );
            self.client
                .comment_on_issue(repo, issue.number, &comment)
                .await?;
            tracing::info!(
                repo,
                issue = issue.number,
                occurrence = escalation.occurrence,
                "Escalation repeated on existing issue"
            );
            return Ok(DeliveryReceipt {
                sink: self.name().to_string(),
                reference: issue.html_url,
                outcome: DeliveryOutcome::Updated,
            });
        }
        let labels = vec![ESCALATION_LABEL.to_string()];
        let issue = self
            .client
            .create_issue(repo, &title, &body, &labels)
            .await?;
        tracing::info!(repo, issue = issue.number, severity = %escalation.severity, "Escalation filed as issue");
        Ok(DeliveryReceipt {
            sink: self.name().to_string(),
            reference: issue.html_url,
            outcome: DeliveryOutcome::Created,
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::backlog::{BacklogError, GithubIssue, GithubPullRequest};
    use crate::escalation::{EscalationSource, Severity};
    use std::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct MockGithub {
        pub issues: Mutex<Vec<(GithubIssue, Vec<String>)>>,
        pub comments: Mutex<Vec<(u64, String)>>,
        pub queries: Mutex<Vec<String>>,
        pub fail_create: bool,
    }

    #[async_trait]
    impl GithubClient for MockGithub {
        async fn search_open_issues(
            &self,
            _repo: &str,
            query: &str,
        ) -> Result<Vec<GithubIssue>, BacklogError> {
            self.queries.lock().unwrap().push(query.to_string());
            Ok(self
                .issues
                .lock()
                .unwrap()
                .iter()
                .map(|(i, _)| i.clone())
                .collect())
        }

        async fn open_pull_requests(
            &self,
            _repo: &str,
        ) -> Result<Vec<GithubPullRequest>, BacklogError> {
            Ok(vec![])
        }

        async fn create_issue(
            &self,
            repo: &str,
            title: &str,
            body: &str,
            labels: &[String],
        ) -> Result<GithubIssue, BacklogError> {
            if self.fail_create {
                return Err(BacklogError::Status {
                    url: repo.to_string(),
                    status: 403,
                });
            }
            let mut issues = self.issues.lock().unwrap();
            let number = issues.len() as u64 + 1;
            let issue = GithubIssue {
                number,
                title: title.to_string(),
                body: Some(body.to_string()),
                html_url: format!("https://example.invalid/{repo}/issues/{number}"),
            };
            issues.push((issue.clone(), labels.to_vec()));
            Ok(issue)
        }

        async fn comment_on_issue(
            &self,
            _repo: &str,
            number: u64,
            body: &str,
        ) -> Result<(), BacklogError> {
            self.comments
                .lock()
                .unwrap()
                .push((number, body.to_string()));
            Ok(())
        }
    }

    fn escalation(summary: &str) -> Escalation {
        Escalation::new(
            Severity::Blocked,
            EscalationSource::Auditor,
            "example/repo",
            summary,
        )
        .with_evidence(vec![
            "token=ghp_abcdefghijklmnopqrstuvwxyz0123456789".to_string()
        ])
    }

    #[tokio::test]
    async fn creates_a_labelled_issue_with_deterministic_title_then_comments_on_repeats() {
        let github = Arc::new(MockGithub::default());
        let sink = GithubIssueSink::new(github.clone());
        let first = escalation("no card fits \"deploy\"");
        let receipt = sink.deliver(&first).await.unwrap();
        assert_eq!(
            receipt,
            DeliveryReceipt {
                sink: "github".into(),
                reference: "https://example.invalid/example/repo/issues/1".into(),
                outcome: DeliveryOutcome::Created
            }
        );
        {
            let issues = github.issues.lock().unwrap();
            assert_eq!(issues.len(), 1);
            assert_eq!(issues[0].0.title, first.title());
            assert_eq!(issues[0].1, vec![ESCALATION_LABEL.to_string()]);
            let body = issues[0].0.body.as_deref().unwrap();
            assert!(body.contains("token=<redacted:credential>") && !body.contains("ghp_"));
        }
        assert_eq!(
            github.queries.lock().unwrap()[0],
            format!(
                "label:nanna-escalation in:title \"{}\"",
                first.title().replace('"', "")
            )
        );

        let repeat = escalation("no card fits \"deploy\"").with_occurrence(4);
        let receipt = sink.deliver(&repeat).await.unwrap();
        assert_eq!(receipt.outcome, DeliveryOutcome::Updated);
        assert_eq!(
            receipt.reference,
            "https://example.invalid/example/repo/issues/1"
        );
        assert_eq!(github.issues.lock().unwrap().len(), 1);
        let comments = github.comments.lock().unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].0, 1);
        assert!(comments[0]
            .1
            .starts_with("Occurrence #4 of this escalation.\n\n**Severity:** blocked"));
        assert!(!comments[0].1.contains("ghp_"));
    }

    #[tokio::test]
    async fn different_summary_opens_a_second_issue_and_errors_propagate() {
        let github = Arc::new(MockGithub::default());
        let sink = GithubIssueSink::new(github.clone());
        sink.deliver(&escalation("first problem")).await.unwrap();
        sink.deliver(&escalation("second problem")).await.unwrap();
        assert_eq!(github.issues.lock().unwrap().len(), 2);
        assert!(github.comments.lock().unwrap().is_empty());

        let failing = GithubIssueSink::new(Arc::new(MockGithub {
            fail_create: true,
            ..MockGithub::default()
        }));
        let err = failing.deliver(&escalation("third")).await.unwrap_err();
        assert!(matches!(
            err,
            EscalationError::Github(BacklogError::Status { status: 403, .. })
        ));
        assert!(err
            .to_string()
            .starts_with("GitHub delivery failed: GitHub returned HTTP 403"));
    }
}
