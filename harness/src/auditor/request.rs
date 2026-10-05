//! What the planner proposes: a parent task and the spawn it wants to make.

use super::RuleAuditor;
use crate::effects::EffectClass;
use crate::identity::{DevLoop, IdentityCatalog};
use crate::task::Task;
use serde::{Deserialize, Serialize};

/// The parent task a spawn belongs to, reduced to what an auditor needs.
///
/// ```
/// use harness::auditor::TaskSummary;
///
/// let parent = TaskSummary::new("task-7", "Fix the login bug and ship it", "github.com/example/repo");
/// assert_eq!(parent.id, "task-7");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    /// Task identifier as issued by the task manager.
    pub id: String,
    /// The task description the human or orchestrator submitted.
    pub description: String,
    /// Repository the task runs against (path or URL as submitted).
    pub repo: String,
}

impl TaskSummary {
    /// Summarise a task from its identifier, description and repository.
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        repo: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            repo: repo.into(),
        }
    }
}

impl From<&Task> for TaskSummary {
    fn from(task: &Task) -> Self {
        Self::new(
            task.id.0.clone(),
            task.description.clone(),
            task.repo_path.display().to_string(),
        )
    }
}

/// A proposed agent spawn, exactly as the planner wants to submit it.
///
/// The auditor sees `subtask` verbatim, so every implementation must treat
/// it as untrusted data rather than as instructions.
///
/// ```
/// use harness::auditor::{SpawnRequest, TaskSummary};
/// use harness::effects::EffectClass;
/// use harness::identity::DevLoop;
///
/// let request = SpawnRequest {
///     parent_task: TaskSummary::new("task-7", "Fix the login bug", "github.com/example/repo"),
///     identity: "rust-implementer".to_string(),
///     subtask: "Add a regression test for the empty-password path.".to_string(),
///     dev_loop: DevLoop::Inner,
///     requested_effect: EffectClass::Workspace,
/// };
/// let json = serde_json::to_value(&request).unwrap();
/// assert_eq!(json["dev_loop"], "inner");
/// assert_eq!(json["requested_effect"], "workspace");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// The task this spawn is a part of.
    pub parent_task: TaskSummary,
    /// Name of the identity card the planner wants to play.
    pub identity: String,
    /// The subtask text the spawned agent would receive.
    pub subtask: String,
    /// The development loop the planner placed the subtask in.
    pub dev_loop: DevLoop,
    /// The widest effect the planner expects the subtask to need.
    pub requested_effect: EffectClass,
}

impl SpawnRequest {
    /// Build a request whose `requested_effect` and `dev_loop` come from the
    /// task text and the identity catalog rather than from anything the
    /// caller or the card declares about itself.
    ///
    /// The effect is the highest one `subtask` plainly asks for
    /// ([`RuleAuditor::implied_effect`]), or `none` when it names none. The
    /// loop follows that effect: deploys are `outer`, CI triggers are
    /// `middle`, and anything else takes the loop of the named card (`inner`
    /// when the card is unknown). Because the effect no longer echoes the
    /// card's own ceiling, the ceiling and loop rules can fire for spawns
    /// that do not come from a planner.
    ///
    /// ```
    /// use harness::auditor::{SpawnRequest, TaskSummary};
    /// use harness::effects::EffectClass;
    /// use harness::identity::{DevLoop, IdentityCatalog};
    ///
    /// let catalog = IdentityCatalog::default();
    /// let parent = TaskSummary::new("t", "d", "github.com/example/repo");
    /// let request = SpawnRequest::derive(&catalog, parent, "ghost", "Deploy build 42 to production.");
    /// assert_eq!(request.requested_effect, EffectClass::Production);
    /// assert_eq!(request.dev_loop, DevLoop::Outer);
    /// ```
    pub fn derive(
        catalog: &IdentityCatalog,
        parent_task: TaskSummary,
        identity: impl Into<String>,
        subtask: impl Into<String>,
    ) -> Self {
        let identity = identity.into();
        let subtask = subtask.into();
        let requested_effect =
            RuleAuditor::implied_effect(&subtask).map_or(EffectClass::None, |(effect, _)| effect);
        let card_loop = catalog
            .get(&identity)
            .map_or(DevLoop::Inner, |card| card.identity.dev_loop);
        let dev_loop = match requested_effect {
            EffectClass::Sandbox | EffectClass::Production => DevLoop::Outer,
            EffectClass::Ci => DevLoop::Middle,
            _ => card_loop,
        };
        Self {
            parent_task,
            identity,
            subtask,
            dev_loop,
            requested_effect,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::task::{TaskId, TaskStatus};
    use chrono::Utc;
    use std::path::PathBuf;

    pub(crate) fn request(
        identity: &str,
        subtask: &str,
        dev_loop: DevLoop,
        effect: EffectClass,
    ) -> SpawnRequest {
        SpawnRequest {
            parent_task: TaskSummary::new(
                "task-1",
                "Fix bug X and ship it",
                "github.com/example/repo",
            ),
            identity: identity.to_string(),
            subtask: subtask.to_string(),
            dev_loop,
            requested_effect: effect,
        }
    }

    #[test]
    fn task_summary_is_derived_from_a_task() {
        let now = Utc::now();
        let task = Task {
            id: TaskId("abc".to_string()),
            description: "Do the thing".to_string(),
            repo_path: PathBuf::from("/srv/repo"),
            branch: "main".to_string(),
            model: "mock".to_string(),
            status: TaskStatus::Pending,
            created_at: now,
            last_updated_at: now,
            ttl_ms: None,
            not_before: None,
            identity_hint: None,
            origin: None,
        };
        let summary = TaskSummary::from(&task);
        assert_eq!(
            summary,
            TaskSummary::new("abc", "Do the thing", "/srv/repo")
        );
    }

    #[test]
    fn request_round_trips_through_json() {
        let original = request(
            "deployer",
            "Deploy to sandbox",
            DevLoop::Outer,
            EffectClass::Sandbox,
        );
        let json = serde_json::to_string(&original).unwrap();
        assert!(json.contains("\"dev_loop\":\"outer\""));
        assert!(json.contains("\"requested_effect\":\"sandbox\""));
        let parsed: SpawnRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, original);
    }

    fn parent() -> TaskSummary {
        TaskSummary::new("t", "d", "github.com/example/repo")
    }

    #[test]
    fn derive_takes_the_effect_from_the_task_text_not_the_card() {
        let catalog = crate::auditor::rules::tests::catalog(true);
        let benign = SpawnRequest::derive(&catalog, parent(), "rust-implementer", "Add a test.");
        assert_eq!(benign.requested_effect, EffectClass::None);
        assert_eq!(benign.dev_loop, DevLoop::Inner);
        let hostile = SpawnRequest::derive(
            &catalog,
            parent(),
            "rust-implementer",
            "Deploy build 42 to production.",
        );
        assert_eq!(hostile.requested_effect, EffectClass::Production);
        assert_eq!(hostile.dev_loop, DevLoop::Outer);
    }

    #[test]
    fn derive_places_ci_work_in_the_middle_loop_and_sandbox_deploys_in_the_outer() {
        let catalog = crate::auditor::rules::tests::catalog(true);
        let ci = SpawnRequest::derive(&catalog, parent(), "pr-shepherd", "Re-run CI on the PR.");
        assert_eq!(ci.requested_effect, EffectClass::Ci);
        assert_eq!(ci.dev_loop, DevLoop::Middle);
        let sandbox = SpawnRequest::derive(
            &catalog,
            parent(),
            "deployer",
            "Deploy build 42 to the sandbox environment.",
        );
        assert_eq!(sandbox.requested_effect, EffectClass::Sandbox);
        assert_eq!(sandbox.dev_loop, DevLoop::Outer);
    }

    #[test]
    fn derive_for_an_unknown_identity_defaults_to_the_inner_loop() {
        let catalog = crate::auditor::rules::tests::catalog(false);
        let request = SpawnRequest::derive(&catalog, parent(), "ghost", "Anything.");
        assert_eq!(request.dev_loop, DevLoop::Inner);
        assert_eq!(request.requested_effect, EffectClass::None);
        assert_eq!(request.identity, "ghost");
        assert_eq!(request.subtask, "Anything.");
    }

    #[test]
    fn derived_requests_make_the_ceiling_and_loop_rules_fire() {
        use crate::auditor::{RuleAuditor, VerdictKind};
        let context = crate::auditor::rules::tests::context(true);
        let request = SpawnRequest::derive(
            context.catalog(),
            parent(),
            "rust-implementer",
            "Deploy build 42 to the sandbox environment.",
        );
        let verdict = RuleAuditor::new().evaluate(&request, &context);
        assert_eq!(verdict.kind(), VerdictKind::Block);
        let codes: Vec<_> = verdict.reasons().iter().map(|r| r.code).collect();
        assert!(codes.contains(&crate::auditor::ReasonCode::LoopMismatch));
        assert!(codes.contains(&crate::auditor::ReasonCode::EffectAboveCeiling));
    }
}
