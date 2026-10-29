use super::{Escalation, EscalationSource, Escalator, Severity};
use crate::protected::{AuditHook, ProtectedPathViolation};
use crate::task::TaskId;
use std::sync::Arc;

pub struct ProtectedPathEscalation {
    escalator: Arc<Escalator>,
    repo: String,
}

impl ProtectedPathEscalation {
    pub fn new(escalator: Arc<Escalator>, repo: impl Into<String>) -> Self {
        Self {
            escalator,
            repo: repo.into(),
        }
    }

    fn escalation(&self, task_id: &str, violation: &ProtectedPathViolation) -> Escalation {
        Escalation::new(
            Severity::Blocked,
            EscalationSource::Auditor,
            &self.repo,
            format!(
                "A task changed protected path `{}` (rule `{}`); the change was refused.",
                violation.path, violation.rule
            ),
        )
        .with_task(TaskId(task_id.to_string()))
        .with_evidence(vec![violation.to_string()])
        .with_suggested_action(
            "Review why the task touched Nanna's own configuration; make the change by hand through a human-authored pull request if it is wanted.",
        )
    }
}

impl AuditHook for ProtectedPathEscalation {
    fn on_protected_path_violation(&self, task_id: &str, violation: &ProtectedPathViolation) {
        let escalation = self.escalation(task_id, violation);
        let escalator = Arc::clone(&self.escalator);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    if let Err(error) = escalator.escalate(escalation).await {
                        tracing::error!(%error, "protected-path escalation could not be recorded; the task stays failed");
                    }
                });
            }
            Err(_) => {
                tracing::error!(task = task_id, path = %violation.path, "protected-path violation could not be escalated: no async runtime");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::escalation::{escalator, CapturingSink};

    fn violation() -> ProtectedPathViolation {
        ProtectedPathViolation {
            path: ".nanna/agents/x.toml".to_string(),
            rule: ".nanna/**".to_string(),
        }
    }

    #[test]
    fn a_violation_outside_an_async_runtime_is_logged_and_escalates_nothing() {
        let sink = CapturingSink::new(false);
        let (escalator, log) = escalator(&sink);
        ProtectedPathEscalation::new(escalator, "example/repo")
            .on_protected_path_violation("task-1", &violation());
        assert!(sink.escalations().is_empty());
        assert!(log.states().is_empty());
    }

    #[tokio::test]
    async fn a_violation_becomes_a_blocked_auditor_escalation_for_the_task() {
        let sink = CapturingSink::new(false);
        let (escalator, log) = escalator(&sink);
        ProtectedPathEscalation::new(escalator, "example/repo")
            .on_protected_path_violation("task-1", &violation());
        for _ in 0..200 {
            if !sink.escalations().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let seen = sink.escalations();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].severity, Severity::Blocked);
        assert_eq!(seen[0].source, EscalationSource::Auditor);
        assert_eq!(seen[0].repo, "example/repo");
        assert_eq!(seen[0].task_id, Some(TaskId("task-1".to_string())));
        assert!(seen[0].summary.contains(".nanna/agents/x.toml"));
        assert!(seen[0].summary.contains(".nanna/**"));
        assert_eq!(seen[0].evidence, vec![violation().to_string()]);
        assert_eq!(log.states().len(), 1);
    }

    #[tokio::test]
    async fn an_unwritable_escalation_log_does_not_panic_the_task() {
        let sink = CapturingSink::new(false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("escalations.jsonl");
        let log = Arc::new(crate::escalation::EscalationLog::open(&path).unwrap());
        std::fs::remove_file(&path).unwrap();
        let escalator = Arc::new(Escalator::new(
            Arc::clone(&log),
            Arc::new(Arc::clone(&sink)),
            Arc::new(crate::leases::SystemClock),
            chrono::Duration::minutes(10),
        ));
        ProtectedPathEscalation::new(escalator, "example/repo")
            .on_protected_path_violation("task-1", &violation());
        for _ in 0..200 {
            if !sink.escalations().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(sink.escalations().len(), 1);
        assert!(log.states().is_empty());
    }
}
