//! Backlog queue and hybrid FIFO/LIFO scheduler behind [`crate::task::TaskManager`].
//!
//! Submissions past the concurrency limit are queued rather than rejected.
//! The queue is an ordered structure keyed by submission time plus a
//! monotonic sequence number; a [`SchedulingPolicy`] decides which queued
//! entry takes a freed slot. The default [`HybridPolicy`] splits the slots
//! between a cursor that serves the newest work (fast wins) and one that
//! serves the oldest (the long tail), so neither starves the other.
//!
//! Queued entries can be persisted through a [`QueueStore`] so that a
//! rebuilt manager resumes the same backlog, and can be parked with a
//! `not_before` instant so that work outside a human-availability window
//! waits without occupying a slot.
//!
//! ```
//! use chrono::Utc;
//! use harness::scheduler::{HybridPolicy, QueuedTask, SchedulingPolicy, Side, SlotState, TaskQueue};
//! use std::path::PathBuf;
//!
//! let policy = HybridPolicy::new(0.5).unwrap();
//! let mut queue = TaskQueue::new();
//! for n in 0..3 {
//!     queue.push(QueuedTask::new(format!("task {n}"), PathBuf::from("/repo"), "HEAD", "model", 10));
//! }
//! let mut slots = SlotState::new(2);
//! let now = Utc::now();
//!
//! let first = policy.next(queue.entries(), &slots, now).unwrap();
//! assert_eq!(first.side, Side::Oldest);
//! let taken = queue.remove_at(first.index);
//! slots.occupy(first.side, &taken.repo_path);
//!
//! let second = policy.next(queue.entries(), &slots, now).unwrap();
//! assert_eq!(second.side, Side::Newest);
//! assert_eq!(queue.entries()[second.index].description, "task 2");
//! ```

mod policy;
mod queue;
mod store;

pub use policy::{HybridPolicy, PolicyError, SchedulingPolicy, Selection, Side, SlotState};
pub use queue::TaskQueue;
pub use store::{InMemoryQueueStore, JsonlQueueStore, QueueStore, QueueStoreError};

use crate::task::TaskId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

/// Where a queued task came from when it was ingested from a backlog rather
/// than submitted directly. Used to deduplicate ingestion runs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskOrigin {
    /// GitHub repository in `owner/name` form.
    pub repo: String,
    /// Issue number within that repository.
    pub issue: u64,
}

impl fmt::Display for TaskOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.issue)
    }
}

/// A task waiting for a slot. This is the unit the queue orders, the policy
/// selects and the store persists; the runtime state of a dispatched task
/// lives in [`crate::task::Task`].
///
/// Entries are ordered by `(submitted_at, seq)`. `seq` is assigned by
/// [`TaskQueue::push`] and only breaks ties between identical timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedTask {
    pub id: TaskId,
    pub description: String,
    pub repo_path: PathBuf,
    pub branch: String,
    pub model: String,
    pub max_iterations: usize,
    pub submitted_at: DateTime<Utc>,
    pub seq: u64,
    /// Earliest instant the task may be dispatched. `None` means immediately.
    #[serde(default)]
    pub not_before: Option<DateTime<Utc>>,
    /// Agent identity the task should run under, when known.
    #[serde(default)]
    pub identity_hint: Option<String>,
    /// Backlog origin, when the task was ingested rather than submitted.
    #[serde(default)]
    pub origin: Option<TaskOrigin>,
}

impl QueuedTask {
    /// Build an entry submitted now with a fresh id and no parking, identity
    /// or origin.
    pub fn new(
        description: impl Into<String>,
        repo_path: PathBuf,
        branch: impl Into<String>,
        model: impl Into<String>,
        max_iterations: usize,
    ) -> Self {
        Self {
            id: TaskId::new(),
            description: description.into(),
            repo_path,
            branch: branch.into(),
            model: model.into(),
            max_iterations,
            submitted_at: Utc::now(),
            seq: 0,
            not_before: None,
            identity_hint: None,
            origin: None,
        }
    }

    /// Park the entry until `not_before`.
    pub fn with_not_before(mut self, not_before: Option<DateTime<Utc>>) -> Self {
        self.not_before = not_before;
        self
    }

    /// Attach the identity the task should run under.
    pub fn with_identity_hint(mut self, identity_hint: Option<String>) -> Self {
        self.identity_hint = identity_hint;
        self
    }

    /// Record the backlog origin of the entry.
    pub fn with_origin(mut self, origin: Option<TaskOrigin>) -> Self {
        self.origin = origin;
        self
    }

    /// Whether the entry may be dispatched at `now`.
    pub fn is_ready(&self, now: DateTime<Utc>) -> bool {
        self.not_before.is_none_or(|t| t <= now)
    }

    fn order_key(&self) -> (DateTime<Utc>, u64) {
        (self.submitted_at, self.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_displays_as_repo_and_issue() {
        let origin = TaskOrigin {
            repo: "owner/name".to_string(),
            issue: 42,
        };
        assert_eq!(origin.to_string(), "owner/name#42");
    }

    #[test]
    fn builders_set_optional_fields() {
        let now = Utc::now();
        let origin = TaskOrigin {
            repo: "o/n".to_string(),
            issue: 1,
        };
        let task = QueuedTask::new("d", PathBuf::from("/r"), "main", "m", 3)
            .with_not_before(Some(now))
            .with_identity_hint(Some("dev".to_string()))
            .with_origin(Some(origin.clone()));
        assert_eq!(task.not_before, Some(now));
        assert_eq!(task.identity_hint.as_deref(), Some("dev"));
        assert_eq!(task.origin, Some(origin));
        assert_eq!(task.max_iterations, 3);
        assert_eq!(task.branch, "main");
        assert_eq!(task.model, "m");
    }

    #[test]
    fn readiness_follows_not_before() {
        let now = Utc::now();
        let ready = QueuedTask::new("d", PathBuf::from("/r"), "HEAD", "m", 1);
        assert!(ready.is_ready(now));
        let parked = ready
            .clone()
            .with_not_before(Some(now + chrono::Duration::seconds(1)));
        assert!(!parked.is_ready(now));
        assert!(parked.is_ready(now + chrono::Duration::seconds(1)));
    }

    #[test]
    fn serde_round_trip_defaults_optional_fields() {
        let json = r#"{"id":"abc","description":"d","repo_path":"/r","branch":"HEAD","model":"m","max_iterations":1,"submitted_at":"2026-01-01T00:00:00Z","seq":7}"#;
        let task: QueuedTask = serde_json::from_str(json).unwrap();
        assert_eq!(task.seq, 7);
        assert!(task.not_before.is_none());
        assert!(task.identity_hint.is_none());
        assert!(task.origin.is_none());
        let back: QueuedTask =
            serde_json::from_str(&serde_json::to_string(&task).unwrap()).unwrap();
        assert_eq!(back, task);
    }
}
