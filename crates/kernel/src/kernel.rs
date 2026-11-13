use crate::{
    changes::Changes,
    core::{Core, CoreApi},
    jobs::{JobContext, Jobs, Task, Timing},
    op::{self, short_name, Handlers},
    setup::Registry,
    state::{Cell, OrderRow, PluginRow, SettingRow, Snapshot, Value, Write},
    store::Store,
    Error, Op, Plugin, Policy, Result, RunId, Setup, State,
};
use std::{
    any::{Any, TypeId},
    collections::{HashMap, VecDeque},
    panic::AssertUnwindSafe,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock, Weak,
    },
    time::Duration,
};
use tokio::runtime::Handle;

/// Emitted once after all plugins loaded and the core started.
#[derive(Clone, Debug)]
pub struct Started;

/// A message for the user (toast). `error` ones are shown in red.
#[derive(Clone, Debug)]
pub struct Notice {
    pub id: u64,
    pub title: String,
    pub text: String,
    pub error: bool,
    /// Page with the details; the toast offers to open it.
    pub page: Option<&'static str>,
}

/// Interface theme, persisted by the kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}
pub(crate) static THEME: State<Theme> = State::new("kernel.theme");

pub struct Kernel {
    pub(crate) reg: Arc<Registry>,
    snap: RwLock<Snapshot>,
    writer: tokio::sync::Mutex<()>,
    store: Store,
    pub(crate) jobs: Jobs,
    pub(crate) changes: Changes,
    pub(crate) core: Arc<Core>,
    http: reqwest::Client,
    notices: Mutex<VecDeque<Notice>>,
    next_notice: AtomicU64,
    schedules: Mutex<HashMap<String, Option<Duration>>>,
    services: Mutex<HashMap<String, (Arc<dyn std::any::Any + Send + Sync>, tokio::task::AbortHandle)>>,
    pub(crate) catalog: Vec<&'static Plugin>,
    pub(crate) saved_toggles: Mutex<HashMap<String, bool>>,
    pub(crate) disabled_reasons: Vec<(String, String)>,
    pub(crate) restart: Arc<dyn Fn() + Send + Sync>,
    /// Set by shutdown: no more writes, events or jobs from this kernel.
    closed: std::sync::atomic::AtomicBool,
    /// Job keys with a queued follow-up run.
    queued: Mutex<std::collections::HashSet<String>>,
    pub data_dir: PathBuf,
    me: Weak<Kernel>,
}

pub(crate) fn is_enabled(p: &Plugin, saved: &HashMap<String, bool>) -> bool {
    p.required || saved.get(p.id).copied().unwrap_or(p.default_enabled)
}

/// Enabled plugins in dependency order, plus the reason each skipped plugin is off.
pub(crate) fn resolve(catalog: &[&'static Plugin], saved: &HashMap<String, bool>) -> (Vec<&'static Plugin>, Vec<(String, String)>) {
    let mut policies: HashMap<&str, Policy> = HashMap::new();
    for p in catalog {
        if let Some(old) = policies.insert(p.category.id, p.category.policy) {
            assert_eq!(old, p.category.policy, "category {} declared with two policies", p.category.id);
        }
    }
    let mut on: Vec<&'static Plugin> = catalog.iter().copied().filter(|p| is_enabled(p, saved)).collect();
    let mut why = Vec::new();
    // Exclusive categories: the first enabled plugin wins.
    let mut taken: HashMap<&str, &str> = HashMap::new();
    on.retain(|p| {
        if p.category.policy == Policy::Exclusive {
            if let Some(winner) = taken.get(p.category.id) {
                let winner = catalog.iter().find(|c| c.id == *winner).map_or(*winner, |c| c.name);
                why.push((p.id.to_owned(), format!("{winner} is already on; only one {} plugin can run", p.category.name.to_lowercase())));
                return false;
            }
            taken.insert(p.category.id, p.id);
        }
        true
    });
    loop {
        let before = on.len();
        let ids: Vec<&str> = on.iter().map(|p| p.id).collect();
        on.retain(|p| match p.requires.iter().find(|r| !ids.contains(r)) {
            Some(missing) => {
                let name = catalog.iter().find(|c| c.id == *missing).map_or(*missing, |c| c.name);
                why.push((p.id.to_owned(), format!("Needs {name}, which is off")));
                false
            }
            None => true,
        });
        if on.len() == before {
            break;
        }
    }
    let mut ordered: Vec<&'static Plugin> = Vec::new();
    while ordered.len() < on.len() {
        let next = on
            .iter()
            .find(|p| !ordered.iter().any(|o| o.id == p.id) && p.requires.iter().all(|r| ordered.iter().any(|o| o.id == *r)))
            .copied()
            .expect("plugin dependency cycle");
        ordered.push(next);
    }
    (ordered, why)
}

impl Kernel {
    pub(crate) async fn start(
        catalog: Vec<&'static Plugin>,
        store: Store,
        data_dir: PathBuf,
        restart: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Arc<Kernel>> {
        store.register::<PluginRow>()?;
        store.register::<OrderRow>()?;
        store.register::<SettingRow>()?;
        let saved: HashMap<String, bool> = store.transaction(|tx| tx.all::<PluginRow>())?.into_iter().map(|r| (r.id, r.enabled)).collect();
        let (plugins, disabled_reasons) = resolve(&catalog, &saved);
        let mut reg = Registry { plugins, data_dir: data_dir.clone(), ..Default::default() };
        Setup { reg: &mut reg, plugin: None }.setting(&THEME);
        for i in 0..reg.plugins.len() {
            let p = reg.plugins[i];
            (p.setup)(&mut Setup { reg: &mut reg, plugin: Some(i) })
                .map_err(|e| Error::msg(format!("Plugin {} failed to start: {e}", p.name)))?;
        }
        reg.finish();
        for (p, m) in &reg.migrations {
            store.transaction(|tx| m(tx.sql())).map_err(|e| Error::msg(format!("{}: migration failed: {e}", reg.plugins[*p].name)))?;
        }
        for (_, _, p) in &reg.persist {
            (p.register)(&store)?;
        }
        let mut cells: HashMap<&'static str, Cell> = store.transaction(|tx| {
            reg.persist
                .iter()
                .map(|(name, _, p)| Ok((*name, Cell { value: (p.load)(tx)?, rev: 1 })))
                .collect::<Result<_>>()
        })?;
        for (name, init) in &reg.memory {
            cells.insert(name, Cell { value: init(), rev: 1 });
        }
        let reg = Arc::new(reg);
        let snapshot = Snapshot { cells: Arc::new(cells), reg: reg.clone(), rev: 1 };
        let changes = Changes::default();
        let k = Arc::new_cyclic(|me| Kernel {
            reg,
            snap: RwLock::new(snapshot.clone()),
            writer: tokio::sync::Mutex::new(()),
            store,
            jobs: Jobs::with_changes(Handle::current(), changes.clone()),
            core: Arc::new(Core::new(changes.clone())),
            changes,
            http: reqwest::Client::builder()
                .no_proxy()
                .user_agent("v2rayN/7.0")
                .timeout(Duration::from_secs(30))
                .build()
                .expect("HTTP client"),
            notices: Mutex::default(),
            next_notice: AtomicU64::new(1),
            schedules: Mutex::default(),
            services: Mutex::default(),
            catalog,
            saved_toggles: Mutex::new(saved),
            disabled_reasons,
            restart,
            closed: Default::default(),
            queued: Mutex::default(),
            data_dir,
            me: me.clone(),
        });
        if let Err(e) = k.core.start(&k.reg, &snapshot).await {
            k.notify_error("Core did not start", &e.message);
        }
        k.reschedule();
        let weak = Arc::downgrade(&k);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            loop {
                tick.tick().await;
                let Some(k) = weak.upgrade() else { return };
                if k.closed() {
                    return;
                }
                k.core.monitor().await;
            }
        });
        k.dispatch(Started);
        Ok(k)
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for (_, (_, task)) in self.services.lock().unwrap().drain() {
            task.abort();
        }
        self.jobs.shutdown().await;
        self.core.stop().await;
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snap.read().unwrap().clone()
    }
    pub(crate) fn arc(&self) -> Arc<Kernel> {
        self.me.upgrade().expect("kernel alive")
    }
    /// A context acting as the kernel itself.
    pub fn cx(&self) -> Cx {
        Cx { k: self.arc(), plugin: None, job: None }
    }
    /// A context acting as plugin `id` (tests, glue code).
    pub fn cx_for(&self, id: &str) -> Cx {
        Cx { k: self.arc(), plugin: self.reg.plugin_index(id), job: None }
    }
    pub fn plugins(&self) -> &[&'static Plugin] {
        &self.reg.plugins
    }
    pub fn board(&self) -> crate::board::Board {
        self.jobs.board()
    }
    pub fn changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub fn core_status(&self) -> crate::core::CoreStatus {
        self.core.status()
    }

    /// Restart mihomo and apply the saved configuration.
    pub async fn restart_core(&self) -> Result<()> {
        let _w = self.writer.lock().await;
        self.core.start(&self.reg, &self.snapshot()).await
    }

    /// Re-run the core apply for the current snapshot under the writer lock.
    pub(crate) async fn resync(&self) -> Result<()> {
        let _w = self.writer.lock().await;
        self.core.sync(&self.reg, &self.snapshot()).await
    }

    // ---- transactions ---------------------------------------------------
    pub(crate) async fn write<R>(&self, plugin: Option<usize>, f: impl FnOnce(&mut Write<'_>) -> Result<R>) -> Result<R> {
        let _w = self.writer.lock().await;
        if self.closed() {
            return Err(Error::new("closed", "rproxy is restarting."));
        }
        let base = self.snapshot();
        let mut w = Write::new(&base, plugin);
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| f(&mut w)))
            .map_err(|p| Error::new("bug", panic_text(p)))??;
        for c in &self.reg.cascades {
            c(&base, &mut w);
        }
        let Write { changed, events, .. } = w;
        if changed.is_empty() {
            drop(_w);
            for e in events {
                e(&self.arc());
            }
            return Ok(out);
        }
        let names: Vec<&'static str> = changed.keys().copied().collect();
        let changed: HashMap<&'static str, Value> = changed.into_iter().map(|(k, v)| (k, Arc::from(v) as Value)).collect();
        let next = base.with(changed);
        if let Err(e) = self.core.sync(&self.reg, &next).await {
            // Put the previous configuration back (the core may be half-updated).
            if let Err(r) = self.core.sync(&self.reg, &base).await {
                self.notify_error("Core configuration could not be restored", &r.message);
            }
            return Err(e);
        }
        let saved = self.store.transaction(|tx| {
            for (name, _, p) in &self.reg.persist {
                if names.contains(name) {
                    (p.save)(tx, &base.cells[name].value, &next.cells[name].value)?;
                }
            }
            Ok(())
        });
        if let Err(e) = saved {
            if let Err(r) = self.core.sync(&self.reg, &base).await {
                self.notify_error("Core configuration could not be restored", &r.message);
            }
            return Err(e);
        }
        *self.snap.write().unwrap() = next;
        self.changes.notify();
        drop(_w);
        let k = self.arc();
        for e in events {
            e(&k);
        }
        self.dispatch(crate::Changed { states: Arc::new(names) });
        self.reschedule();
        Ok(out)
    }

    pub(crate) fn dispatch<E: Clone + Send + Sync + 'static>(&self, event: E) {
        if self.closed() {
            return;
        }
        let Some(list) = self.reg.observers.get(&TypeId::of::<E>()) else { return };
        let event: Arc<dyn Any + Send + Sync> = Arc::new(event);
        for o in list {
            let cx = Cx { k: self.arc(), plugin: (o.plugin != usize::MAX).then_some(o.plugin), job: None };
            if let Some(fut) = (o.f)(cx.clone(), &*event) {
                tokio::spawn(async move {
                    if let Err(e) = fut.await {
                        if !e.cancelled() {
                            cx.k.notify_error(&format!("{} failed", cx.plugin_name()), &e.message);
                        }
                    }
                });
            }
        }
    }

    fn reschedule(&self) {
        let mut known = self.schedules.lock().unwrap();
        let snap = self.snapshot();
        for s in &self.reg.schedules {
            let every = (s.every)(&snap);
            if known.get(&s.name) == Some(&every) {
                continue;
            }
            known.insert(s.name.clone(), every);
            match every {
                Some(d) if !d.is_zero() => {
                    let cx = Cx { k: self.arc(), plugin: Some(s.plugin), job: None };
                    let _ = self.jobs.schedule(&s.name, (s.task)(&cx), Timing::repeating(d, d));
                }
                _ => {
                    let _ = self.jobs.unschedule(&s.name);
                }
            }
        }
        drop(known);
        if self.closed() {
            return;
        }
        let mut running = self.services.lock().unwrap();
        for s in &self.reg.services {
            let spec = (s.spec)(&snap);
            match (&spec, running.get(&s.name)) {
                (Some(new), Some((old, _))) if (s.same)(new.as_ref(), old.as_ref()) => continue,
                (None, None) => continue,
                _ => {}
            }
            if let Some((_, task)) = running.remove(&s.name) {
                task.abort();
            }
            let Some(spec) = spec else { continue };
            let cx = Cx { k: self.arc(), plugin: Some(s.plugin), job: None };
            let me = self.me.clone();
            let (plugin, fut) = (self.reg.plugins[s.plugin].name, (s.run)(cx, spec.clone()));
            let task = tokio::spawn(async move {
                if let Err(e) = fut.await {
                    if let Some(k) = me.upgrade().filter(|k| !k.closed()) {
                        k.notify_error(plugin, &e.message);
                    }
                }
            });
            running.insert(s.name.clone(), (spec, task.abort_handle()));
        }
    }

    // ---- notices ----------------------------------------------------------
    pub fn notify(&self, title: &str, text: &str) {
        self.push_notice(title, text, false, None);
    }
    pub fn notify_error(&self, title: &str, text: &str) {
        self.push_notice(title, text, true, None);
    }
    /// Redraw every window and browser session (state that lives outside `State`).
    pub fn repaint(&self) {
        self.changes.notify();
    }
    /// An error whose details live on `page`.
    pub fn notify_error_at(&self, title: &str, text: &str, page: Option<&'static str>) {
        self.push_notice(title, text, true, page);
    }
    fn push_notice(&self, title: &str, text: &str, error: bool, page: Option<&'static str>) {
        let id = self.next_notice.fetch_add(1, Ordering::Relaxed);
        let mut n = self.notices.lock().unwrap();
        n.push_back(Notice { id, title: title.into(), text: text.into(), error, page });
        while n.len() > 16 {
            n.pop_front();
        }
        drop(n);
        self.changes.notify();
    }
    pub(crate) fn notices_after(&self, id: u64) -> Vec<Notice> {
        self.notices.lock().unwrap().iter().filter(|n| n.id > id).cloned().collect()
    }
    /// A fresh notice id (for toasts that live in one session only).
    pub(crate) fn next_notice_id(&self) -> u64 {
        self.next_notice.fetch_add(1, Ordering::Relaxed)
    }
    pub(crate) fn last_notice(&self) -> u64 {
        self.next_notice.load(Ordering::Relaxed) - 1
    }

    // ---- plugin manager ---------------------------------------------------
    /// Save a plugin on/off choice. It applies after the next kernel restart.
    pub fn set_plugin(&self, id: &str, enabled: bool) -> Result<()> {
        let mut t = self.saved_toggles.lock().unwrap();
        let mut changes = vec![(id.to_owned(), enabled)];
        if enabled {
            if let Some(p) = self.catalog.iter().find(|p| p.id == id) {
                if p.category.policy == Policy::Exclusive {
                    for other in self.catalog.iter().filter(|o| o.id != id && o.category.id == p.category.id) {
                        changes.push((other.id.to_owned(), false));
                    }
                }
            }
        }
        self.store.transaction(|tx| {
            for (id, on) in &changes {
                tx.put(&PluginRow { id: id.clone(), enabled: *on })?;
            }
            Ok(())
        })?;
        t.extend(changes);
        drop(t);
        self.changes.notify();
        Ok(())
    }
    /// True when saved plugin choices differ from the running set.
    pub fn restart_pending(&self) -> bool {
        let saved = self.saved_toggles.lock().unwrap();
        let (want, _) = resolve(&self.catalog, &saved);
        let want: Vec<&str> = want.iter().map(|p| p.id).collect();
        let have: Vec<&str> = self.reg.plugins.iter().map(|p| p.id).collect();
        want != have
    }
    pub fn request_restart(&self) {
        (self.restart)()
    }
}

fn panic_text(p: Box<dyn Any + Send>) -> String {
    p.downcast_ref::<String>()
        .cloned()
        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "internal error".into())
}

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
        Cx { k: self.k.clone(), plugin, job: self.job.clone() }
    }
    pub fn kernel(&self) -> &Arc<Kernel> {
        &self.k
    }
    pub fn plugin_id(&self) -> &'static str {
        self.plugin.map_or("kernel", |p| self.k.reg.plugins[p].id)
    }
    pub fn plugin_name(&self) -> &'static str {
        self.plugin.map_or("rproxy", |p| self.k.reg.plugins[p].name)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.k.snapshot()
    }
    pub fn get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.k.snapshot().get(s)
    }
    pub fn try_get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Option<Arc<T>> {
        self.k.snapshot().try_get(s)
    }
    /// Atomic change of this plugin's state. Keep network I/O outside.
    pub async fn write<R>(&self, f: impl FnOnce(&mut Write<'_>) -> Result<R>) -> Result<R> {
        self.k.write(self.plugin, f).await
    }
    /// Run an op inline (inherits this job's progress and cancellation).
    pub fn call<O: Op>(&self, op: O) -> impl std::future::Future<Output = Result<O::Reply>> + Send + 'static {
        let h = self.k.reg.ops.get(&TypeId::of::<O>()).map(|b| &**b);
        op::run(self, h, op)
    }
    /// Run every handler that accepts `op` (fan-out), each without `next`.
    pub async fn call_all<O: Op + Clone>(&self, op: O) -> Vec<(&'static str, Result<O::Reply>)> {
        let snap = self.snapshot();
        let list = self
            .k
            .reg
            .ops
            .get(&TypeId::of::<O>())
            .and_then(|b| b.downcast_ref::<Handlers<O>>())
            .map(|h| op::chain(&h.0, &snap, &op))
            .unwrap_or_default();
        let mut out = Vec::new();
        for h in list {
            let cx = self.as_plugin(Some(h.plugin));
            let r = op::run_single(&cx, &h, op.clone()).await;
            out.push((self.k.reg.plugins[h.plugin].id, r));
        }
        out
    }
    /// The job that runs ops made by `make` with this context.
    pub(crate) fn task_with<O: Op>(&self, spec: crate::JobSpec, make: impl Fn() -> O + Send + Sync + 'static) -> Task {
        let cx = self.clone();
        let label = spec.label.clone();
        let page = spec.page;
        Task::new(spec.key, move |jcx| {
            let run = jcx.clone();
            let cx = Cx { job: Some(jcx), ..cx.clone() };
            let (label, op) = (label.clone(), make());
            async move {
                let r = cx.call(op).await.map(drop);
                if let Err(e) = &r {
                    if !e.cancelled() && !cx.k.closed() {
                        // An error reads as one: "Couldn't …" set by the op, else "<label> failed".
                        let title = run.run().and_then(|r| r.failure_title).unwrap_or_else(|| format!("{label} failed"));
                        cx.k.notify_error_at(&title, &e.message, page);
                    }
                }
                r
            }
        })
        .label(spec.label)
        .quiet(spec.quiet)
    }
    /// Start an op as a background job (see [`Op::job`] and [`crate::JobSpec::queue`]).
    pub fn spawn<O: Op>(&self, op: O) -> Result<RunId> {
        let spec = op.job().unwrap_or_else(|| crate::JobSpec::keyed(format!("{}:{}", short_name::<O>(), crate::Id::random()), short_name::<O>()));
        let (key, queue) = (spec.key.clone(), spec.queue);
        let cell = Arc::new(Mutex::new(Some(op)));
        let task = self.task_with(spec, move || cell.lock().unwrap().take().expect("op runs once"));
        let jobs = &self.k.jobs;
        if queue {
            if let Some(active) = jobs.board().runs.into_iter().find(|r| r.key == key && r.phase.active()) {
                // One follow-up per key: later requests are covered by it.
                if !self.k.queued.lock().unwrap().insert(key.clone()) {
                    return Ok(active.id);
                }
                let k = self.k.clone();
                let handle = jobs.handle(active.id);
                tokio::spawn(async move {
                    if let Some(h) = handle {
                        h.wait().await;
                    }
                    k.queued.lock().unwrap().remove(&key);
                    if !k.closed() {
                        let _ = k.jobs.start(task);
                    }
                });
                return Ok(active.id);
            }
        }
        Ok(jobs.start(task)?.id)
    }
    /// The active run of a job key, if any.
    pub fn running(&self, key: &str) -> Option<crate::Run> {
        self.k.jobs.board().runs.into_iter().find(|r| r.key == key && r.phase.active())
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
        tokio::select! { biased; _ = self.cancelled() => Err(Error::new("cancelled", "Cancelled")), v = fut => Ok(v) }
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
        CoreApi { core: self.k.core.clone(), kernel: self.k.clone() }
    }
    /// Default direct HTTP client (no proxy, 30 s timeout).
    pub fn http(&self) -> &reqwest::Client {
        &self.k.http
    }
    pub fn notify(&self, text: &str) {
        self.k.notify(self.plugin_name(), text)
    }
}
