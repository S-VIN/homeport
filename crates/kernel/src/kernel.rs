use crate::{
    Error, Op, Plugin, Policy, Result, RunId, Setup, State,
    changes::Changes,
    core::{Core, CoreApi},
    jobs::{JobContext, Jobs, Task, Timing},
    op::{self, Handlers, short_name},
    setup::Registry,
    state::{Cell, OrderRow, PluginRow, SettingRow, Snapshot, Value, Write},
    store::Store,
};
use std::{
    any::{Any, TypeId},
    collections::{HashMap, VecDeque},
    panic::AssertUnwindSafe,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicU64, Ordering},
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
    /// A button on the toast, see [`Cx::notify_with`].
    pub action: Option<NoticeAction>,
}
/// A toast button that runs an op of the plugin that showed the toast.
#[derive(Clone)]
pub struct NoticeAction {
    pub label: &'static str,
    pub(crate) plugin: Option<usize>,
    pub(crate) run: Arc<dyn Fn(&mut crate::UiCx) + Send + Sync>,
}
impl std::fmt::Debug for NoticeAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
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
    pub(crate) store: Store,
    pub(crate) jobs: Jobs,
    pub(crate) changes: Changes,
    pub(crate) core: Arc<Core>,
    http: reqwest::Client,
    notices: Mutex<VecDeque<Notice>>,
    next_notice: AtomicU64,
    schedules: Mutex<HashMap<String, Option<Duration>>>,
    pub(crate) updates: Mutex<crate::updates::CoreUpdates>,
    services: Mutex<HashMap<String, RunningService>>,
    pub(crate) catalog: Vec<&'static Plugin>,
    pub(crate) saved_toggles: Mutex<HashMap<String, bool>>,
    pub(crate) disabled_reasons: Vec<(String, String)>,
    pub(crate) restart: Arc<dyn Fn() + Send + Sync>,
    /// Apply a validated imported configuration (provided by the host).
    pub(crate) restore: Arc<dyn Fn(Arc<crate::configuration::Import>) + Send + Sync>,
    /// True while a window or browser tab shows the UI.
    ui_open: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Set by shutdown: no more writes, events or jobs from this kernel.
    closed: std::sync::atomic::AtomicBool,
    /// Event handlers and UI ops in flight: cancelled and awaited on shutdown.
    tasks: tokio_util::task::TaskTracker,
    stop: tokio_util::sync::CancellationToken,
    /// Job keys with a queued follow-up run.
    /// The run waiting behind an active job of the same key (the newest request).
    queued: Mutex<HashMap<String, Task>>,
    pub data_dir: PathBuf,
    me: Weak<Kernel>,
}

pub(crate) fn is_enabled(p: &Plugin, saved: &HashMap<String, bool>) -> bool {
    p.required || saved.get(p.id).copied().unwrap_or(p.default_enabled)
}

/// A running service: its spec (`None` while stopping), the sender whose drop
/// stops it, and its task, which ends after the run and its `on_stop` finished.
struct RunningService {
    spec: Option<Arc<dyn std::any::Any + Send + Sync>>,
    _stop: tokio::sync::oneshot::Sender<()>,
    done: tokio::task::JoinHandle<()>,
}

/// Enabled plugins in dependency order, plus the reason each skipped plugin is off.
pub(crate) fn resolve(
    catalog: &[&'static Plugin],
    saved: &HashMap<String, bool>,
) -> (Vec<&'static Plugin>, Vec<(String, String)>) {
    let mut policies: HashMap<&str, Policy> = HashMap::new();
    for p in catalog {
        if let Some(old) = policies.insert(p.category.id, p.category.policy) {
            assert_eq!(
                old, p.category.policy,
                "category {} declared with two policies",
                p.category.id
            );
        }
    }
    let mut why = Vec::new();
    let mut on: Vec<&'static Plugin> = catalog
        .iter()
        .copied()
        .filter(|p| is_enabled(p, saved))
        .collect();
    on.retain(|p| {
        if p.supported() {
            return true;
        }
        assert!(
            !p.required,
            "required plugin {} does not support this system",
            p.id
        );
        why.push((p.id.to_owned(), p.platforms_note().unwrap_or_default()));
        false
    });
    // Exclusive categories: the first enabled plugin wins.
    let mut taken: HashMap<&str, &str> = HashMap::new();
    on.retain(|p| {
        if p.category.policy == Policy::Exclusive {
            if let Some(winner) = taken.get(p.category.id) {
                let winner = catalog
                    .iter()
                    .find(|c| c.id == *winner)
                    .map_or(*winner, |c| c.name);
                why.push((
                    p.id.to_owned(),
                    format!(
                        "{winner} is already on; only one {} plugin can run",
                        p.category.name.to_lowercase()
                    ),
                ));
                return false;
            }
            taken.insert(p.category.id, p.id);
        }
        true
    });
    // A requirement names a plugin or a capability it provides.
    let gives = |p: &Plugin, need: &str| p.id == need || p.provides.contains(&need);
    loop {
        let before = on.len();
        let present = on.clone();
        on.retain(|p| {
            match p
                .requires
                .iter()
                .find(|r| !present.iter().any(|o| gives(o, r)))
            {
                Some(missing) => {
                    let name = catalog
                        .iter()
                        .find(|c| gives(c, missing))
                        .map_or(*missing, |c| c.name);
                    why.push((p.id.to_owned(), format!("Needs {name}, which is off")));
                    false
                }
                None => true,
            }
        });
        if on.len() == before {
            break;
        }
    }
    let mut ordered: Vec<&'static Plugin> = Vec::new();
    while ordered.len() < on.len() {
        let next = on
            .iter()
            .find(|p| {
                !ordered.iter().any(|o| o.id == p.id)
                    && p.requires
                        .iter()
                        .all(|r| ordered.iter().any(|o| gives(o, r)))
            })
            .copied()
            .expect("plugin dependency cycle");
        ordered.push(next);
    }
    (ordered, why)
}

impl Kernel {
    pub(crate) fn load_configuration(
        catalog: &[&'static Plugin],
        store: &Store,
        data_dir: &std::path::Path,
        all_plugins: bool,
    ) -> Result<Snapshot> {
        store.register::<PluginRow>()?;
        store.register::<OrderRow>()?;
        store.register::<SettingRow>()?;
        store.register::<crate::state::MigrationRow>()?;
        let saved: HashMap<String, bool> = store
            .transaction(|tx| tx.all::<PluginRow>())?
            .into_iter()
            .map(|r| (r.id, r.enabled))
            .collect();
        // Import validation also reads data of disabled plugins (not of plugins
        // for other systems: those never run here). It
        // only registers/deserializes; no services or core process are started.
        let plugins = if all_plugins {
            catalog.iter().copied().filter(|p| p.supported()).collect()
        } else {
            resolve(catalog, &saved).0
        };
        let mut reg = Registry {
            plugins,
            data_dir: data_dir.to_owned(),
            ..Default::default()
        };
        Setup {
            reg: &mut reg,
            plugin: None,
        }
        .setting(&THEME);
        for i in 0..reg.plugins.len() {
            let p = reg.plugins[i];
            (p.setup)(&mut Setup {
                reg: &mut reg,
                plugin: Some(i),
            })
            .map_err(|e| Error::msg(format!("Plugin {} failed to start: {e}", p.name)))?;
        }
        reg.finish();
        migrate(store, &mut reg)?;
        let mut cells = load_cells(store, &mut reg, all_plugins)?;
        for (name, init) in &reg.live {
            cells.insert(name, Cell { value: init() });
        }
        let reg = Arc::new(reg);
        Ok(Snapshot {
            cells: Arc::new(cells),
            reg,
            rev: 1,
        })
    }

    pub(crate) async fn start(
        catalog: Vec<&'static Plugin>,
        store: Store,
        data_dir: PathBuf,
        restart: Arc<dyn Fn() + Send + Sync>,
        restore: Arc<dyn Fn(Arc<crate::configuration::Import>) + Send + Sync>,
        ui_open: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Arc<Kernel>> {
        let snapshot = Self::load_configuration(&catalog, &store, &data_dir, false)?;
        let reg = snapshot.reg.clone();
        let saved: HashMap<String, bool> = store
            .transaction(|tx| tx.all::<PluginRow>())?
            .into_iter()
            .map(|r| (r.id, r.enabled))
            .collect();
        let (_, disabled_reasons) = resolve(&catalog, &saved);
        let changes = Changes::default();
        let k = Arc::new_cyclic(|me| Kernel {
            reg,
            snap: RwLock::new(snapshot.clone()),
            writer: tokio::sync::Mutex::new(()),
            store,
            jobs: Jobs::with_changes(Handle::current(), changes.clone()),
            core: Arc::new(Core::new(changes.clone(), data_dir.join("core"))),
            changes,
            http: crate::mihomo::http()
                .no_proxy()
                .user_agent("rproxy")
                .timeout(Duration::from_secs(30))
                .build()
                .expect("HTTP client"),
            notices: Mutex::default(),
            next_notice: AtomicU64::new(1),
            schedules: Mutex::default(),
            services: Mutex::default(),
            updates: Mutex::new(crate::updates::CoreUpdates {
                installed: rproxy_mihomo::installed(&data_dir.join("core")).is_file(),
                ..Default::default()
            }),
            catalog,
            saved_toggles: Mutex::new(saved),
            disabled_reasons,
            restart,
            restore,
            ui_open,
            closed: Default::default(),
            tasks: Default::default(),
            stop: Default::default(),
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
        for warning in &k.reg.warnings {
            k.notify_error("Settings", warning);
        }
        k.dispatch(Started);
        Ok(k)
    }

    pub async fn shutdown(&self) {
        {
            // Synchronous plugin choices must finish before the closed flag,
            // and later choices see it under the same lock.
            let _toggles = self.saved_toggles.lock().unwrap();
            self.closed.store(true, Ordering::SeqCst);
        }
        // Stop every service and wait for its cleanup (e.g. restoring the system proxy).
        let services: Vec<_> = self
            .services
            .lock()
            .unwrap()
            .drain()
            .map(|(_, s)| s.done)
            .collect();
        for done in services {
            let _ = done.await;
        }
        self.stop.cancel();
        self.tasks.close();
        self.tasks.wait().await;
        self.jobs.shutdown().await;
        // UI operations without a job can still hold the writer across core I/O.
        // Drain them before the host snapshots or replaces the shared database.
        let _writer = self.writer.lock().await;
        self.core.stop().await;
    }

    pub fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    /// Run work that belongs to `cx`'s plugin (an event handler, a UI op): it
    /// is cancelled and awaited on shutdown, and its error or panic is shown
    /// with the plugin's name. Resolves to the result unless cancelled.
    pub(crate) fn track<R: Send + 'static>(
        &self,
        cx: &Cx,
        fut: impl std::future::Future<Output = Result<R>> + Send + 'static,
    ) -> tokio::task::JoinHandle<Option<Result<R>>> {
        let (cx, stop) = (cx.clone(), self.stop.clone());
        self.tasks.spawn(async move {
            let result = tokio::select! {
                r = caught(fut) => r,
                _ = stop.cancelled() => return None,
            };
            if let Err(e) = &result
                && !e.cancelled()
                && !cx.k.closed()
            {
                cx.k.notify_error(&format!("{} failed", cx.plugin_name()), &e.message);
            }
            Some(result)
        })
    }
    /// Wait until no job, event handler or UI op is running (tests).
    pub async fn idle(&self) {
        loop {
            let busy =
                !self.tasks.is_empty() || self.jobs.board().runs.iter().any(|r| r.phase.active());
            if !busy {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }
    fn ensure_open(&self) -> Result<()> {
        if self.closed() {
            return Err(Error::new("closed", "rproxy is restarting."));
        }
        Ok(())
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snap.read().unwrap().clone()
    }
    pub(crate) fn arc(&self) -> Arc<Kernel> {
        self.me.upgrade().expect("kernel alive")
    }
    /// A context acting as the kernel itself.
    pub fn cx(&self) -> Cx {
        Cx {
            k: self.arc(),
            plugin: None,
            job: None,
        }
    }
    /// A context acting as plugin `id` (tests, glue code).
    pub fn cx_for(&self, id: &str) -> Cx {
        let plugin = self
            .reg
            .plugin_index(id)
            .unwrap_or_else(|| panic!("no enabled plugin `{id}`"));
        Cx {
            k: self.arc(),
            plugin: Some(plugin),
            job: None,
        }
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
    /// Typed controller API of the running core.
    pub fn core_api(&self) -> CoreApi {
        CoreApi {
            core: self.core.clone(),
            kernel: self.arc(),
        }
    }
    pub fn core_status(&self) -> crate::core::CoreStatus {
        self.core.status()
    }

    /// Restart mihomo and apply the saved configuration.
    pub async fn restart_core(&self) -> Result<()> {
        let _w = self.writer.lock().await;
        self.ensure_open()?;
        self.core.start(&self.reg, &self.snapshot()).await
    }

    /// Re-run the core apply for the current snapshot under the writer lock.
    pub(crate) async fn resync(&self) -> Result<()> {
        let _w = self.writer.lock().await;
        self.ensure_open()?;
        self.core.sync(&self.reg, &self.snapshot()).await
    }

    // ---- transactions ---------------------------------------------------
    pub(crate) async fn write<R>(
        &self,
        plugin: Option<usize>,
        f: impl FnOnce(&mut Write<'_>) -> Result<R>,
    ) -> Result<R> {
        let _w = self.writer.lock().await;
        self.ensure_open()?;
        let base = self.snapshot();
        let mut w = Write::new(&base, plugin);
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| f(&mut w)))
            .map_err(|p| Error::new("bug", panic_text(&*p)))??;
        for c in &self.reg.cascades {
            c(&base, &mut w);
        }
        let Write {
            changed, events, ..
        } = w;
        if changed.is_empty() {
            drop(_w);
            for e in events {
                e(&self.arc());
            }
            return Ok(out);
        }
        let names: Vec<&'static str> = changed.keys().copied().collect();
        let changed: HashMap<&'static str, Value> = changed
            .into_iter()
            .map(|(k, v)| (k, Arc::from(v) as Value))
            .collect();
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
        self.dispatch(crate::Changed {
            states: Arc::new(names),
        });
        self.reschedule();
        Ok(out)
    }

    pub(crate) fn dispatch<E: Clone + Send + Sync + 'static>(&self, event: E) {
        if self.closed() {
            return;
        }
        let Some(list) = self.reg.observers.get(&TypeId::of::<E>()) else {
            return;
        };
        let event: Arc<dyn Any + Send + Sync> = Arc::new(event);
        for o in list {
            let cx = Cx {
                k: self.arc(),
                plugin: (o.plugin != usize::MAX).then_some(o.plugin),
                job: None,
            };
            if let Some(fut) = (o.f)(cx.clone(), &*event) {
                o.policy.run(&cx, fut);
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
                    let cx = Cx {
                        k: self.arc(),
                        plugin: Some(s.plugin),
                        job: None,
                    };
                    let _ = self
                        .jobs
                        .schedule(&s.name, (s.task)(&cx), Timing::repeating(d, d));
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
            let current = running.remove(&s.name);
            let unchanged = match (&spec, &current) {
                (Some(new), Some(r)) => r
                    .spec
                    .as_ref()
                    .is_some_and(|old| (s.same)(new.as_ref(), old.as_ref())),
                (None, None) => true,
                (None, Some(r)) => r.spec.is_none(),
                (Some(_), None) => false,
            };
            if unchanged {
                if let Some(r) = current {
                    running.insert(s.name.clone(), r);
                }
                continue;
            }
            // Dropping the current run's sender stops it; the next run waits until it has.
            let previous = current.map(|r| r.done);
            let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
            let cx = Cx {
                k: self.arc(),
                plugin: Some(s.plugin),
                job: None,
            };
            let (plugin, run, on_stop, restart) = (
                self.reg.plugins[s.plugin].name,
                s.run.clone(),
                s.stop.clone(),
                s.restart,
            );
            let report = move |cx: &Cx, r: Result<()>| {
                if let (Err(e), false) = (r, cx.k.closed()) {
                    cx.k.notify_error(plugin, &e.message);
                }
            };
            let task_spec = spec.clone();
            let done = tokio::spawn(async move {
                if let Some(previous) = previous {
                    let _ = previous.await;
                }
                let Some(spec) = task_spec else { return };
                let runs = async {
                    // With `restart_on_error`, a failed run is retried after a pause
                    // that doubles up to five minutes; the same error is shown once.
                    let mut pause = std::time::Duration::from_secs(2);
                    let mut shown: Option<String> = None;
                    loop {
                        let r = caught(run(cx.clone(), spec.clone())).await;
                        let failed = r.as_ref().err().map(|e| e.message.clone());
                        if failed.is_none() || failed != shown {
                            report(&cx, r);
                        }
                        if failed.is_none() || !restart {
                            return;
                        }
                        shown = failed;
                        tokio::time::sleep(pause).await;
                        pause = (pause * 2).min(std::time::Duration::from_secs(300));
                    }
                };
                tokio::select! {
                    _ = runs => {}
                    _ = stopped => {}
                }
                if let Some(on_stop) = on_stop {
                    report(&cx, caught(on_stop(cx.clone(), spec)).await);
                }
            });
            running.insert(
                s.name.clone(),
                RunningService {
                    spec,
                    _stop: stop,
                    done,
                },
            );
        }
    }

    // ---- notices ----------------------------------------------------------
    pub fn notify(&self, title: &str, text: &str) {
        self.push_notice(title, text, false, None, None);
    }
    pub fn notify_error(&self, title: &str, text: &str) {
        self.push_notice(title, text, true, None, None);
    }
    /// Redraw every window and browser session (state that lives outside `State`).
    pub fn repaint(&self) {
        self.changes.notify();
    }
    /// An error whose details live on `page`.
    pub fn notify_error_at(&self, title: &str, text: &str, page: Option<&'static str>) {
        self.push_notice(title, text, true, page, None);
    }
    fn push_notice(
        &self,
        title: &str,
        text: &str,
        error: bool,
        page: Option<&'static str>,
        action: Option<NoticeAction>,
    ) {
        let id = self.next_notice.fetch_add(1, Ordering::Relaxed);
        let mut n = self.notices.lock().unwrap();
        n.push_back(Notice {
            id,
            title: title.into(),
            text: text.into(),
            error,
            page,
            action,
        });
        while n.len() > 16 {
            n.pop_front();
        }
        drop(n);
        self.changes.notify();
    }
    /// Notices newer than `id` (0: all kept), e.g. to check in a test that
    /// an error was shown.
    pub fn notices_after(&self, id: u64) -> Vec<Notice> {
        self.notices
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.id > id)
            .cloned()
            .collect()
    }
    /// Notices whose action nobody has run yet.
    pub(crate) fn notice_actions(&self) -> Vec<u64> {
        let notices = self.notices.lock().unwrap();
        notices
            .iter()
            .filter(|n| n.action.is_some())
            .map(|n| n.id)
            .collect()
    }
    /// The action of a notice, for the first window that presses it.
    pub(crate) fn take_notice_action(&self, id: u64) -> Option<NoticeAction> {
        let mut notices = self.notices.lock().unwrap();
        notices.iter_mut().find(|n| n.id == id)?.action.take()
    }
    pub(crate) fn last_notice(&self) -> u64 {
        self.next_notice.load(Ordering::Relaxed) - 1
    }

    // ---- plugin manager ---------------------------------------------------
    /// Save a plugin on/off choice. It applies after the next kernel restart.
    pub fn set_plugin(&self, id: &str, enabled: bool) -> Result<()> {
        let mut t = self.saved_toggles.lock().unwrap();
        self.ensure_open()?;
        let mut changes = vec![(id.to_owned(), enabled)];
        if enabled
            && let Some(p) = self.catalog.iter().find(|p| p.id == id)
            && p.category.policy == Policy::Exclusive
        {
            for other in self
                .catalog
                .iter()
                .filter(|o| o.id != id && o.category.id == p.category.id)
            {
                changes.push((other.id.to_owned(), false));
            }
        }
        self.store.transaction(|tx| {
            for (id, on) in &changes {
                tx.put(&PluginRow {
                    id: id.clone(),
                    enabled: *on,
                })?;
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
    pub fn kernel(&self) -> &Arc<Kernel> {
        &self.k
    }
    pub fn plugin_id(&self) -> &'static str {
        self.plugin.map_or("kernel", |p| self.k.reg.plugins[p].id)
    }
    /// The plugin's own folder, as `Setup::data_dir`.
    pub fn data_dir(&self) -> std::path::PathBuf {
        let id = self.plugin.map_or("kernel", |p| self.k.reg.plugins[p].id);
        self.k.reg.data_dir.join("plugins").join(id)
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
                    cx.k.notify_error_at(&title, &e.message, page);
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
        let (key, queue, latest) = (self.job_key(&op, &spec.key), spec.queue, spec.latest);
        let cell = Arc::new(Mutex::new(Some(op)));
        let task = self.task_with(key.clone(), spec, None, move || {
            cell.lock().unwrap().take().expect("op runs once")
        });
        let jobs = &self.k.jobs;
        if (queue || latest)
            && let Some(active) = jobs
                .board()
                .runs
                .into_iter()
                .find(|r| r.key == key && r.phase.active())
        {
            if latest {
                let _ = jobs.cancel(active.id);
            }
            // One follow-up per key, always the newest request.
            if self
                .k
                .queued
                .lock()
                .unwrap()
                .insert(key.clone(), task)
                .is_some()
            {
                return Ok(active.id);
            }
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
                let next = k.queued.lock().unwrap().remove(&key);
                if let Some(task) = next.filter(|_| !k.closed()) {
                    let _ = k.jobs.start(task);
                }
            });
            return Ok(active.id);
        }
        Ok(jobs.start(task)?.id)
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
            _ = self.cancelled() => Err(Error::new("cancelled", "Cancelled")),
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
    pub fn notify(&self, text: &str) {
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
        self.k
            .push_notice(self.plugin_name(), text, false, None, Some(action));
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

/// Bring saved data up to the plugins' schemas: lenient table step (new
/// columns), migrations in version order, then the strict check. A plugin that
/// never saved anything starts on its newest schema: its migrations are
/// recorded as done without running.
fn migrate(store: &Store, reg: &mut Registry) -> Result<()> {
    use crate::state::MigrationRow;
    let done = |id: String| MigrationRow {
        id,
        at: crate::now() as i64,
    };
    reg.migrations.sort_by_key(|(p, v, _)| (*p, *v));
    let fresh: Vec<bool> = store.transaction(|tx| {
        (0..reg.plugins.len())
            .map(|p| Ok(!installed(tx, reg.plugins[p].id)? && !has_data(tx.sql(), reg, p)?))
            .collect()
    })?;
    for (_, _, p) in &reg.persist {
        (p.register)(store, false)?;
    }
    for (p, version, step) in &reg.migrations {
        let plugin = &reg.plugins[*p];
        let id = format!("{}|{version}", plugin.id);
        store
            .transaction(|tx| {
                if tx.get::<MigrationRow>(id.as_str())?.is_none() {
                    if !fresh[*p] {
                        step(tx.sql())?;
                    }
                    tx.put(&done(id.clone()))?;
                }
                Ok(())
            })
            .map_err(|e| Error::msg(format!("{}: migration {version} failed: {e}", plugin.name)))?;
    }
    store.transaction(|tx| {
        for p in &reg.plugins {
            let id = format!("{}|installed", p.id);
            if tx.get::<MigrationRow>(id.as_str())?.is_none() {
                tx.put(&done(id))?;
            }
        }
        Ok(())
    })?;
    for (_, _, p) in &reg.persist {
        (p.register)(store, true)?;
    }
    Ok(())
}

/// Read every persisted state. A setting that no longer reads (an older or
/// damaged value) starts from its default instead of stopping the app, with a
/// warning; import validation (`strict`) refuses it.
fn load_cells(
    store: &Store,
    reg: &mut Registry,
    strict: bool,
) -> Result<HashMap<&'static str, Cell>> {
    let mut warnings = Vec::new();
    let cells = store.transaction(|tx| {
        let mut cells = HashMap::new();
        for (name, plugin, p) in &reg.persist {
            let value = match ((p.load)(tx), &p.fallback) {
                (Ok(value), _) => value,
                (Err(e), Some(default)) if !strict && e.code == "setting" => {
                    set_aside(tx, name)?;
                    let owner = plugin.map_or("rproxy", |i| reg.plugins[i].name);
                    let warning = format!("{owner}: the saved setting {name} could not be read, so its default is used.");
                    warnings.push(warning);
                    default()
                }
                (Err(e), _) => return Err(e),
            };
            cells.insert(*name, Cell { value });
        }
        Ok(cells)
    })?;
    reg.warnings = warnings;
    Ok(cells)
}

/// Keep an unreadable setting as `<name>.broken` and drop it, so the profile
/// still exports and imports cleanly.
fn set_aside(tx: &crate::store::Tx<'_>, name: &str) -> Result<()> {
    if let Some(row) = tx.get::<SettingRow>(name)? {
        tx.put(&SettingRow {
            id: format!("{name}.broken"),
            json: row.json,
        })?;
        tx.delete::<SettingRow>(name)?;
    }
    Ok(())
}

/// The plugin ran here before (it gets a `<plugin>|installed` migration row).
fn installed(tx: &crate::store::Tx<'_>, plugin: &str) -> Result<bool> {
    Ok(tx
        .get::<crate::state::MigrationRow>(format!("{plugin}|installed").as_str())?
        .is_some())
}

/// The plugin saved something before: one of its tables exists or it has a
/// setting (for data from before the `installed` row existed).
fn has_data(
    sql: &crate::store::rusqlite::Connection,
    reg: &Registry,
    plugin: usize,
) -> Result<bool> {
    let id = reg.plugins[plugin].id;
    let exists = |table: &str| -> Result<bool> {
        let query = "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?)";
        Ok(sql.query_row(query, [table], |r| r.get(0))?)
    };
    let setting: bool = sql.query_row(
        "SELECT EXISTS(SELECT 1 FROM kernel_settings WHERE substr(id, 1, length(?1) + 1) = ?1 || '.')",
        [id],
        |r| r.get(0),
    )?;
    let mut tables = reg
        .tables
        .iter()
        .filter(|(_, state)| reg.owner(state) == Some(plugin))
        .map(|(table, _)| *table);
    Ok(setting || tables.try_fold(false, |found, t| Ok::<_, Error>(found || exists(t)?))?)
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
            Err(Error::new("cancelled", "Changed while this was running."))
        }
    }
}

/// Run `fut`; a panic becomes an error with its text, so it is shown and handled
/// like any other failure.
pub(crate) async fn caught<R>(fut: impl std::future::Future<Output = Result<R>>) -> Result<R> {
    use futures_util::FutureExt;
    std::panic::AssertUnwindSafe(fut)
        .catch_unwind()
        .await
        .unwrap_or_else(|panic| Err(Error::new("panic", panic_text(&*panic))))
}

/// The message of a caught panic.
pub(crate) fn panic_text(panic: &(dyn Any + Send)) -> String {
    let text = panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned());
    format!(
        "Crashed: {}",
        text.unwrap_or_else(|| "unknown error".into())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Platform, categories};
    static ELSEWHERE: Plugin = Plugin {
        id: "elsewhere",
        name: "Elsewhere",
        description: "",
        category: categories::NETWORK,
        requires: &[],
        provides: &[],
        required: false,
        default_enabled: true,
        platforms: if cfg!(windows) {
            &[Platform::Macos]
        } else {
            &[Platform::Windows]
        },
        setup: |_| Ok(()),
    };
    static NEEDS_IT: Plugin = Plugin {
        id: "needs-it",
        name: "Needs it",
        requires: &["elsewhere"],
        platforms: Platform::ALL,
        ..ELSEWHERE
    };
    #[test]
    fn plugins_for_other_systems_are_skipped_with_a_reason() {
        let (on, why) = resolve(&[&ELSEWHERE, &NEEDS_IT], &HashMap::new());
        assert!(on.is_empty());
        let only = if cfg!(windows) {
            "macOS only"
        } else {
            "Windows only"
        };
        assert!(why.contains(&("elsewhere".into(), only.into())));
        assert!(why.contains(&("needs-it".into(), "Needs Elsewhere, which is off".into())));
    }
}
