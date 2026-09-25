//! What the planner proposes: a parent task and the spawn it wants to make.

use crate::effects::EffectClass;
use crate::identity::DevLoop;
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
}
