use super::QueuedTask;
use crate::task::TaskId;
use chrono::{DateTime, Duration, Utc};

/// Ordered backlog of [`QueuedTask`]s, oldest first.
///
/// Entries are kept sorted by `(submitted_at, seq)`; [`push`](Self::push)
/// assigns the next sequence number so two submissions with the same
/// timestamp keep their arrival order. The sequence counter continues from
/// the largest restored value so it stays monotonic across restarts.
///
/// ```
/// use harness::scheduler::{QueuedTask, TaskQueue};
/// use std::path::PathBuf;
///
/// let mut queue = TaskQueue::new();
/// let a = queue.push(QueuedTask::new("a", PathBuf::from("/r"), "HEAD", "m", 1));
/// let b = queue.push(QueuedTask::new("b", PathBuf::from("/r"), "HEAD", "m", 1));
/// assert!(b.seq > a.seq);
/// assert_eq!(queue.len(), 2);
/// assert_eq!(queue.remove(&a.id).unwrap().description, "a");
/// assert_eq!(queue.entries()[0].description, "b");
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskQueue {
    entries: Vec<QueuedTask>,
    next_seq: u64,
}

impl TaskQueue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild a queue from persisted entries, preserving their sequence
    /// numbers.
    pub fn from_entries(entries: Vec<QueuedTask>) -> Self {
        let next_seq = entries.iter().map(|t| t.seq + 1).max().unwrap_or(0);
        let mut queue = Self { entries, next_seq };
        queue.entries.sort_by_key(QueuedTask::order_key);
        queue
    }

    /// Insert an entry, assigning its sequence number, and return the stored
    /// copy.
    pub fn push(&mut self, mut task: QueuedTask) -> QueuedTask {
        task.seq = self.next_seq;
        self.next_seq += 1;
        let key = task.order_key();
        let at = self.entries.partition_point(|t| t.order_key() <= key);
        self.entries.insert(at, task.clone());
        task
    }

    /// Remove and return the entry at `index` (oldest first).
    pub fn remove_at(&mut self, index: usize) -> QueuedTask {
        self.entries.remove(index)
    }

    /// Remove and return the entry with `id`, if queued.
    pub fn remove(&mut self, id: &TaskId) -> Option<QueuedTask> {
        let index = self.entries.iter().position(|t| &t.id == id)?;
        Some(self.entries.remove(index))
    }

    /// Whether `id` is queued.
    pub fn contains(&self, id: &TaskId) -> bool {
        self.entries.iter().any(|t| &t.id == id)
    }

    /// Entries in dispatch order, oldest first.
    pub fn entries(&self) -> &[QueuedTask] {
        &self.entries
    }

    /// Number of queued entries, parked ones included.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the queue has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries that are parked past `now`.
    pub fn parked(&self, now: DateTime<Utc>) -> usize {
        self.entries.iter().filter(|t| !t.is_ready(now)).count()
    }

    /// Earliest `not_before` still in the future at `now`.
    pub fn next_wake(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.entries
            .iter()
            .filter_map(|t| t.not_before)
            .filter(|t| *t > now)
            .min()
    }

    /// Age of the oldest entry at `now`, or `None` when empty.
    pub fn oldest_age(&self, now: DateTime<Utc>) -> Option<Duration> {
        self.entries.first().map(|t| now - t.submitted_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    fn task(secs: i64) -> QueuedTask {
        let mut t = QueuedTask::new("t", PathBuf::from("/r"), "HEAD", "m", 1);
        t.submitted_at = at(secs);
        t
    }

    #[test]
    fn push_keeps_submission_order_and_assigns_seq() {
        let mut queue = TaskQueue::new();
        assert!(queue.is_empty());
        let late = queue.push(task(10));
        let early = queue.push(task(5));
        assert_eq!(late.seq, 0);
        assert_eq!(early.seq, 1);
        assert_eq!(queue.entries()[0].id, early.id);
        assert_eq!(queue.entries()[1].id, late.id);
        assert!(queue.contains(&late.id));
        assert!(!queue.contains(&TaskId("nope".to_string())));
    }

    #[test]
    fn equal_timestamps_order_by_sequence() {
        let mut queue = TaskQueue::new();
        let first = queue.push(task(1));
        let second = queue.push(task(1));
        assert_eq!(queue.entries()[0].id, first.id);
        assert_eq!(queue.entries()[1].id, second.id);
    }

    #[test]
    fn remove_by_id_and_index() {
        let mut queue = TaskQueue::new();
        let a = queue.push(task(1));
        let b = queue.push(task(2));
        assert_eq!(queue.remove(&a.id).unwrap().id, a.id);
        assert!(queue.remove(&a.id).is_none());
        assert_eq!(queue.remove_at(0).id, b.id);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn from_entries_sorts_and_continues_sequence() {
        let mut later = task(9);
        later.seq = 4;
        let mut earlier = task(3);
        earlier.seq = 2;
        let mut queue = TaskQueue::from_entries(vec![later.clone(), earlier.clone()]);
        assert_eq!(queue.entries()[0].id, earlier.id);
        let pushed = queue.push(task(1));
        assert_eq!(pushed.seq, 5);
        assert_eq!(TaskQueue::from_entries(vec![]).push(task(0)).seq, 0);
    }

    #[test]
    fn parked_wake_and_age() {
        let mut queue = TaskQueue::new();
        assert_eq!(queue.oldest_age(at(0)), None);
        assert_eq!(queue.next_wake(at(0)), None);
        queue.push(task(0));
        queue.push(task(5).with_not_before(Some(at(100))));
        queue.push(task(6).with_not_before(Some(at(50))));
        queue.push(task(7).with_not_before(Some(at(10))));
        assert_eq!(queue.parked(at(20)), 2);
        assert_eq!(queue.next_wake(at(20)), Some(at(50)));
        assert_eq!(queue.next_wake(at(100)), None);
        assert_eq!(queue.oldest_age(at(30)), Some(Duration::seconds(30)));
    }
}
