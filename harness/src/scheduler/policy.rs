use super::QueuedTask;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Which cursor of the hybrid deque claimed a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    /// The cursor that takes the most recently submitted eligible entry.
    Newest,
    /// The cursor that takes the least recently submitted eligible entry.
    Oldest,
}

impl Side {
    /// Lower-case label for metrics and logs.
    pub fn label(self) -> &'static str {
        match self {
            Side::Newest => "newest",
            Side::Oldest => "oldest",
        }
    }
}

/// Invalid policy configuration.
#[derive(Debug, Error, PartialEq)]
pub enum PolicyError {
    #[error("newest_share must be a finite value in 0.0..=1.0, got {0}")]
    InvalidShare(f32),
}

/// Occupancy of the concurrency slots, as seen by a policy.
///
/// The dispatcher keeps this in step with the tasks it has started: each
/// running task is charged to the [`Side`] that selected it and to its
/// repository path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotState {
    max_concurrent: usize,
    running_newest: usize,
    running_oldest: usize,
    running_by_repo: HashMap<PathBuf, usize>,
}

impl SlotState {
    /// An empty slot table with `max_concurrent` slots.
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            running_newest: 0,
            running_oldest: 0,
            running_by_repo: HashMap::new(),
        }
    }

    /// Total number of slots.
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    /// Slots currently occupied.
    pub fn running(&self) -> usize {
        self.running_newest + self.running_oldest
    }

    /// Slots currently free.
    pub fn free(&self) -> usize {
        self.max_concurrent.saturating_sub(self.running())
    }

    /// Slots occupied by tasks selected from `side`.
    pub fn running_on(&self, side: Side) -> usize {
        match side {
            Side::Newest => self.running_newest,
            Side::Oldest => self.running_oldest,
        }
    }

    /// Slots occupied by tasks for `repo`.
    pub fn running_for_repo(&self, repo: &Path) -> usize {
        self.running_by_repo.get(repo).copied().unwrap_or(0)
    }

    /// Charge one slot to `side` and `repo`.
    pub fn occupy(&mut self, side: Side, repo: &Path) {
        match side {
            Side::Newest => self.running_newest += 1,
            Side::Oldest => self.running_oldest += 1,
        }
        *self.running_by_repo.entry(repo.to_path_buf()).or_insert(0) += 1;
    }

    /// Return the slot charged by [`occupy`](Self::occupy).
    pub fn release(&mut self, side: Side, repo: &Path) {
        match side {
            Side::Newest => self.running_newest = self.running_newest.saturating_sub(1),
            Side::Oldest => self.running_oldest = self.running_oldest.saturating_sub(1),
        }
        if let Some(count) = self.running_by_repo.get_mut(repo) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.running_by_repo.remove(repo);
            }
        }
    }
}

/// The entry a policy chose and the side that will be charged for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Index into the queue slice passed to [`SchedulingPolicy::next`].
    pub index: usize,
    pub side: Side,
}

/// Chooses which queued entry takes the next free slot.
///
/// `queue` is ordered oldest first, as [`super::TaskQueue::entries`] returns
/// it. The dispatcher calls [`next`](Self::next) repeatedly, removing the
/// selected entry and charging `slots` between calls, until it returns
/// `None`. Implementations must not select an entry whose
/// [`QueuedTask::is_ready`] is false at `now`.
pub trait SchedulingPolicy: Send + Sync {
    /// Select an entry for a free slot, or `None` if nothing may start now.
    fn next(
        &self,
        queue: &[QueuedTask],
        slots: &SlotState,
        now: DateTime<Utc>,
    ) -> Option<Selection>;
}

/// Hybrid FIFO/LIFO policy: `N` slots are split into a newest-first cursor
/// and an oldest-first cursor.
///
/// # Slot split
///
/// `newest_cap = round(N * newest_share)` (half rounds up) and
/// `oldest_cap = N - newest_cap`. A share of `0.0` is pure FIFO and `1.0`
/// pure LIFO. With a single slot and the default share the slot belongs to
/// the newest cursor.
///
/// # Tie-breaking
///
/// - A slot freed by one side is refilled from the same side, because the
///   other side is already at its cap.
/// - When both sides have room (for example on an idle machine) the side
///   with more free slots is served first; equal room goes to the oldest
///   cursor so the long tail is picked up first.
/// - A single eligible entry is both the newest and the oldest; whichever
///   side claims it is charged for it.
/// - Entries with the same submission instant are ordered by their sequence
///   number, so the newest cursor takes the higher sequence.
///
/// # Aging
///
/// There is no priority aging. An entry the newest cursor never reaches
/// (because newer work keeps arriving) is still reached by the oldest cursor,
/// whose share of the slots is fixed, so every entry's wait is bounded by the
/// oldest cursor's throughput. A parked entry keeps its original submission
/// time and rejoins the ordering at that position once it is ready.
///
/// # Per-repository fairness
///
/// With [`with_max_per_repo`](Self::with_max_per_repo) set, entries whose
/// repository already has that many running tasks are skipped by both
/// cursors, so one backlog cannot occupy every slot.
///
/// ```
/// use chrono::Utc;
/// use harness::scheduler::{HybridPolicy, QueuedTask, SchedulingPolicy, Side, SlotState};
/// use std::path::PathBuf;
///
/// let policy = HybridPolicy::new(0.5).unwrap().with_max_per_repo(Some(1));
/// assert_eq!(policy.caps(4), (2, 2));
///
/// let queue: Vec<QueuedTask> = (0..2)
///     .map(|_| QueuedTask::new("t", PathBuf::from("/busy"), "HEAD", "m", 1))
///     .collect();
/// let mut slots = SlotState::new(4);
/// slots.occupy(Side::Oldest, &PathBuf::from("/busy"));
/// assert_eq!(policy.next(&queue, &slots, Utc::now()), None);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct HybridPolicy {
    newest_share: f32,
    max_per_repo: Option<usize>,
}

impl Default for HybridPolicy {
    fn default() -> Self {
        Self {
            newest_share: 0.5,
            max_per_repo: None,
        }
    }
}

impl HybridPolicy {
    /// A policy giving `newest_share` of the slots to the newest cursor.
    pub fn new(newest_share: f32) -> Result<Self, PolicyError> {
        if !newest_share.is_finite() || !(0.0..=1.0).contains(&newest_share) {
            return Err(PolicyError::InvalidShare(newest_share));
        }
        Ok(Self {
            newest_share,
            max_per_repo: None,
        })
    }

    /// Cap the number of running tasks per repository path.
    pub fn with_max_per_repo(mut self, max_per_repo: Option<usize>) -> Self {
        self.max_per_repo = max_per_repo;
        self
    }

    /// The configured newest share.
    pub fn newest_share(&self) -> f32 {
        self.newest_share
    }

    /// The configured per-repository cap.
    pub fn max_per_repo(&self) -> Option<usize> {
        self.max_per_repo
    }

    /// `(newest_cap, oldest_cap)` for `max_concurrent` slots.
    pub fn caps(&self, max_concurrent: usize) -> (usize, usize) {
        let newest = ((max_concurrent as f32) * self.newest_share).round() as usize;
        let newest = newest.min(max_concurrent);
        (newest, max_concurrent - newest)
    }

    fn eligible(&self, task: &QueuedTask, slots: &SlotState, now: DateTime<Utc>) -> bool {
        task.is_ready(now)
            && self
                .max_per_repo
                .is_none_or(|cap| slots.running_for_repo(&task.repo_path) < cap)
    }
}

impl SchedulingPolicy for HybridPolicy {
    fn next(
        &self,
        queue: &[QueuedTask],
        slots: &SlotState,
        now: DateTime<Utc>,
    ) -> Option<Selection> {
        if slots.free() == 0 {
            return None;
        }
        let (newest_cap, oldest_cap) = self.caps(slots.max_concurrent());
        let newest_room = newest_cap.saturating_sub(slots.running_on(Side::Newest));
        let oldest_room = oldest_cap.saturating_sub(slots.running_on(Side::Oldest));
        let side = if newest_room > oldest_room {
            Side::Newest
        } else {
            Side::Oldest
        };
        let mut eligible = queue
            .iter()
            .enumerate()
            .filter(|(_, task)| self.eligible(task, slots, now))
            .map(|(index, _)| index);
        let index = match side {
            Side::Newest => eligible.next_back(),
            Side::Oldest => eligible.next(),
        }?;
        Some(Selection { index, side })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use proptest::prelude::*;

    fn task(repo: &str, offset_secs: i64) -> QueuedTask {
        let mut t = QueuedTask::new("t", PathBuf::from(repo), "HEAD", "m", 1);
        t.submitted_at = DateTime::<Utc>::from_timestamp(1_700_000_000 + offset_secs, 0).unwrap();
        t
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_100_000, 0).unwrap()
    }

    #[test]
    fn share_validation() {
        assert!(HybridPolicy::new(0.0).is_ok());
        assert!(HybridPolicy::new(1.0).is_ok());
        assert_eq!(
            HybridPolicy::new(1.5).unwrap_err(),
            PolicyError::InvalidShare(1.5)
        );
        assert!(HybridPolicy::new(-0.1).is_err());
        assert!(HybridPolicy::new(f32::NAN).is_err());
        assert_eq!(
            HybridPolicy::new(1.5).unwrap_err().to_string(),
            "newest_share must be a finite value in 0.0..=1.0, got 1.5"
        );
    }

    #[test]
    fn caps_round_half_up_and_cover_extremes() {
        let half = HybridPolicy::default();
        assert_eq!(half.newest_share(), 0.5);
        assert_eq!(half.max_per_repo(), None);
        assert_eq!(half.caps(0), (0, 0));
        assert_eq!(half.caps(1), (1, 0));
        assert_eq!(half.caps(3), (2, 1));
        assert_eq!(half.caps(8), (4, 4));
        assert_eq!(HybridPolicy::new(0.0).unwrap().caps(5), (0, 5));
        assert_eq!(HybridPolicy::new(1.0).unwrap().caps(5), (5, 0));
        assert_eq!(HybridPolicy::new(0.25).unwrap().caps(2), (1, 1));
    }

    #[test]
    fn side_labels() {
        assert_eq!(Side::Newest.label(), "newest");
        assert_eq!(Side::Oldest.label(), "oldest");
    }

    #[test]
    fn slot_state_accounting() {
        let repo = PathBuf::from("/r");
        let mut slots = SlotState::new(3);
        assert_eq!(slots.max_concurrent(), 3);
        assert_eq!(slots.free(), 3);
        slots.occupy(Side::Newest, &repo);
        slots.occupy(Side::Oldest, &repo);
        assert_eq!(slots.running(), 2);
        assert_eq!(slots.running_on(Side::Newest), 1);
        assert_eq!(slots.running_on(Side::Oldest), 1);
        assert_eq!(slots.running_for_repo(&repo), 2);
        assert_eq!(slots.running_for_repo(Path::new("/other")), 0);
        slots.release(Side::Newest, &repo);
        assert_eq!(slots.running_for_repo(&repo), 1);
        slots.release(Side::Oldest, &repo);
        assert_eq!(slots.running_for_repo(&repo), 0);
        assert_eq!(slots.free(), 3);
        slots.release(Side::Oldest, &repo);
        slots.release(Side::Newest, Path::new("/never"));
        assert_eq!(slots.running(), 0);
    }

    #[test]
    fn empty_queue_or_full_slots_select_nothing() {
        let policy = HybridPolicy::default();
        assert_eq!(policy.next(&[], &SlotState::new(2), now()), None);
        let queue = vec![task("/r", 0)];
        assert_eq!(policy.next(&queue, &SlotState::new(0), now()), None);
    }

    #[test]
    fn idle_machine_serves_oldest_first_then_newest() {
        let policy = HybridPolicy::default();
        let queue = vec![task("/r", 0), task("/r", 1), task("/r", 2)];
        let mut slots = SlotState::new(2);
        let first = policy.next(&queue, &slots, now()).unwrap();
        assert_eq!(
            first,
            Selection {
                index: 0,
                side: Side::Oldest
            }
        );
        slots.occupy(first.side, &queue[first.index].repo_path);
        let rest = &queue[1..];
        let second = policy.next(rest, &slots, now()).unwrap();
        assert_eq!(
            second,
            Selection {
                index: 1,
                side: Side::Newest
            }
        );
    }

    #[test]
    fn freed_slot_is_refilled_from_the_same_side() {
        let policy = HybridPolicy::default();
        let queue = vec![task("/r", 0), task("/r", 1), task("/r", 2)];
        let mut slots = SlotState::new(2);
        slots.occupy(Side::Oldest, Path::new("/r"));
        assert_eq!(
            policy.next(&queue, &slots, now()).unwrap().side,
            Side::Newest
        );
        let mut slots = SlotState::new(2);
        slots.occupy(Side::Newest, Path::new("/r"));
        assert_eq!(
            policy.next(&queue, &slots, now()).unwrap().side,
            Side::Oldest
        );
    }

    #[test]
    fn single_slot_default_share_belongs_to_newest() {
        let policy = HybridPolicy::default();
        let queue = vec![task("/r", 0), task("/r", 1)];
        let sel = policy.next(&queue, &SlotState::new(1), now()).unwrap();
        assert_eq!(sel.side, Side::Newest);
        assert_eq!(sel.index, 1);
    }

    #[test]
    fn parked_entries_are_skipped_by_both_cursors() {
        let policy = HybridPolicy::default();
        let later = now() + Duration::hours(1);
        let queue = vec![
            task("/r", 0).with_not_before(Some(later)),
            task("/r", 1),
            task("/r", 2).with_not_before(Some(later)),
        ];
        let mut slots = SlotState::new(4);
        let sel = policy.next(&queue, &slots, now()).unwrap();
        assert_eq!(sel.index, 1);
        slots.occupy(sel.side, Path::new("/r"));
        let remaining = vec![queue[0].clone(), queue[2].clone()];
        assert_eq!(policy.next(&remaining, &slots, now()), None);
        assert!(policy.next(&remaining, &slots, later).is_some());
    }

    #[test]
    fn same_timestamp_ties_use_sequence() {
        let policy = HybridPolicy::new(1.0).unwrap();
        let mut a = task("/r", 0);
        a.seq = 1;
        let mut b = task("/r", 0);
        b.seq = 2;
        let queue = vec![a, b];
        let sel = policy.next(&queue, &SlotState::new(1), now()).unwrap();
        assert_eq!(queue[sel.index].seq, 2);
    }

    #[test]
    fn per_repo_cap_skips_busy_repositories() {
        let policy = HybridPolicy::default().with_max_per_repo(Some(1));
        assert_eq!(policy.max_per_repo(), Some(1));
        let queue = vec![task("/busy", 0), task("/idle", 1), task("/busy", 2)];
        let mut slots = SlotState::new(4);
        slots.occupy(Side::Oldest, Path::new("/busy"));
        let sel = policy.next(&queue, &slots, now()).unwrap();
        assert_eq!(sel.index, 1);
        slots.occupy(sel.side, Path::new("/idle"));
        let remaining = vec![queue[0].clone(), queue[2].clone()];
        assert_eq!(policy.next(&remaining, &slots, now()), None);
    }

    #[derive(Debug, Clone)]
    enum Op {
        Submit { repo: u8, parked: bool },
        Complete(usize),
        Advance,
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0u8..3, any::<bool>()).prop_map(|(repo, parked)| Op::Submit { repo, parked }),
            3 => any::<usize>().prop_map(Op::Complete),
            1 => Just(Op::Advance),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn hybrid_policy_respects_caps(
            max_concurrent in 0usize..6,
            share in 0.0f32..=1.0,
            max_per_repo in proptest::option::of(1usize..4),
            ops in proptest::collection::vec(op_strategy(), 1..60),
        ) {
            let policy = HybridPolicy::new(share).unwrap().with_max_per_repo(max_per_repo);
            let (newest_cap, oldest_cap) = policy.caps(max_concurrent);
            let mut queue = super::super::TaskQueue::new();
            let mut slots = SlotState::new(max_concurrent);
            let mut running: Vec<(Side, PathBuf)> = Vec::new();
            let mut clock = now();
            let mut counter = 0i64;
            for op in ops {
                match op {
                    Op::Submit { repo, parked } => {
                        counter += 1;
                        let mut t = task(&format!("/repo{repo}"), counter);
                        if parked {
                            t.not_before = Some(clock + Duration::minutes(1));
                        }
                        queue.push(t);
                    }
                    Op::Complete(i) => {
                        if !running.is_empty() {
                            let (side, repo) = running.remove(i % running.len());
                            slots.release(side, &repo);
                        }
                    }
                    Op::Advance => clock += Duration::minutes(2),
                }
                while let Some(sel) = policy.next(queue.entries(), &slots, clock) {
                    let taken = queue.remove_at(sel.index);
                    prop_assert!(taken.is_ready(clock));
                    slots.occupy(sel.side, &taken.repo_path);
                    running.push((sel.side, taken.repo_path));
                }
                prop_assert!(running.len() <= max_concurrent);
                prop_assert!(slots.running_on(Side::Newest) <= newest_cap + 1);
                prop_assert!(slots.running_on(Side::Oldest) <= oldest_cap + 1);
                prop_assert!(slots.running_on(Side::Newest) <= newest_cap);
                prop_assert!(slots.running_on(Side::Oldest) <= oldest_cap);
                if let Some(cap) = max_per_repo {
                    for (_, repo) in &running {
                        prop_assert!(slots.running_for_repo(repo) <= cap);
                    }
                }
                let ready_waiting = queue.entries().iter().any(|t| t.is_ready(clock)
                    && max_per_repo.is_none_or(|cap| slots.running_for_repo(&t.repo_path) < cap));
                prop_assert!(!(ready_waiting && slots.free() > 0));
            }
        }
    }
}
