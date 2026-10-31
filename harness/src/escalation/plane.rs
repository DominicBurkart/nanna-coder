use super::{
    default_window, EscalationError, EscalationLog, EscalationSink, Escalator, EscalatorSpawnHook,
    FanoutSink, GithubIssueSink, LocalRecordSink, RateLimitedSink, WebhookSink,
    DEFAULT_RECORD_LIMIT_BYTES, DEFAULT_REMOTE_CAP,
};
use crate::backlog::{ReqwestGithubClient, GITHUB_API_URL};
use crate::leases::{Clock, SystemClock};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

pub const ESCALATION_REPO_ENV: &str = "NANNA_ESCALATION_REPO";
pub const ESCALATION_GITHUB_TOKEN_ENV: &str = "NANNA_ESCALATION_GITHUB_TOKEN";
pub const GITHUB_TOKEN_ENV: &str = "GITHUB_TOKEN";
pub const ESCALATION_WEBHOOK_URL_ENV: &str = "NANNA_ESCALATION_WEBHOOK_URL";
pub const RECORD_FILE_NAME: &str = "escalation_records.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SinkConfigError {
    #[error("{ESCALATION_REPO_ENV} must be `owner/name`")]
    InvalidRepo,
    #[error("{ESCALATION_REPO_ENV} is set but neither {ESCALATION_GITHUB_TOKEN_ENV} nor {GITHUB_TOKEN_ENV} is")]
    MissingToken,
    #[error("{ESCALATION_WEBHOOK_URL_ENV} must be an http or https URL with a host")]
    InvalidWebhookUrl,
    #[error("{0} is not valid unicode")]
    NotUnicode(&'static str),
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct SinkSettings {
    repo: Option<String>,
    token: Option<String>,
    webhook_url: Option<String>,
}

impl std::fmt::Debug for SinkSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SinkSettings")
            .field("repo", &self.repo)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .field(
                "webhook_url",
                &self.webhook_url.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

fn read(
    lookup: &dyn Fn(&str) -> Option<OsString>,
    key: &'static str,
) -> Result<Option<String>, SinkConfigError> {
    match lookup(key) {
        None => Ok(None),
        Some(value) => {
            let text = value
                .into_string()
                .map_err(|_| SinkConfigError::NotUnicode(key))?;
            let text = text.trim();
            Ok((!text.is_empty()).then(|| text.to_string()))
        }
    }
}

impl SinkSettings {
    /// Read the sink configuration through `lookup`. A repository sink needs
    /// both `NANNA_ESCALATION_REPO` and a token; a webhook needs an http(s)
    /// URL; a malformed value is an error and an absent one disables that
    /// sink. A token alone never enables anything.
    ///
    /// ```
    /// use harness::escalation::{SinkConfigError, SinkSettings};
    /// use std::ffi::OsString;
    ///
    /// let none = SinkSettings::from_lookup(&|_| None).unwrap();
    /// assert!(!none.has_remote_sink());
    ///
    /// let webhook = |key: &str| (key == "NANNA_ESCALATION_WEBHOOK_URL").then(|| OsString::from("https://hooks.example.com/x"));
    /// assert!(SinkSettings::from_lookup(&webhook).unwrap().has_remote_sink());
    ///
    /// let repo_without_token = |key: &str| (key == "NANNA_ESCALATION_REPO").then(|| OsString::from("example/repo"));
    /// assert_eq!(SinkSettings::from_lookup(&repo_without_token).unwrap_err(), SinkConfigError::MissingToken);
    /// ```
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<OsString>) -> Result<Self, SinkConfigError> {
        let repo = read(lookup, ESCALATION_REPO_ENV)?;
        if let Some(repo) = &repo {
            if !valid_repo(repo) {
                return Err(SinkConfigError::InvalidRepo);
            }
        }
        let token = match read(lookup, ESCALATION_GITHUB_TOKEN_ENV)? {
            Some(token) => Some(token),
            None => read(lookup, GITHUB_TOKEN_ENV)?,
        };
        if repo.is_some() && token.is_none() {
            return Err(SinkConfigError::MissingToken);
        }
        let webhook_url = read(lookup, ESCALATION_WEBHOOK_URL_ENV)?;
        if let Some(url) = &webhook_url {
            match reqwest::Url::parse(url) {
                Ok(parsed)
                    if matches!(parsed.scheme(), "http" | "https")
                        && parsed.host_str().is_some() => {}
                _ => return Err(SinkConfigError::InvalidWebhookUrl),
            }
        }
        Ok(Self {
            repo,
            token,
            webhook_url,
        })
    }

    pub fn has_remote_sink(&self) -> bool {
        self.repo.is_some() || self.webhook_url.is_some()
    }

    /// The production fan-out: the local record first, then, when a remote
    /// sink is configured, the GitHub and webhook sinks behind a per-severity
    /// cap of [`DEFAULT_REMOTE_CAP`] deliveries per [`default_window`].
    ///
    /// ```
    /// use harness::escalation::{Escalation, EscalationSource, Severity, SinkSettings};
    /// use harness::leases::SystemClock;
    /// use std::sync::Arc;
    ///
    /// # tokio::runtime::Runtime::new().unwrap().block_on(async {
    /// let dir = tempfile::tempdir().unwrap();
    /// let sink = SinkSettings::default().build(dir.path().join("records.jsonl"), "https://api.github.com", Arc::new(SystemClock));
    /// let escalation = Escalation::new(Severity::Info, EscalationSource::Manual, "example/repo", "hello");
    /// sink.deliver(&escalation).await.unwrap();
    /// assert!(std::fs::read_to_string(dir.path().join("records.jsonl")).unwrap().contains("hello"));
    /// # });
    /// ```
    pub fn build(
        &self,
        record_path: impl Into<PathBuf>,
        github_api_url: &str,
        clock: Arc<dyn Clock>,
    ) -> Arc<dyn EscalationSink> {
        let limit = DEFAULT_RECORD_LIMIT_BYTES;
        let local = LocalRecordSink::new(record_path, limit);
        let local: Box<dyn EscalationSink> = Box::new(local);
        let mut remote: Vec<Box<dyn EscalationSink>> = Vec::new();
        if let (Some(repo), Some(token)) = (&self.repo, &self.token) {
            let client = ReqwestGithubClient::new(github_api_url, Some(token.clone()));
            remote.push(Box::new(GithubIssueSink::for_repo(Arc::new(client), repo)));
        }
        if let Some(url) = &self.webhook_url {
            remote.push(Box::new(WebhookSink::new(url.clone())));
        }
        if remote.is_empty() {
            tracing::warn!("no human-facing escalation sink is configured (set {ESCALATION_REPO_ENV} and a token, or {ESCALATION_WEBHOOK_URL_ENV}); escalations are only written to the local record");
            return Arc::new(FanoutSink(vec![local]));
        }
        let inner = Arc::new(FanoutSink(remote));
        let (cap, window) = (DEFAULT_REMOTE_CAP, default_window());
        let capped = RateLimitedSink::new(inner, clock, cap, window);
        Arc::new(FanoutSink(vec![local, Box::new(capped)]))
    }
}

fn valid_repo(repo: &str) -> bool {
    crate::backlog::is_valid_repo(repo)
}

/// The escalation log and the sink every producer of a process shares, so
/// one rate limit and one record cover the spawn gate, the action gate, the
/// cost accountant and the protected-path hook.
///
/// ```
/// use harness::escalation::{EscalationLog, EscalationPlane};
/// use std::sync::Arc;
///
/// let dir = tempfile::tempdir().unwrap();
/// let log = Arc::new(EscalationLog::in_memory());
/// let plane = EscalationPlane::from_lookup(log, dir.path(), &|_| None).unwrap();
/// assert!(Arc::ptr_eq(plane.log(), plane.escalator().log()));
/// ```
#[derive(Clone)]
pub struct EscalationPlane {
    log: Arc<EscalationLog>,
    sink: Arc<dyn EscalationSink>,
}

impl EscalationPlane {
    pub fn new(log: Arc<EscalationLog>, sink: Arc<dyn EscalationSink>) -> Self {
        Self { log, sink }
    }

    pub fn from_lookup(
        log: Arc<EscalationLog>,
        record_dir: &std::path::Path,
        lookup: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Self, SinkConfigError> {
        let settings = SinkSettings::from_lookup(lookup)?;
        let sink = settings.build(
            record_dir.join(RECORD_FILE_NAME),
            GITHUB_API_URL,
            Arc::new(SystemClock),
        );
        Ok(Self::new(log, sink))
    }

    pub fn log(&self) -> &Arc<EscalationLog> {
        &self.log
    }

    pub fn sink(&self) -> Arc<dyn EscalationSink> {
        Arc::clone(&self.sink)
    }

    pub fn escalator(&self) -> Arc<Escalator> {
        Arc::new(Escalator::new(
            Arc::clone(&self.log),
            Arc::clone(&self.sink),
            Arc::new(SystemClock),
            default_window(),
        ))
    }

    pub fn spawn_hook(&self) -> EscalatorSpawnHook {
        EscalatorSpawnHook::new(self.escalator())
    }
}

pub struct UndeliveredSink;

#[async_trait::async_trait]
impl EscalationSink for UndeliveredSink {
    fn name(&self) -> &str {
        "undelivered"
    }

    async fn deliver(
        &self,
        _escalation: &super::Escalation,
    ) -> Result<super::DeliveryReceipt, EscalationError> {
        Err(EscalationError::NoSink)
    }
}

impl From<Arc<EscalationLog>> for EscalationPlane {
    fn from(log: Arc<EscalationLog>) -> Self {
        Self::new(log, Arc::new(UndeliveredSink))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::escalation::sink::tests::RecordingSink;
    use crate::escalation::{Escalation, EscalationSource, Severity};
    use std::collections::HashMap;

    const SECRET_TOKEN: &str = "ghp_PLANTEDtoken0123456789abcdefghijkl";
    const SECRET_URL: &str = "https://hooks.example.com/services/T0P-S3CR3T";

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).map(OsString::from)
    }

    fn settings(pairs: &[(&str, &str)]) -> Result<SinkSettings, SinkConfigError> {
        SinkSettings::from_lookup(&lookup(pairs))
    }

    fn escalation(summary: &str) -> Escalation {
        Escalation::new(
            Severity::Blocked,
            EscalationSource::Auditor,
            "example/repo",
            summary,
        )
    }

    #[test]
    fn an_empty_environment_configures_no_remote_sink() {
        let none = settings(&[]).unwrap();
        assert!(!none.has_remote_sink());
        assert_eq!(none, SinkSettings::default());
        let blanks = settings(&[
            (ESCALATION_REPO_ENV, "  "),
            (ESCALATION_WEBHOOK_URL_ENV, ""),
            (GITHUB_TOKEN_ENV, "x"),
        ])
        .unwrap();
        assert!(!blanks.has_remote_sink());
    }

    #[test]
    fn a_token_alone_never_enables_the_github_sink() {
        let only_token = settings(&[(GITHUB_TOKEN_ENV, SECRET_TOKEN)]).unwrap();
        assert!(!only_token.has_remote_sink());
    }

    #[test]
    fn a_repo_needs_a_token_and_a_valid_name() {
        assert_eq!(
            settings(&[(ESCALATION_REPO_ENV, "example/repo")]).unwrap_err(),
            SinkConfigError::MissingToken
        );
        for bad in ["repo", "a/b/c", "../x", "o/r?x", "o/r#x", "o /r"] {
            assert_eq!(
                settings(&[(ESCALATION_REPO_ENV, bad), (GITHUB_TOKEN_ENV, "t")]).unwrap_err(),
                SinkConfigError::InvalidRepo,
                "{bad}"
            );
        }
        let ok = settings(&[
            (ESCALATION_REPO_ENV, "example/repo"),
            (GITHUB_TOKEN_ENV, "t"),
        ])
        .unwrap();
        assert!(ok.has_remote_sink());
    }

    #[test]
    fn the_dedicated_token_wins_over_the_general_one() {
        let both = settings(&[
            (ESCALATION_REPO_ENV, "example/repo"),
            (ESCALATION_GITHUB_TOKEN_ENV, "dedicated"),
            (GITHUB_TOKEN_ENV, "general"),
        ])
        .unwrap();
        assert_eq!(both.token.as_deref(), Some("dedicated"));
        let fallback = settings(&[
            (ESCALATION_REPO_ENV, "example/repo"),
            (GITHUB_TOKEN_ENV, "general"),
        ])
        .unwrap();
        assert_eq!(fallback.token.as_deref(), Some("general"));
    }

    #[test]
    fn a_webhook_url_must_be_http_or_https_with_a_host() {
        for bad in [
            "not a url",
            "ftp://hooks.example.com/x",
            "file:///etc/passwd",
            "http://",
            "javascript:alert(1)",
        ] {
            let err = settings(&[(ESCALATION_WEBHOOK_URL_ENV, bad)]).unwrap_err();
            assert_eq!(err, SinkConfigError::InvalidWebhookUrl, "{bad}");
            assert!(!err.to_string().contains(bad));
        }
        for ok in ["http://127.0.0.1:9/hook", SECRET_URL] {
            assert!(settings(&[(ESCALATION_WEBHOOK_URL_ENV, ok)])
                .unwrap()
                .has_remote_sink());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_non_unicode_value_is_a_configuration_error() {
        use std::os::unix::ffi::OsStringExt;
        let lookup = |key: &str| {
            (key == ESCALATION_WEBHOOK_URL_ENV).then(|| OsString::from_vec(vec![0xff, 0xfe]))
        };
        assert_eq!(
            SinkSettings::from_lookup(&lookup).unwrap_err(),
            SinkConfigError::NotUnicode(ESCALATION_WEBHOOK_URL_ENV)
        );
    }

    #[test]
    fn debug_output_never_shows_the_token_or_the_webhook_url() {
        let configured = settings(&[
            (ESCALATION_REPO_ENV, "example/repo"),
            (ESCALATION_GITHUB_TOKEN_ENV, SECRET_TOKEN),
            (ESCALATION_WEBHOOK_URL_ENV, SECRET_URL),
        ])
        .unwrap();
        let shown = format!("{configured:?}");
        assert!(
            !shown.contains(SECRET_TOKEN) && !shown.contains("S3CR3T"),
            "{shown}"
        );
        assert!(shown.contains("example/repo"));
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn config_error_messages_name_the_variables_and_never_a_value() {
        for err in [
            SinkConfigError::InvalidRepo,
            SinkConfigError::MissingToken,
            SinkConfigError::InvalidWebhookUrl,
        ] {
            assert!(err.to_string().contains("NANNA_ESCALATION_"), "{err}");
        }
    }

    #[tokio::test]
    async fn the_unconfigured_sink_writes_the_local_record_and_only_that() {
        let dir = tempfile::tempdir().unwrap();
        let sink = SinkSettings::default().build(
            dir.path().join(RECORD_FILE_NAME),
            "http://127.0.0.1:1",
            Arc::new(SystemClock),
        );
        let receipt = sink.deliver(&escalation("only local")).await.unwrap();
        let text = std::fs::read_to_string(dir.path().join(RECORD_FILE_NAME)).unwrap();
        assert!(text.contains("only local"));
        assert_eq!(receipt.reference, "1 sinks");
    }

    #[tokio::test]
    async fn from_lookup_surfaces_a_config_error_before_any_sink_exists() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(EscalationLog::in_memory());
        let err = EscalationPlane::from_lookup(
            log,
            dir.path(),
            &lookup(&[(ESCALATION_WEBHOOK_URL_ENV, "ftp://x/y")]),
        )
        .err()
        .unwrap();
        assert_eq!(err, SinkConfigError::InvalidWebhookUrl);
    }

    #[tokio::test]
    async fn from_lookup_records_into_the_directory_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(EscalationLog::in_memory());
        let plane =
            EscalationPlane::from_lookup(Arc::clone(&log), dir.path(), &lookup(&[])).unwrap();
        assert!(Arc::ptr_eq(plane.log(), &log));
        plane
            .escalator()
            .escalate(escalation("kept"))
            .await
            .unwrap();
        let text = std::fs::read_to_string(dir.path().join(RECORD_FILE_NAME)).unwrap();
        assert!(text.contains("kept"));
        assert_eq!(log.states().len(), 1);
    }

    #[tokio::test]
    async fn a_bare_log_plane_fails_loudly_and_is_retried_not_collapsed() {
        let log = Arc::new(EscalationLog::in_memory());
        let plane = EscalationPlane::from(Arc::clone(&log));
        let escalator = plane.escalator();
        for _ in 0..2 {
            let err = escalator.escalate(escalation("x")).await.unwrap_err();
            assert!(matches!(err, EscalationError::NoSink), "{err}");
        }
        assert_eq!(log.states()[0].count, 2);
        assert_eq!(UndeliveredSink.name(), "undelivered");
    }

    #[tokio::test]
    async fn the_plane_hands_out_the_sink_and_hook_it_was_built_with() {
        let log = Arc::new(EscalationLog::in_memory());
        let recording = Arc::new(RecordingSink::new("rec", false));
        let plane = EscalationPlane::new(Arc::clone(&log), recording.clone());
        plane.sink().deliver(&escalation("direct")).await.unwrap();
        plane
            .escalator()
            .escalate(escalation("through"))
            .await
            .unwrap();
        let seen = recording.seen.lock().unwrap();
        assert_eq!(
            seen.iter().map(|e| e.summary.as_str()).collect::<Vec<_>>(),
            vec!["direct", "through"]
        );
        let _hook = plane.spawn_hook();
    }
}
