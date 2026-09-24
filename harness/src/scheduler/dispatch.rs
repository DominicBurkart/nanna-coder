use super::{
    QueueMetrics, QueueStore, QueueStoreError, QueuedTask, SchedulingPolicy, Side, SlotState,
    TaskQueue,
};
use crate::task::TaskId;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;

/// Boxed future run for a dispatched task.
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Turns a queued entry into the work that occupies its slot.
///
/// The dispatcher spawns the returned future and releases the slot when it
/// resolves or is aborted, so the launcher only has to run the task and
/// record its outcome.
pub trait Launcher: Send + Sync + 'static {
    /// Build the future that runs `task`. `side` is the cursor that selected
    /// it, for logging.
    fn launch(&self, task: &QueuedTask, side: Side) -> BoxFuture;
}

/// What [`Dispatcher::cancel`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    /// The entry was still queued and has been removed.
    Queued(Box<QueuedTask>),
    /// The task was running; it has been aborted and its slot released.
    Running,
    /// Neither queued nor running.
    NotFound,
}

struct RunningSlot {
    side: Side,
    repo: PathBuf,
    abort: AbortHandle,
}

struct State {
    queue: TaskQueue,
    slots: SlotState,
    running: HashMap<TaskId, RunningSlot>,
    wake: Option<(DateTime<Utc>, AbortHandle)>,
}

/// Owns the queue, the slots and the policy, and starts work as slots free.
///
/// Every state change that can free a slot (a launched future resolving, a
/// cancellation) runs the policy again, and parked entries arm a timer so
/// they are considered as soon as their `not_before` passes.
///
/// ```
/// use harness::scheduler::{
///     BoxFuture, Dispatcher, HybridPolicy, InMemoryQueueStore, Launcher, QueuedTask, Side,
/// };
/// use std::path::PathBuf;
///
/// struct Instant;
/// impl Launcher for Instant {
///     fn launch(&self, _task: &QueuedTask, _side: Side) -> BoxFuture {
///         Box::pin(async {})
///     }
/// }
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let dispatcher = Dispatcher::open(
///     Instant,
///     Box::new(HybridPolicy::default()),
///     Box::new(InMemoryQueueStore::default()),
///     1,
/// )
/// .unwrap();
/// let task = QueuedTask::new("t", PathBuf::from("/r"), "HEAD", "m", 1);
/// dispatcher.enqueue(task).await.unwrap();
/// # });
/// ```
pub struct Dispatcher<L: Launcher> {
    launcher: L,
    policy: Box<dyn SchedulingPolicy>,
    store: Box<dyn QueueStore>,
    state: Mutex<State>,
    dispatched_newest: AtomicU64,
    dispatched_oldest: AtomicU64,
}

impl<L: Launcher> Dispatcher<L> {
    /// Build a dispatcher whose queue is reloaded from `store`. Nothing is
    /// dispatched until [`dispatch`](Self::dispatch) or
    /// [`enqueue`](Self::enqueue) is called.
    pub fn open(
        launcher: L,
        policy: Box<dyn SchedulingPolicy>,
        store: Box<dyn QueueStore>,
        max_concurrent: usize,
    ) -> Result<Arc<Self>, QueueStoreError> {
        let queue = TaskQueue::from_entries(store.load()?);
        Ok(Arc::new(Self {
            launcher,
            policy,
            store,
            state: Mutex::new(State {
                queue,
                slots: SlotState::new(max_concurrent),
                running: HashMap::new(),
                wake: None,
            }),
            dispatched_newest: AtomicU64::new(0),
            dispatched_oldest: AtomicU64::new(0),
        }))
    }

    /// Snapshot of the queued entries, oldest first.
    pub async fn queued(&self) -> Vec<QueuedTask> {
        self.state.lock().await.queue.entries().to_vec()
    }

    /// Whether `id` currently occupies a slot.
    pub async fn is_running(&self, id: &TaskId) -> bool {
        self.state.lock().await.running.contains_key(id)
    }

    /// Whether `id` is waiting in the queue.
    pub async fn is_queued(&self, id: &TaskId) -> bool {
        self.state.lock().await.queue.contains(id)
    }

    /// Current backlog metrics.
    pub async fn metrics(&self, now: DateTime<Utc>) -> QueueMetrics {
        let state = self.state.lock().await;
        QueueMetrics::from_queue(
            &state.queue,
            state.slots.running(),
            self.dispatched_newest.load(Ordering::Relaxed),
            self.dispatched_oldest.load(Ordering::Relaxed),
            now,
        )
    }

    /// Persist and queue `task`, then dispatch whatever may start. Returns
    /// the stored entry with its sequence number assigned.
    pub async fn enqueue(
        self: &Arc<Self>,
        task: QueuedTask,
    ) -> Result<QueuedTask, QueueStoreError> {
        let stored = {
            let mut state = self.state.lock().await;
            let stored = state.queue.push(task);
            if let Err(e) = self.store.insert(&stored) {
                state.queue.remove(&stored.id);
                return Err(e);
            }
            stored
        };
        self.dispatch().await;
        Ok(stored)
    }

    /// Run the policy until no further entry may start, and arm the wake
    /// timer for the earliest parked entry.
    pub fn dispatch(self: &Arc<Self>) -> BoxFuture {
        let this = Arc::clone(self);
        Box::pin(async move { this.dispatch_now().await })
    }

    async fn dispatch_now(self: &Arc<Self>) {
        let now = Utc::now();
        let mut state = self.state.lock().await;
        while let Some(selection) = self.policy.next(state.queue.entries(), &state.slots, now) {
            let task = state.queue.remove_at(selection.index);
            let side = selection.side;
            let future = self.launcher.launch(&task, side);
            let this = Arc::clone(self);
            let id = task.id.clone();
            let handle = tokio::spawn(async move {
                future.await;
                this.release(&id).await;
            });
            state.slots.occupy(side, &task.repo_path);
            state.running.insert(
                task.id.clone(),
                RunningSlot {
                    side,
                    repo: task.repo_path.clone(),
                    abort: handle.abort_handle(),
                },
            );
            match side {
                Side::Newest => self.dispatched_newest.fetch_add(1, Ordering::Relaxed),
                Side::Oldest => self.dispatched_oldest.fetch_add(1, Ordering::Relaxed),
            };
            tracing::info!(task_id = %task.id, side = side.label(), "Dispatched queued task");
        }
        if let Some(wake_at) = state.queue.next_wake(now) {
            let sooner = state.wake.as_ref().is_none_or(|(at, _)| wake_at < *at);
            if sooner {
                if let Some((_, previous)) = state.wake.take() {
                    previous.abort();
                }
                let this = Arc::clone(self);
                let delay = (wake_at - now).to_std().unwrap_or_default();
                let handle = tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    this.state.lock().await.wake = None;
                    this.dispatch().await;
                });
                state.wake = Some((wake_at, handle.abort_handle()));
            }
        }
    }

    /// Free the slot held by `id` (if any), forget it in the store, and
    /// dispatch the next entry.
    pub async fn release(self: &Arc<Self>, id: &TaskId) {
        let freed = {
            let mut state = self.state.lock().await;
            match state.running.remove(id) {
                Some(slot) => {
                    state.slots.release(slot.side, &slot.repo);
                    true
                }
                None => false,
            }
        };
        if !freed {
            return;
        }
        if let Err(e) = self.store.remove(id) {
            tracing::error!(task_id = %id, error = %e, "Failed to remove finished task from queue store");
        }
        self.dispatch().await;
    }

    /// Remove `id` from the queue, or abort it if running.
    pub async fn cancel(self: &Arc<Self>, id: &TaskId) -> Result<CancelOutcome, QueueStoreError> {
        let outcome = {
            let mut state = self.state.lock().await;
            if let Some(task) = state.queue.remove(id) {
                CancelOutcome::Queued(Box::new(task))
            } else if let Some(slot) = state.running.remove(id) {
                slot.abort.abort();
                state.slots.release(slot.side, &slot.repo);
                CancelOutcome::Running
            } else {
                return Ok(CancelOutcome::NotFound);
            }
        };
        self.store.remove(id)?;
        if outcome == CancelOutcome::Running {
            self.dispatch().await;
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::{HybridPolicy, InMemoryQueueStore};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[derive(Default)]
    struct ManualLauncher {
        started: StdMutex<Vec<(TaskId, Side)>>,
        finishers: StdMutex<HashMap<TaskId, oneshot::Sender<()>>>,
    }

    impl ManualLauncher {
        fn started(&self) -> Vec<(TaskId, Side)> {
            self.started.lock().unwrap().clone()
        }

        fn finish(&self, id: &TaskId) {
            let tx = self.finishers.lock().unwrap().remove(id).unwrap();
            tx.send(()).unwrap();
        }
    }

    impl Launcher for Arc<ManualLauncher> {
        fn launch(&self, task: &QueuedTask, side: Side) -> BoxFuture {
            let (tx, rx) = oneshot::channel();
            self.finishers.lock().unwrap().insert(task.id.clone(), tx);
            self.started.lock().unwrap().push((task.id.clone(), side));
            Box::pin(async move {
                let _ = rx.await;
            })
        }
    }

    struct FailingStore;

    impl QueueStore for FailingStore {
        fn load(&self) -> Result<Vec<QueuedTask>, QueueStoreError> {
            Ok(vec![])
        }
        fn insert(&self, _task: &QueuedTask) -> Result<(), QueueStoreError> {
            Err(QueueStoreError::Rejected("insert".to_string()))
        }
        fn remove(&self, _id: &TaskId) -> Result<(), QueueStoreError> {
            Err(QueueStoreError::Rejected("remove".to_string()))
        }
    }

    struct RemoveFailsStore(InMemoryQueueStore);

    impl QueueStore for RemoveFailsStore {
        fn load(&self) -> Result<Vec<QueuedTask>, QueueStoreError> {
            self.0.load()
        }
        fn insert(&self, task: &QueuedTask) -> Result<(), QueueStoreError> {
            self.0.insert(task)
        }
        fn remove(&self, _id: &TaskId) -> Result<(), QueueStoreError> {
            Err(QueueStoreError::Rejected("remove".to_string()))
        }
    }

    fn task(repo: &str) -> QueuedTask {
        QueuedTask::new("t", PathBuf::from(repo), "HEAD", "m", 1)
    }

    fn dispatcher(
        max_concurrent: usize,
        store: Box<dyn QueueStore>,
    ) -> (Arc<Dispatcher<Arc<ManualLauncher>>>, Arc<ManualLauncher>) {
        let launcher = Arc::new(ManualLauncher::default());
        let dispatcher = Dispatcher::open(
            Arc::clone(&launcher),
            Box::new(HybridPolicy::default()),
            store,
            max_concurrent,
        )
        .unwrap();
        (dispatcher, launcher)
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..200 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not met within 2s");
    }

    async fn wait_for_started(launcher: &ManualLauncher, count: usize) {
        wait_until(|| launcher.started().len() == count).await;
    }

    #[tokio::test]
    async fn queues_beyond_capacity_and_refills_same_side() {
        let store = InMemoryQueueStore::default();
        let (dispatcher, launcher) = dispatcher(2, Box::new(store.clone()));
        let mut ids = Vec::new();
        for _ in 0..4 {
            ids.push(dispatcher.enqueue(task("/r")).await.unwrap().id);
        }
        let started = launcher.started();
        assert_eq!(started.len(), 2);
        assert_eq!(started[0], (ids[0].clone(), Side::Oldest));
        assert_eq!(started[1], (ids[1].clone(), Side::Newest));
        assert_eq!(dispatcher.queued().await.len(), 2);
        assert!(dispatcher.is_running(&ids[0]).await);
        assert!(dispatcher.is_queued(&ids[2]).await);
        assert_eq!(store.load().unwrap().len(), 4);

        launcher.finish(&ids[1]);
        wait_for_started(&launcher, 3).await;
        assert_eq!(launcher.started()[2], (ids[3].clone(), Side::Newest));
        wait_until(|| store.load().unwrap().len() == 3).await;

        launcher.finish(&ids[0]);
        wait_for_started(&launcher, 4).await;
        assert_eq!(launcher.started()[3], (ids[2].clone(), Side::Oldest));

        let metrics = dispatcher.metrics(Utc::now()).await;
        assert_eq!(metrics.queued, 0);
        assert_eq!(metrics.running, 2);
        assert_eq!(metrics.dispatched_newest, 2);
        assert_eq!(metrics.dispatched_oldest, 2);
    }

    #[tokio::test]
    async fn parked_entries_do_not_hold_slots_and_wake_later() {
        let (dispatcher, launcher) = dispatcher(1, Box::new(InMemoryQueueStore::default()));
        let far = Utc::now() + chrono::Duration::hours(1);
        let soon = Utc::now() + chrono::Duration::milliseconds(80);
        let parked_far = dispatcher
            .enqueue(task("/r").with_not_before(Some(far)))
            .await
            .unwrap();
        let parked_soon = dispatcher
            .enqueue(task("/r").with_not_before(Some(soon)))
            .await
            .unwrap();
        let ready = dispatcher.enqueue(task("/r")).await.unwrap();
        assert_eq!(launcher.started(), vec![(ready.id.clone(), Side::Newest)]);
        let metrics = dispatcher.metrics(Utc::now()).await;
        assert_eq!(metrics.queued, 2);
        assert_eq!(metrics.parked, 2);
        assert_eq!(metrics.running, 1);

        launcher.finish(&ready.id);
        wait_for_started(&launcher, 2).await;
        assert_eq!(launcher.started()[1].0, parked_soon.id);
        assert!(dispatcher.is_queued(&parked_far.id).await);
        assert!(dispatcher.state.lock().await.wake.is_some());
    }

    #[tokio::test]
    async fn wake_fires_when_slot_is_already_free() {
        let (dispatcher, launcher) = dispatcher(1, Box::new(InMemoryQueueStore::default()));
        let soon = Utc::now() + chrono::Duration::milliseconds(50);
        let parked = dispatcher
            .enqueue(task("/r").with_not_before(Some(soon)))
            .await
            .unwrap();
        assert!(launcher.started().is_empty());
        wait_for_started(&launcher, 1).await;
        assert_eq!(launcher.started()[0].0, parked.id);
        assert!(dispatcher.state.lock().await.wake.is_none());
    }

    #[tokio::test]
    async fn cancel_queued_running_and_unknown() {
        let store = InMemoryQueueStore::default();
        let (dispatcher, launcher) = dispatcher(1, Box::new(store.clone()));
        let running = dispatcher.enqueue(task("/r")).await.unwrap();
        let queued = dispatcher.enqueue(task("/r")).await.unwrap();
        let next = dispatcher.enqueue(task("/r")).await.unwrap();

        assert_eq!(
            dispatcher.cancel(&queued.id).await.unwrap(),
            CancelOutcome::Queued(Box::new(queued.clone()))
        );
        assert_eq!(store.load().unwrap().len(), 2);
        assert_eq!(
            dispatcher.cancel(&queued.id).await.unwrap(),
            CancelOutcome::NotFound
        );

        assert_eq!(
            dispatcher.cancel(&running.id).await.unwrap(),
            CancelOutcome::Running
        );
        assert!(!dispatcher.is_running(&running.id).await);
        assert_eq!(launcher.started()[1].0, next.id);
        assert_eq!(store.load().unwrap().len(), 1);
        launcher.finish(&next.id);
        wait_until(|| store.load().unwrap().is_empty()).await;
    }

    #[tokio::test]
    async fn store_survives_rebuild() {
        let store = InMemoryQueueStore::default();
        let (first, _launcher) = dispatcher(0, Box::new(store.clone()));
        let a = first.enqueue(task("/a")).await.unwrap();
        let b = first.enqueue(task("/b")).await.unwrap();
        drop(first);
        let (second, launcher) = dispatcher(2, Box::new(store.clone()));
        assert_eq!(second.queued().await, vec![a.clone(), b.clone()]);
        assert!(launcher.started().is_empty());
        second.dispatch().await;
        assert_eq!(launcher.started().len(), 2);
        assert_eq!(second.queued().await.len(), 0);
    }

    #[tokio::test]
    async fn open_propagates_store_load_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("queue.jsonl");
        std::fs::write(&path, "garbage\n").unwrap();
        let store = crate::scheduler::JsonlQueueStore::open(&path).unwrap();
        let result = Dispatcher::open(
            Arc::new(ManualLauncher::default()),
            Box::new(HybridPolicy::default()),
            Box::new(store),
            1,
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn enqueue_failure_leaves_queue_unchanged() {
        let (dispatcher, launcher) = dispatcher(1, Box::new(FailingStore));
        let err = dispatcher.enqueue(task("/r")).await.unwrap_err();
        assert!(matches!(err, QueueStoreError::Rejected(_)));
        assert!(dispatcher.queued().await.is_empty());
        assert!(launcher.started().is_empty());
        assert_eq!(
            dispatcher.cancel(&TaskId("x".to_string())).await.unwrap(),
            CancelOutcome::NotFound
        );
    }

    #[tokio::test]
    async fn release_logs_store_failures_and_still_dispatches() {
        let (dispatcher, launcher) =
            dispatcher(1, Box::new(RemoveFailsStore(InMemoryQueueStore::default())));
        let first = dispatcher.enqueue(task("/r")).await.unwrap();
        let second = dispatcher.enqueue(task("/r")).await.unwrap();
        launcher.finish(&first.id);
        wait_for_started(&launcher, 2).await;
        assert_eq!(launcher.started()[1].0, second.id);
        assert!(dispatcher.cancel(&second.id).await.is_err());
        dispatcher.release(&second.id).await;
        assert!(!dispatcher.is_running(&second.id).await);
    }
}
