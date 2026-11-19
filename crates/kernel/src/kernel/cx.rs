//! [`Cx`]: what a handler, observer or job acts with; [`Input`] and [`Guard`]
//! check that what it read still holds when it writes.
use super::*;
/// Execution context of a handler, observer or job: who acts, plus access to
/// state, other ops, the core and the job's progress/cancellation.
#[derive(Clone)]
pub struct Cx {
    pub(crate) k: Arc<Kernel>,
    pub(crate) plugin: Option<usize>,
    pub(crate) job: Option<JobContext>,
}
impl Cx {
    pub(crate) fn as_plugin(&self, plugin: Option<usize>) -> Cx {
        Cx {
            k: self.k.clone(),
            plugin,
            job: self.job.clone(),
        }
    }
    pub fn plugin_id(&self) -> &'static str {
        self.plugin.map_or("kernel", |p| self.k.reg.plugins[p].id)
    }
    /// The plugin's own folder (created), as `Setup::data_dir`.
    pub fn data_dir(&self) -> std::path::PathBuf {
        let id = self.plugin.map_or("kernel", |p| self.k.reg.plugins[p].id);
        let dir = self.k.reg.data_dir.join("plugins").join(id);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }
    pub fn plugin_name(&self) -> &'static str {
        self.plugin
            .map_or("homeport", |p| self.k.reg.plugins[p].name)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.k.snapshot()
    }
    pub fn get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.k.snapshot().get(s)
    }
    /// True while a window or browser tab shows the UI: work that only feeds
    /// the screen (live readings) can slow down otherwise.
    pub fn ui_open(&self) -> bool {
        (self.k.ui_open)()
    }
    /// The latest value of a live state (see [`Setup::live`]).
    pub fn live<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.k.snapshot().live(s)
    }
    /// Replace a live state this plugin owns; the UI repaints if it changed.
    pub fn set_live<T: PartialEq + Send + Sync + 'static>(&self, s: &State<T>, value: T) {
        let cell = self.live_cell(s);
        let mut current = cell.0.write().unwrap();
        if **current != value {
            *current = Arc::new(value);
            drop(current);
            self.k.repaint();
        }
    }
    /// Change a live state this plugin owns; the UI repaints if it changed.
    pub fn update_live<T: Clone + PartialEq + Send + Sync + 'static, R>(
        &self,
        s: &State<T>,
        f: impl FnOnce(&mut T) -> R,
    ) -> R {
        let cell = self.live_cell(s);
        let mut current = cell.0.write().unwrap();
        let mut value = (**current).clone();
        let r = f(&mut value);
        if value != **current {
            *current = Arc::new(value);
            drop(current);
            self.k.repaint();
        }
        r
    }
    /// Put `item` into a live list while the returned guard lives, e.g. the
    /// servers being checked right now: `let _busy = cx.mark_live(&BUSY, id);`
    pub fn mark_live<T: Clone + PartialEq + Send + Sync + 'static>(
        &self,
        s: &'static State<Vec<T>>,
        item: T,
    ) -> LiveMark {
        self.update_live(s, |list| list.push(item.clone()));
        let cx = self.clone();
        LiveMark(Some(Box::new(move || {
            cx.update_live(s, |list| list.retain(|x| *x != item))
        })))
    }
    fn live_cell<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<crate::state::Live<T>> {
        let snap = self.k.snapshot();
        if let Some(me) = self.plugin {
            assert_eq!(
                snap.reg.owner(s.name),
                Some(me),
                "plugin {} cannot write state {}; call an Op of its owner instead",
                snap.reg.plugins[me].id,
                s.name
            );
        }
        snap.live_cell(s)
    }
    pub fn try_get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Option<Arc<T>> {
        self.k.snapshot().try_get(s)
    }
    /// Atomic change of this plugin's state. Keep network I/O outside.
    pub async fn write<R>(&self, f: impl FnOnce(&mut Write<'_>) -> Result<R>) -> Result<R> {
        self.k.write(self.plugin, f).await
    }
    /// Run an op inline (inherits this job's progress and cancellation).
    pub fn call<O: Op>(
        &self,
        op: O,
    ) -> impl std::future::Future<Output = Result<O::Reply>> + Send + 'static + use<O> {
        let h = self.k.reg.ops.get(&TypeId::of::<O>()).map(|b| &**b);
        op::run(self, h, op)
    }
    /// The job that runs ops made by `make` with this context. With `last_error`
    /// (scheduled jobs), the same error is shown once until a run succeeds.
    pub(crate) fn task_with<O: Op>(
        &self,
        key: String,
        spec: crate::JobSpec,
        last_error: Option<Arc<Mutex<Option<String>>>>,
        make: impl Fn() -> O + Send + Sync + 'static,
    ) -> Task {
        let cx = self.clone();
        let label = spec.label.clone();
        let page = spec.page;
        Task::new(key, move |jcx| {
            let run = jcx.clone();
            let cx = Cx {
                job: Some(jcx),
                ..cx.clone()
            };
            let (label, op, last_error) = (label.clone(), make(), last_error.clone());
            async move {
                let r = caught(cx.call(op)).await.map(drop);
                let repeated = last_error.is_some_and(|last| {
                    let message = r.as_ref().err().map(|e| e.message.clone());
                    std::mem::replace(&mut *last.lock().unwrap(), message.clone())
                        .is_some_and(|m| Some(m) == message)
                });
                if let Err(e) = &r
                    && !e.cancelled()
                    && !cx.k.closed()
                    && !repeated
                {
                    // An error reads as one: "Couldn't …" set by the op, else "<label> failed".
                    let title = run
                        .run()
                        .and_then(|r| r.failure_title)
                        .unwrap_or_else(|| format!("{label} failed"));
                    cx.k.push_notice(cx.plugin, &title, &e.message, true, page, None);
                }
                // What a run someone watches found ("Test servers: 82 checked · 3 failed").
                if let (Ok(()), Some(run)) = (&r, run.run().filter(|r| !r.quiet))
                    && let Some(summary) = run.summary
                {
                    cx.k.log(cx.plugin, log::Level::Info, &format!("{label}: {summary}"));
                }
                r
            }
        })
        .label(spec.label)
        .quiet(spec.quiet)
    }
    /// Start an op as a background job (see [`Op::job`] and [`crate::JobSpec::queue`]).
    pub fn spawn<O: Op>(&self, op: O) -> Result<RunId> {
        let spec = op.job().unwrap_or_else(|| {
            crate::JobSpec::keyed(crate::Id::random().to_string(), short_name::<O>())
        });
        let (key, repeat) = (self.job_key(&op, &spec.key), spec.repeat);
        let cell = Arc::new(Mutex::new(Some(op)));
        let take = cell.clone();
        let task = self.task_with(key.clone(), spec, None, move || {
            take.lock().unwrap().take().expect("op runs once")
        });
        let jobs = &self.k.jobs;
        if repeat == crate::Repeat::Join {
            return Ok(jobs.start(task)?.id);
        }
        // Deciding, and starting a follow-up, under one lock: two runs of a
        // key never overlap.
        let mut queued = self.k.queued.lock().unwrap();
        let waiting = queued
            .get(&key)
            .and_then(|(_, c)| c.downcast_ref::<Arc<Mutex<Option<O>>>>())
            .cloned();
        let active = (jobs.board().runs.into_iter()).find(|r| r.key == key && r.phase.active());
        let Some(active) = active else {
            // The run it waited for just ended: this one starts in its place.
            if let Some(old) = waiting.and_then(|w| w.lock().unwrap().take()) {
                queued.remove(&key);
                let mut op = cell.lock().unwrap();
                *op = Some(old.merge(op.take().expect("not started")));
            }
            return Ok(jobs.start(task)?.id);
        };
        if repeat == crate::Repeat::Latest {
            let _ = jobs.cancel(active.id);
        }
        // One follow-up per key: a newer request merges into it.
        if let Some(waiting) = waiting {
            let newer = cell.lock().unwrap().take().expect("not started");
            let mut waiting = waiting.lock().unwrap();
            *waiting = Some(match waiting.take() {
                Some(old) => old.merge(newer),
                None => newer,
            });
            return Ok(active.id);
        }
        if queued.insert(key.clone(), (task, Box::new(cell))).is_some() {
            return Ok(active.id);
        }
        drop(queued);
        let k = self.k.clone();
        let handle = jobs.handle(active.id);
        // Tracked, so `idle()` also waits for the follow-up.
        self.k.tasks.spawn(async move {
            if let Some(h) = handle {
                // Shutdown cancels the active job later: don't hold it up.
                tokio::select! {
                    _ = h.wait() => {}
                    _ = k.stop.cancelled() => return,
                }
            }
            let mut queued = k.queued.lock().unwrap();
            if let Some((task, _)) = queued.remove(&key).filter(|_| !k.closed()) {
                let _ = k.jobs.start(task);
            }
        });
        Ok(active.id)
    }
    /// Read what an op's result depends on (e.g. a subscription's link) when
    /// it starts; [`Input::check`] inside the final write then refuses to save
    /// a result that an edit or a removal overtook meanwhile.
    pub fn input<K: Clone + PartialEq + Send + Sync + 'static>(
        &self,
        read: impl Fn(&Snapshot) -> Option<K> + Send + Sync + 'static,
    ) -> Input<K> {
        let value = read(&self.snapshot());
        Input {
            read: Arc::new(read),
            value,
        }
    }
    /// The active job that `op` would join, if any.
    pub fn running<O: Op>(&self, op: &O) -> Option<crate::Run> {
        let key = self.job_key(op, &op.job()?.key);
        self.k
            .jobs
            .board()
            .runs
            .into_iter()
            .find(|r| r.key == key && r.phase.active())
    }
    /// `<plugin that handles O>/<key or op type>`: job keys never clash
    /// between plugins, and every caller of one op finds the same job.
    pub(crate) fn job_key<O: Op>(&self, op: &O, key: &str) -> String {
        // The plugin whose handler does the work (wrappers aside), as the
        // claims pick it for this op.
        let owner = self
            .k
            .reg
            .ops
            .get(&TypeId::of::<O>())
            .and_then(|b| b.downcast_ref::<Handlers<O>>())
            .and_then(|h| {
                op::chain(&h.0, &self.snapshot(), op)
                    .into_iter()
                    .find(|h| h.rank != crate::Rank::Specific)
            })
            .map_or("kernel", |h| self.k.reg.plugins[h.plugin].id);
        if key.is_empty() {
            format!("{owner}/{}", short_name::<O>())
        } else {
            format!("{owner}/{key}")
        }
    }
    /// Deliver an event now.
    pub fn emit<E: Clone + Send + Sync + 'static>(&self, event: E) {
        self.k.dispatch(event)
    }
    pub fn cancel(&self, run: RunId) -> Result<()> {
        self.k.jobs.cancel(run)
    }
    /// Error if the current job was cancelled.
    pub fn checkpoint(&self) -> Result<()> {
        self.job.as_ref().map_or(Ok(()), |j| j.checkpoint())
    }
    pub async fn cancelled(&self) {
        match &self.job {
            Some(j) => j.cancelled().await,
            None => std::future::pending().await,
        }
    }
    /// Run `fut` unless the job is cancelled first.
    pub async fn or_cancel<T>(&self, fut: impl std::future::Future<Output = T>) -> Result<T> {
        tokio::select! {
            biased;
            _ = self.cancelled() => Err(Error::cancel("Cancelled")),
            v = fut => Ok(v),
        }
    }
    pub fn progress(&self, done: u64, total: Option<u64>) {
        if let Some(j) = &self.job {
            j.progress(done, total)
        }
    }
    /// The job's result in a few words, shown in Activity ("82 checked · 3 failed").
    pub fn summary(&self, text: impl Into<String>) {
        if let Some(j) = &self.job {
            j.summary(text)
        }
    }
    /// Title of the error toast if the job fails ("Couldn't refresh example.com").
    pub fn failure_title(&self, text: impl Into<String>) {
        if let Some(j) = &self.job {
            j.failure_title(text)
        }
    }
    pub fn core(&self) -> CoreApi {
        CoreApi {
            core: self.k.core.clone(),
            kernel: self.k.clone(),
        }
    }
    /// Default direct HTTP client (no proxy, 30 s timeout).
    pub fn http(&self) -> &reqwest::Client {
        &self.k.http
    }
    /// A toast with the plugin's name, also written to the log.
    pub fn notify(&self, text: &str) {
        self.k.log(self.plugin, log::Level::Info, text);
        self.k.notify(self.plugin_name(), text)
    }
    /// A toast with a button that runs `op()`, e.g. "Undo" or "Turn back on":
    /// `cx.notify_with("Auto select is off.", "Turn back on", || SetAuto(true))`.
    pub fn notify_with<O: Op>(
        &self,
        text: &str,
        label: &'static str,
        op: impl Fn() -> O + Send + Sync + 'static,
    ) {
        let action = NoticeAction {
            label,
            plugin: self.plugin,
            run: Arc::new(move |cx| cx.run(op())),
        };
        self.k.log(self.plugin, log::Level::Info, text);
        self.k.push_notice(
            self.plugin,
            self.plugin_name(),
            text,
            false,
            None,
            Some(action),
        );
    }
}

/// Returned by [`Cx::mark_live`]; dropping it takes the item out again.
#[must_use = "the item leaves the list when this guard drops"]
pub struct LiveMark(Option<Box<dyn FnOnce() + Send>>);
impl Drop for LiveMark {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

/// What an op read when it started (see [`Cx::input`]).
#[derive(Clone)]
pub struct Input<K> {
    read: Arc<dyn Fn(&Snapshot) -> Option<K> + Send + Sync>,
    value: Option<K>,
}
impl<K: PartialEq> Input<K> {
    /// The value at the start; `None` when the object did not exist.
    pub fn value(&self) -> Option<&K> {
        self.value.as_ref()
    }
    /// Still the same in the state this write starts from.
    pub fn current(&self, w: &Write<'_>) -> bool {
        self.value.is_some() && (self.read)(w.base()) == self.value
    }
    /// Inside `cx.write`: a silent cancellation when the input changed or is gone.
    pub fn check(&self, w: &Write<'_>) -> Result<()> {
        if self.current(w) {
            Ok(())
        } else {
            Err(Error::cancel("Changed while this was running."))
        }
    }
}
impl<K: Clone + PartialEq + Send + Sync + 'static> Input<K> {
    /// The check alone, without the value's type: an op carries it into the
    /// write of the plugin that handles it.
    pub fn guard(&self) -> Guard {
        let (read, value) = (self.read.clone(), self.value.clone());
        Guard(Arc::new(move |snap| value.is_some() && read(snap) == value))
    }
}
/// [`Input::check`] without the value's type ([`Input::guard`]).
#[derive(Clone)]
pub struct Guard(Arc<dyn Fn(&Snapshot) -> bool + Send + Sync>);
impl Guard {
    /// Inside `cx.write`: a silent cancellation when the input changed or is gone.
    pub fn check(&self, w: &Write<'_>) -> Result<()> {
        if (self.0)(w.base()) {
            Ok(())
        } else {
            Err(Error::cancel("Changed while this was running."))
        }
    }
}
