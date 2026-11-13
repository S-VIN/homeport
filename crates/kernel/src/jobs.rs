//! Monotonic timers and cooperative workers. Admission, cancellation, commit
//! and timer generation checks share a lock. No user code runs under it.
use crate::changes::Changes;
use crate::{
    board::{Board, Phase, Run, RunId, Schedule},
    Error, Result,
};
use futures_util::FutureExt;
use std::{
    collections::BTreeMap,
    future::Future,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    runtime::Handle,
    sync::watch,
    time::{sleep_until, Instant},
};
use tokio_util::sync::CancellationToken;

type Work = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
#[derive(Clone)]
pub struct Task {
    key: String,
    label: String,
    quiet: bool,
    body: Arc<dyn Fn(JobContext) -> Work + Send + Sync>,
}
impl Task {
    /// Equal keys identify equivalent jobs. Concurrent manual starts join.
    pub fn new<F, Fut>(key: impl Into<String>, f: F) -> Self
    where
        F: Fn(JobContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let key = key.into();
        Self {
            label: key.clone(),
            quiet: false,
            key,
            body: Arc::new(move |cx| Box::pin(f(cx))),
        }
    }
    /// Hide successful runs from Activity (frequent background polls).
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }
    /// Text shown to the user instead of the key.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}
#[derive(Clone, Copy)]
pub struct Timing {
    pub after: Duration,
    pub every: Option<Duration>,
}
impl Timing {
    pub fn once(after: Duration) -> Self {
        Self { after, every: None }
    }
    pub fn repeating(after: Duration, every: Duration) -> Self {
        Self {
            after,
            every: Some(every),
        }
    }
    pub fn at(time: SystemTime) -> Self {
        Self::once(time.duration_since(SystemTime::now()).unwrap_or_default())
    }
    fn deadline(self) -> Result<Instant> {
        const MAX: Duration = Duration::from_secs(10 * 366 * 24 * 3600);
        if self.after > MAX
            || self
                .every
                .is_some_and(|d| d < Duration::from_millis(1) || d > MAX)
        {
            return Err(Error::new(
                "invalid_timing",
                "Delay must be within ten years and period at least 1 ms",
            ));
        }
        Ok(Instant::now() + self.after)
    }
}
#[derive(Clone)]
pub struct Jobs(Arc<Core>);
struct Core {
    executor: Handle,
    state: Mutex<State>,
    changes: Changes,
}
#[derive(Default)]
struct State {
    closed: bool,
    next_id: u64,
    runs: BTreeMap<RunId, Slot>,
    plans: BTreeMap<String, Plan>,
}
struct Slot {
    run: Run,
    /// Dropped when the run ends so finished runs do not keep their closures alive.
    task: Option<Task>,
    stop: CancellationToken,
    finished: watch::Sender<Option<Run>>,
}
struct Plan {
    task: Task,
    timing: Timing,
    due: Instant,
    generation: u64,
    stop: CancellationToken,
}
#[derive(Clone)]
pub struct JobHandle {
    pub id: RunId,
    finished: watch::Receiver<Option<Run>>,
}
impl JobHandle {
    pub async fn wait(mut self) -> Run {
        self.finished
            .wait_for(Option::is_some)
            .await
            .expect("worker owns sender until completion")
            .as_ref()
            .unwrap()
            .clone()
    }
}
#[derive(Clone)]
pub struct JobContext {
    pub(crate) jobs: Jobs,
    id: RunId,
    stop: CancellationToken,
}
impl JobContext {
    pub fn id(&self) -> RunId {
        self.id
    }
    pub async fn cancelled(&self) {
        self.stop.cancelled().await
    }
    pub fn checkpoint(&self) -> Result<()> {
        if self.stop.is_cancelled() {
            Err(cancelled())
        } else {
            Ok(())
        }
    }
    pub fn progress(&self, done: u64, total: Option<u64>) {
        let mut s = self.jobs.lock();
        if let Some(slot) = s
            .runs
            .get_mut(&self.id)
            .filter(|slot| slot.run.phase.active())
        {
            slot.run.done = done;
            slot.run.total = total;
            self.jobs.changed();
        }
    }
    /// The result in a few words, shown in Activity ("82 checked · 3 failed").
    pub fn summary(&self, text: impl Into<String>) {
        self.set(|run| run.summary = Some(text.into()));
    }
    /// Title of the error toast if this run fails ("Couldn't refresh example.com").
    pub fn failure_title(&self, text: impl Into<String>) {
        self.set(|run| run.failure_title = Some(text.into()));
    }
    fn set(&self, f: impl FnOnce(&mut Run)) {
        let mut s = self.jobs.lock();
        if let Some(slot) = s.runs.get_mut(&self.id) {
            f(&mut slot.run);
            self.jobs.changed();
        }
    }
    /// The run as the board shows it now.
    pub fn run(&self) -> Option<Run> {
        self.jobs.lock().runs.get(&self.id).map(|slot| slot.run.clone())
    }
    /// Call immediately before the first irreversible operation.
    pub fn begin_commit(&self) -> Result<()> {
        let mut s = self.jobs.lock();
        let slot = s.runs.get_mut(&self.id).ok_or_else(missing)?;
        self.checkpoint()?;
        if !slot.run.phase.active() {
            return Err(busy());
        }
        slot.run.phase = Phase::Committing;
        self.jobs.changed();
        Ok(())
    }
}
fn cancelled() -> Error {
    Error::new("cancelled", "Job cancelled")
}
fn missing() -> Error {
    Error::new("not_found", "Unknown run or schedule")
}
fn busy() -> Error {
    Error::new("conflict", "Job is already committing or finished")
}

impl Jobs {
    pub fn new(executor: Handle) -> Self {
        Self::with_changes(executor, Changes::default())
    }
    pub fn with_changes(executor: Handle, changes: Changes) -> Self {
        Self(Arc::new(Core {
            executor,
            state: Mutex::default(),
            changes,
        }))
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.state.lock().expect("job state lock")
    }
    fn changed(&self) {
        self.0.changes.notify();
    }
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.0.changes.subscribe()
    }
    pub fn board(&self) -> Board {
        let s = self.lock();
        let now = Instant::now();
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Board {
            runs: s.runs.values().rev().map(|v| v.run.clone()).collect(),
            schedules: s
                .plans
                .iter()
                .map(|(name, p)| Schedule {
                    name: name.clone(),
                    key: p.task.key.clone(),
                    label: p.task.label.clone(),
                    quiet: p.task.quiet,
                    next_unix_ms: (wall + p.due.saturating_duration_since(now)).as_millis() as u64,
                    every_ms: p.timing.every.map(|e| e.as_millis() as u64),
                })
                .collect(),
        }
    }
    /// Wait handle of a run that is still tracked.
    pub fn handle(&self, id: RunId) -> Option<JobHandle> {
        let s = self.lock();
        s.runs.get(&id).map(|slot| JobHandle { id, finished: slot.finished.subscribe() })
    }
    pub fn start(&self, task: Task) -> Result<JobHandle> {
        let mut s = self.lock();
        ensure_open(&s)?;
        Ok(self.admit(&mut s, task))
    }
    fn admit(&self, s: &mut State, task: Task) -> JobHandle {
        if let Some(slot) = active(s, &task.key) {
            return JobHandle {
                id: slot.run.id,
                finished: slot.finished.subscribe(),
            };
        }
        s.next_id += 1;
        let id = s.next_id;
        let stop = CancellationToken::new();
        let cx = JobContext {
            jobs: self.clone(),
            id,
            stop: stop.clone(),
        };
        let (finished, rx) = watch::channel(None);
        let run = Run {
            id,
            key: task.key.clone(),
            label: task.label.clone(),
            quiet: task.quiet,
            phase: Phase::Running,
            done: 0,
            total: None,
            error: None,
            started: unix_now(),
            ended: None,
            summary: None,
            failure_title: None,
        };
        s.runs.insert(
            id,
            Slot {
                run,
                task: Some(task.clone()),
                stop,
                finished,
            },
        );
        self.changed();
        let jobs = self.clone();
        self.0.executor.spawn(async move {
            let result = AssertUnwindSafe(async move { (task.body)(cx).await })
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(Error::new("panic", "Job panicked")));
            let mut s = jobs.lock();
            let slot = s.runs.get_mut(&id).expect("active worker registered");
            slot.run.phase = match (slot.run.phase, result) {
                (Phase::Cancelling, _) => Phase::Cancelled,
                (_, Ok(())) => Phase::Succeeded,
                (_, Err(e)) => {
                    slot.run.error = Some(e);
                    Phase::Failed
                }
            };
            slot.run.ended = Some(unix_now());
            slot.finished.send_replace(Some(slot.run.clone()));
            slot.task = None;
            if slot.run.quiet && slot.run.phase == Phase::Succeeded {
                s.runs.remove(&id);
            }
            // Retain the newest 64 terminal entries even when an older run is active.
            if let Some(old) = s
                .runs
                .iter()
                .rev()
                .filter(|(_, v)| !v.run.phase.active())
                .nth(64)
                .map(|(&id, _)| id)
            {
                s.runs.remove(&old);
            }
            jobs.changed();
        });
        JobHandle { id, finished: rx }
    }
    pub fn cancel(&self, id: RunId) -> Result<()> {
        let mut s = self.lock();
        let slot = s.runs.get_mut(&id).ok_or_else(missing)?;
        match slot.run.phase {
            Phase::Running => {
                slot.run.phase = Phase::Cancelling;
                slot.stop.cancel();
                self.changed();
                Ok(())
            }
            Phase::Cancelling | Phase::Cancelled => Ok(()),
            _ => Err(busy()),
        }
    }
    /// Stop this exact run and queue a fresh one. It may start only after cleanup.
    pub fn postpone(&self, id: RunId, delay: Duration) -> Result<String> {
        let timing = Timing::once(delay);
        let due = timing.deadline()?;
        let mut s = self.lock();
        ensure_open(&s)?;
        let slot = s.runs.get_mut(&id).ok_or_else(missing)?;
        let name = format!("@defer/{id}");
        if !matches!(slot.run.phase, Phase::Running | Phase::Cancelling) {
            return Err(busy());
        }
        slot.run.phase = Phase::Cancelling;
        slot.stop.cancel();
        let task = slot.task.clone().ok_or_else(busy)?;
        self.set_plan(&mut s, name.clone(), task, timing, due);
        Ok(name)
    }
    pub fn schedule(&self, name: impl Into<String>, task: Task, timing: Timing) -> Result<()> {
        let name = name.into();
        if name.trim().is_empty() || name.starts_with('@') {
            return Err(Error::new(
                "invalid_name",
                "Empty or reserved schedule name",
            ));
        }
        let due = timing.deadline()?;
        let mut s = self.lock();
        ensure_open(&s)?;
        self.set_plan(&mut s, name, task, timing, due);
        Ok(())
    }
    pub fn reschedule(&self, name: &str, timing: Timing) -> Result<()> {
        let due = timing.deadline()?;
        let mut s = self.lock();
        ensure_open(&s)?;
        let task = s.plans.get(name).ok_or_else(missing)?.task.clone();
        self.set_plan(&mut s, name.into(), task, timing, due);
        Ok(())
    }
    fn set_plan(&self, s: &mut State, name: String, task: Task, timing: Timing, due: Instant) {
        s.next_id += 1;
        let generation = s.next_id;
        let stop = CancellationToken::new();
        if let Some(old) = s.plans.insert(
            name.clone(),
            Plan {
                task,
                timing,
                due,
                generation,
                stop: stop.clone(),
            },
        ) {
            old.stop.cancel()
        }
        self.changed();
        let jobs = self.clone();
        self.0.executor.spawn(async move {
            tokio::select! { biased; _=stop.cancelled()=>{}, _=jobs.timer(name,generation,due)=>{} }
        });
    }
    async fn timer(&self, name: String, generation: u64, mut due: Instant) {
        let mut updates = self.subscribe();
        loop {
            sleep_until(due).await;
            updates.borrow_and_update();
            let wait_for_cleanup = {
                let mut s = self.lock();
                let Some(p) = s.plans.get(&name) else { return };
                if p.generation != generation {
                    return;
                }
                let task = p.task.clone();
                let occupied = active(&s, &task.key).is_some();
                match p.timing.every {
                    None if occupied => true,
                    None => {
                        s.plans.remove(&name);
                        self.admit(&mut s, task);
                        return;
                    }
                    Some(period) => {
                        // Advance beyond now in one step, preserving the original phase.
                        let now = Instant::now();
                        let rem = now.saturating_duration_since(due).as_nanos() % period.as_nanos();
                        due = now + (period - Duration::from_nanos(rem as u64));
                        s.plans.get_mut(&name).unwrap().due = due;
                        if !occupied {
                            self.admit(&mut s, task);
                        } else {
                            self.changed();
                        }
                        false
                    }
                }
            };
            if wait_for_cleanup && updates.changed().await.is_err() {
                return;
            }
        }
    }
    pub fn unschedule(&self, name: &str) -> Result<()> {
        let mut s = self.lock();
        s.plans.remove(name).ok_or_else(missing)?.stop.cancel();
        self.changed();
        Ok(())
    }
    pub async fn shutdown(&self) {
        let workers = {
            let mut s = self.lock();
            s.closed = true;
            for (_, p) in std::mem::take(&mut s.plans) {
                p.stop.cancel()
            }
            let mut workers = Vec::new();
            for slot in s.runs.values_mut().filter(|v| v.run.phase.active()) {
                if slot.run.phase == Phase::Running {
                    slot.run.phase = Phase::Cancelling;
                    slot.stop.cancel()
                }
                workers.push(JobHandle {
                    id: slot.run.id,
                    finished: slot.finished.subscribe(),
                });
            }
            self.changed();
            workers
        };
        for worker in workers {
            worker.wait().await;
        }
    }
}
fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
fn ensure_open(s: &State) -> Result<()> {
    if s.closed {
        Err(Error::new("closed", "Job manager is shutting down"))
    } else {
        Ok(())
    }
}
fn active<'a>(s: &'a State, key: &str) -> Option<&'a Slot> {
    s.runs
        .values()
        .find(|v| v.run.phase.active() && v.run.key == key)
}
