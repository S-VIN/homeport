use crate::{
    Error, Op, Plugin, Result, RunId, Setup, State,
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

mod cx;
mod migrate;
mod notices;
pub use cx::{Cx, Guard, Input, LiveMark};
use migrate::load_cells;
pub(crate) use migrate::migrate;
pub use notices::{Notice, NoticeAction};

/// Emitted once after all plugins loaded and the core started.
#[derive(Clone, Debug)]
pub struct Started;

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
    /// With the cell of its op, so a newer request merges into it ([`Op::merge`]).
    queued: Mutex<HashMap<String, (Task, Box<dyn std::any::Any + Send>)>>,
    pub data_dir: PathBuf,
    me: Weak<Kernel>,
}

pub(crate) fn is_enabled(p: &Plugin, saved: &HashMap<String, bool>) -> bool {
    p.required || saved.get(p.id).copied().unwrap_or(false)
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
    let gives = |p: &Plugin, need: &Plugin| p.id == need.id;
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
                    let name = missing.name;
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
        migrate(store, &[])?;
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
        migrate(store, &reg.plugins)?;
        store.check::<PluginRow>()?;
        store.check::<OrderRow>()?;
        store.check::<SettingRow>()?;
        store.check::<crate::state::MigrationRow>()?;
        for (_, _, p) in &reg.persist {
            (p.check)(store)?;
        }
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
        crate::logging::init(&catalog);
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
                .user_agent("homeport")
                .timeout(Duration::from_secs(30))
                .build()
                .expect("HTTP client"),
            notices: Mutex::default(),
            next_notice: AtomicU64::new(1),
            schedules: Mutex::default(),
            services: Mutex::default(),
            updates: Mutex::new(crate::updates::CoreUpdates {
                installed: homeport_mihomo::installed(&data_dir.join("core")).is_file(),
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
        let on: Vec<&str> = k.reg.plugins.iter().map(|p| p.name).collect();
        log::info!("Started with {}", on.join(", "));
        for (title, warning) in &k.reg.warnings {
            k.notify_error(title, warning);
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
                cx.k.notice(
                    cx.plugin,
                    &format!("{} failed", cx.plugin_name()),
                    &e.message,
                );
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
            return Err(Error::new("closed", "Homeport is restarting."));
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
        // A panic in the change or a cascade fails this write, not the writer.
        let out = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let out = f(&mut w)?;
            for c in &self.reg.cascades {
                c(&base, &mut w);
            }
            Ok::<R, Error>(out)
        }))
        .map_err(|p| Error::new("bug", panic_text(&*p)))??;
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
            let (plugin, run, on_stop, restart) =
                (s.plugin, s.run.clone(), s.stop.clone(), s.restart);
            let report = move |cx: &Cx, r: Result<()>| {
                if let (Err(e), false) = (r, cx.k.closed()) {
                    let name = cx.k.reg.plugins[plugin].name;
                    cx.k.notice(Some(plugin), name, &e.message);
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

    /// Redraw every window and browser session (state that lives outside `State`).
    pub fn repaint(&self) {
        self.changes.notify();
    }

    // ---- plugin manager ---------------------------------------------------
    /// Save a plugin on/off choice. It applies after the next kernel restart.
    pub fn set_plugin(&self, id: &str, enabled: bool) -> Result<()> {
        let mut t = self.saved_toggles.lock().unwrap();
        self.ensure_open()?;
        let changes = vec![(id.to_owned(), enabled)];
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
        if let Some(p) = self.catalog.iter().find(|p| p.id == id) {
            let state = if enabled { "on" } else { "off" };
            log::info!("{} turned {state}; applies after a restart", p.name);
        }
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
    use crate::Platform;
    static ELSEWHERE: Plugin = Plugin {
        id: "elsewhere",
        name: "Elsewhere",
        platforms: if cfg!(windows) {
            &[Platform::Macos]
        } else {
            &[Platform::Windows]
        },
        ..Plugin::DEFAULT
    };
    static NEEDS_IT: Plugin = Plugin {
        id: "needs-it",
        name: "Needs it",
        requires: &[&ELSEWHERE],
        platforms: Platform::ALL,
        ..ELSEWHERE
    };
    #[test]
    fn plugins_for_other_systems_are_skipped_with_a_reason() {
        let saved = HashMap::from([("elsewhere".into(), true), ("needs-it".into(), true)]);
        let (on, why) = resolve(&[&ELSEWHERE, &NEEDS_IT], &saved);
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
