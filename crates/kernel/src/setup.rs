//! Registration: the only API a plugin's `setup` function uses.
use crate::{
    Claim, Cx, Hook, Id, Next, Op, Plugin, Rank, Record, Result, Slot, Snapshot, State, list,
    mihomo::Fragment,
    op::{BoxFut, Handler, Handlers},
    slot::{Contribution, Contributions, HookFn, HookFns, ViewFn},
    state::{self, Persist, Value},
    ui::{Page, PageSpec, UiCx},
};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    future::Future,
    sync::Arc,
    time::Duration,
};

pub(crate) type ObserverFn =
    Arc<dyn Fn(Cx, &(dyn Any + Send + Sync)) -> Option<BoxFut<()>> + Send + Sync>;
pub(crate) struct Observer {
    pub plugin: usize,
    pub f: ObserverFn,
    pub policy: Policy,
}

/// What happens when an event arrives while the observer still handles the
/// previous one. Set with [`ObserverSpec`].
pub(crate) enum Policy {
    /// Every event gets its own run (the default).
    Parallel,
    /// The newest event wins: the previous run is cancelled.
    Latest(std::sync::Mutex<Option<tokio::task::AbortHandle>>),
    /// One run at a time; the next run handles the newest event that arrived
    /// meanwhile (older waiting ones are dropped).
    Queued(
        Arc<tokio::sync::Mutex<()>>,
        Arc<std::sync::Mutex<Option<BoxFut<()>>>>,
    ),
}
impl Policy {
    pub(crate) fn run(&self, cx: &Cx, fut: BoxFut<()>) {
        match self {
            Policy::Parallel => drop(cx.k.track(cx, fut)),
            Policy::Latest(last) => {
                let run = cx.k.track(cx, fut);
                if let Some(previous) = last.lock().unwrap().replace(run.abort_handle()) {
                    previous.abort();
                }
            }
            Policy::Queued(turn, next) => {
                if next.lock().unwrap().replace(fut).is_some() {
                    return;
                }
                let (turn, next) = (turn.clone(), next.clone());
                drop(cx.k.track(cx, async move {
                    let _turn = turn.lock().await;
                    let fut = next.lock().unwrap().take();
                    match fut {
                        Some(fut) => fut.await,
                        None => Ok(()),
                    }
                }));
            }
        }
    }
}

/// Returned by [`Setup::on`] and [`Setup::watch`] to choose how repeated
/// events are handled.
pub struct ObserverSpec<'a> {
    observer: &'a mut Observer,
}
impl ObserverSpec<'_> {
    /// Cancel the running handler when a newer event arrives (e.g. reload
    /// something after every edit: only the last reload matters).
    pub fn latest(self) -> Self {
        self.observer.policy = Policy::Latest(Default::default());
        self
    }
    /// One run at a time; of the events that arrive meanwhile only the newest runs.
    pub fn queued(self) -> Self {
        self.observer.policy = Policy::Queued(Default::default(), Default::default());
        self
    }
}
pub(crate) struct Schedule {
    pub plugin: usize,
    pub name: String,
    pub every: Arc<dyn Fn(&Snapshot) -> Option<Duration> + Send + Sync>,
    /// Builds the job that runs the op directly (no trampoline run).
    pub task: Arc<dyn Fn(&Cx) -> crate::jobs::Task + Send + Sync>,
}
/// A long-running task whose configuration is derived from the snapshot.
pub(crate) struct Service {
    pub plugin: usize,
    pub name: String,
    /// The current spec, type-erased; compared with the running one after each commit.
    pub spec: Arc<dyn Fn(&Snapshot) -> Option<Arc<dyn Any + Send + Sync>> + Send + Sync>,
    pub same: Arc<dyn Fn(&(dyn Any + Send + Sync), &(dyn Any + Send + Sync)) -> bool + Send + Sync>,
    pub run: Arc<dyn Fn(Cx, Arc<dyn Any + Send + Sync>) -> BoxFut<()> + Send + Sync>,
    pub stop: Option<Arc<dyn Fn(Cx, Arc<dyn Any + Send + Sync>) -> BoxFut<()> + Send + Sync>>,
    pub restart: bool,
}

/// Returned by [`Setup::service`] to add a cleanup step.
pub struct ServiceSetup<'a, S> {
    service: &'a mut Service,
    _spec: std::marker::PhantomData<fn(S)>,
}
impl<S: Clone + Send + Sync + 'static> ServiceSetup<'_, S> {
    /// Run again after an error (2 s, then doubling up to 5 min) while the
    /// spec stays the same. Without it a failed service waits for a new spec.
    pub fn restart_on_error(self) -> Self {
        self.service.restart = true;
        self
    }
    /// Undo what a run did, after it stopped: its spec changed, it was turned
    /// off, or the app quits. The next run starts only after this finished.
    pub fn on_stop<F, Fut>(self, f: F) -> Self
    where
        F: Fn(Cx, S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.service.stop = Some(Arc::new(move |cx, s| {
            let s = s.downcast_ref::<S>().expect("service spec type").clone();
            Box::pin(f(cx, s))
        }));
        self
    }
}
pub(crate) struct PageEntry {
    pub plugin: usize,
    /// The plugin id.
    pub id: &'static str,
    pub spec: PageSpec,
    pub make: Arc<dyn Fn() -> Box<dyn Page> + Send + Sync>,
}
pub(crate) type Cascade = Arc<dyn Fn(&Snapshot, &mut crate::Write<'_>) + Send + Sync>;
type SlotEdit = Box<dyn FnOnce(&mut Registry) + Send + Sync>;

/// Everything the enabled plugins registered. Immutable after startup.
#[derive(Default)]
pub(crate) struct Registry {
    pub plugins: Vec<&'static Plugin>,
    pub owners: HashMap<&'static str, Option<usize>>,
    pub persist: Vec<(&'static str, Option<usize>, Persist)>,
    /// Initial values of live states (see [`Setup::live`]).
    pub live: Vec<(&'static str, Box<dyn Fn() -> Value + Send + Sync>)>,
    pub ops: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    pub observers: HashMap<TypeId, Vec<Observer>>,
    pub hooks: HashMap<&'static str, Box<dyn Any + Send + Sync>>,
    /// The plugin answering each exclusive hook.
    pub deciders: HashMap<&'static str, Option<usize>>,
    pub slots: HashMap<&'static str, Box<dyn Any + Send + Sync>>,
    /// Columns added to lists, by list id ([`crate::columns`]).
    pub columns: HashMap<&'static str, Box<dyn Any + Send + Sync>>,
    pub slot_edits: Vec<SlotEdit>,
    /// `(slot id, plugin)` of every contribution: where a plugin shows its UI.
    pub fills: Vec<(&'static str, usize)>,
    /// `(table, state)` of every persisted list and collection.
    pub tables: Vec<(&'static str, &'static str)>,
    pub pages: Vec<PageEntry>,
    pub fragments: Vec<(usize, Arc<dyn Fn(&Snapshot) -> Fragment + Send + Sync>)>,
    pub schedules: Vec<Schedule>,
    pub services: Vec<Service>,
    pub cascades: Vec<Cascade>,
    /// `(title, text)` of problems met while starting (shown once the app runs).
    pub warnings: Vec<(&'static str, String)>,
    pub seq: usize,
    pub data_dir: std::path::PathBuf,
}
impl Registry {
    pub fn owner(&self, name: &str) -> Option<usize> {
        self.owners.get(name).copied().flatten()
    }
    pub fn hook_fns<I: ?Sized + 'static, O: 'static>(&self, hook: &Hook<I, O>) -> &[HookFn<I, O>] {
        self.hooks
            .get(hook.id)
            .and_then(|h| h.downcast_ref::<HookFns<I, O>>())
            .map_or(&[], |h| &h.0)
    }
    pub fn contributions<P: 'static>(&self, slot: &Slot<P>) -> &[Contribution<P>] {
        self.slots
            .get(slot.id)
            .and_then(|s| s.downcast_ref::<Contributions<P>>())
            .map_or(&[], |c| &c.0)
    }
    fn contributions_mut<P: 'static>(
        &mut self,
        slot: &'static Slot<P>,
    ) -> &mut Vec<Contribution<P>> {
        &mut self
            .slots
            .entry(slot.id)
            .or_insert_with(|| Box::new(Contributions::<P>(Vec::new())))
            .downcast_mut::<Contributions<P>>()
            .expect("slot props type")
            .0
    }
    /// The contribution `plugin/id` in `slot`. It may be missing only when
    /// its plugin is off; a typo or a renamed contribution is reported.
    fn target<P: 'static>(
        &mut self,
        slot: &'static Slot<P>,
        contribution: &str,
    ) -> impl Iterator<Item = &mut Contribution<P>> {
        let owner = contribution.split('/').next().unwrap_or_default();
        let found = self
            .contributions(slot)
            .iter()
            .any(|c| c.id == contribution);
        if !found && self.plugin_index(owner).is_some() {
            let ids: Vec<&str> = self
                .contributions(slot)
                .iter()
                .map(|c| c.id.as_str())
                .collect();
            panic!(
                "no contribution `{contribution}` in zone `{}`; it has {ids:?}",
                slot.id
            );
        }
        self.contributions_mut(slot)
            .iter_mut()
            .filter(move |c| c.id == contribution)
    }
    pub fn plugin_index(&self, id: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.id == id)
    }
    fn declare(&mut self, name: &'static str, plugin: Option<usize>) {
        if let Some(p) = plugin {
            let id = self.plugins[p].id;
            assert!(
                name.strip_prefix(id).is_some_and(|r| r.starts_with('.')),
                "state {name} must start with `{id}.`"
            );
        }
        assert!(
            self.owners.insert(name, plugin).is_none(),
            "state {name} is declared twice"
        );
    }
    pub(crate) fn finish(&mut self) {
        for (i, (table, state)) in self.tables.iter().enumerate() {
            let owner = self.owners[state].map_or("kernel", |p| self.plugins[p].id);
            let prefix = owner.replace('-', "_");
            assert!(
                table
                    .strip_prefix(&prefix)
                    .is_some_and(|r| r.is_empty() || r.starts_with('_')),
                "table {table} of {state} must start with `{prefix}`"
            );
            if let Some((_, other)) = self.tables[..i].iter().find(|(t, _)| t == table) {
                panic!(
                    "lists {other} and {state} both store rows in table `{table}`: give one its own with `p.list(..).table(\"…\")`"
                );
            }
        }
        for edit in std::mem::take(&mut self.slot_edits) {
            edit(self);
        }
        for (i, page) in self.pages.iter().enumerate() {
            assert!(
                self.pages[..i].iter().all(|p| p.id != page.id),
                "{} registers two pages: a plugin has at most one",
                page.id
            );
        }
        // Grouped like Settings → Plugins (categories in plugin order), then by `order`.
        let plugins = &self.plugins;
        let group = |p: &PageEntry| {
            let category = plugins[p.plugin].category.id;
            plugins.iter().position(|pl| pl.category.id == category)
        };
        self.pages.sort_by_key(|p| (group(p), p.spec.order));
    }
}

/// Registration context handed to a plugin's `setup`.
pub struct Setup<'a> {
    pub(crate) reg: &'a mut Registry,
    pub(crate) plugin: Option<usize>,
}

impl<'a> Setup<'a> {
    pub(crate) fn me(&self) -> usize {
        self.plugin.expect("kernel registrations use internal APIs")
    }
    /// A zone this plugin may fill: the shell's, its own or one of a plugin it
    /// `requires` (the zone id starts with the owner's id, like state names).
    pub(crate) fn check_zone(&self, zone: &str) {
        let owner = zone.split('.').next().unwrap_or_default();
        let me = self.reg.plugins[self.me()];
        assert!(
            owner == "shell" || owner == me.id || me.requires.iter().any(|r| r.id == owner),
            "{} fills `{zone}`, but `{owner}` is not in its `requires`",
            me.id
        );
    }
    pub fn plugin_id(&self) -> &'static str {
        self.plugin.map_or("kernel", |p| self.reg.plugins[p].id)
    }
    /// This plugin's own folder (`<data>/plugins/<id>`, created on first use)
    /// for files it must keep outside SQLite: certificates, caches, journals.
    pub fn data_dir(&self) -> std::path::PathBuf {
        let dir = self.reg.data_dir.join("plugins").join(self.plugin_id());
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    // ---- state ----------------------------------------------------------
    /// An ordered, persisted list with ready-made Add/Save/Move/Remove ops.
    pub fn list<T: Record + DeserializeOwned>(
        &mut self,
        s: &'static State<Vec<T>>,
    ) -> list::ListSpec<'_, 'a, T> {
        self.reg.declare(s.name, self.plugin);
        self.reg
            .persist
            .push((s.name, self.plugin, state::list::<T>(s.name, true, T::NAME)));
        self.reg.tables.push((T::NAME, s.name));
        list::register(self, s)
    }
    /// See [`list::ListSpec::table`].
    pub(crate) fn list_table<T: Record + DeserializeOwned>(
        &mut self,
        s: &'static State<Vec<T>>,
        table: &'static str,
    ) {
        for (name, _, persist) in self
            .reg
            .persist
            .iter_mut()
            .filter(|(name, ..)| *name == s.name)
        {
            *persist = state::list::<T>(name, true, table);
        }
        for entry in self
            .reg
            .tables
            .iter_mut()
            .filter(|(_, state)| *state == s.name)
        {
            entry.0 = table;
        }
    }
    /// A persisted collection in insertion order (no position, no default ops).
    pub fn collection<T: Record + DeserializeOwned>(&mut self, s: &'static State<Vec<T>>) {
        self.reg.declare(s.name, self.plugin);
        self.reg.persist.push((
            s.name,
            self.plugin,
            state::list::<T>(s.name, false, T::NAME),
        ));
        self.reg.tables.push((T::NAME, s.name));
    }
    /// A persisted value (stored as JSON), `Default` on first start.
    pub fn setting<T: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static>(
        &mut self,
        s: &'static State<T>,
    ) {
        self.reg.declare(s.name, self.plugin);
        self.reg
            .persist
            .push((s.name, self.plugin, state::setting::<T>(s.name)));
    }
    /// A persisted setting the UI may change directly with
    /// [`UiCx::update`] (or the [`Set`](crate::Set) / [`Update`](crate::Update) ops).
    pub fn editable_setting<
        T: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static,
    >(
        &mut self,
        s: &'static State<T>,
    ) {
        self.setting(s);
        self.handle(|cx: Cx, op: crate::Set<T>| async move {
            cx.write(|w| {
                w.set(op.state, op.value);
                Ok(())
            })
            .await
        })
        .claim(move |_, op| (op.state.name == s.name).then_some(Claim::OWNED));
        self.handle(|cx: Cx, op: crate::Update<T>| async move {
            cx.write(|w| {
                w.update(op.state, op.change);
                Ok(())
            })
            .await
        })
        .claim(move |_, op| (op.state.name == s.name).then_some(Claim::OWNED));
    }
    /// Live state: readings that change often and are never saved (traffic,
    /// what is being checked right now). No transaction and no core config:
    /// `cx.set_live` / `cx.update_live` replace it and repaint the UI; read it
    /// with `cx.live(&S)` (or `snap.live`). Not available to `p.core`.
    pub fn live<T: Send + Sync + 'static>(
        &mut self,
        s: &'static State<T>,
        init: impl Fn() -> T + Send + Sync + 'static,
    ) {
        self.reg.declare(s.name, self.plugin);
        self.reg.live.push((
            s.name,
            Box::new(move || Arc::new(crate::state::Live::new(init())) as Value),
        ));
    }
    /// Delete child rows whose parent row disappeared, in the same transaction.
    pub fn cascade<P: Record, C: Record>(
        &mut self,
        parent: &'static State<Vec<P>>,
        child: &'static State<Vec<C>>,
        key: fn(&C) -> Id,
    ) {
        self.reg.cascades.push(Arc::new(move |base, w| {
            let (Some(before), true) = (base.try_get(parent), base.try_get(child).is_some()) else {
                return;
            };
            if !w.changed.contains_key(parent.name) {
                return;
            }
            let after = w.get(parent);
            let alive: std::collections::HashSet<Id> = after.iter().map(|p| p.id()).collect();
            let gone = before.iter().any(|p| !alive.contains(&p.id()));
            if gone && w.get(child).iter().any(|c| !alive.contains(&key(c))) {
                let plugin = w.plugin;
                w.plugin = None;
                w.update(child, |rows| rows.retain(|c| alive.contains(&key(c))));
                w.plugin = plugin;
            }
        }));
    }

    // ---- behaviour ------------------------------------------------------
    /// Implement an op. Runs at [`Rank::Default`] unless changed.
    pub fn handle<O: Op, F, Fut>(&mut self, f: F) -> HandlerSpec<'_, O>
    where
        F: Fn(Cx, O) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O::Reply>> + Send + 'static,
    {
        self.chain(
            Rank::Default,
            Arc::new(move |cx, op, _next| Box::pin(f(cx, op))),
        )
    }
    /// Put a handler in front of the existing ones. Call `next.run(&cx, op)` to continue.
    pub fn wrap<O: Op, F, Fut>(&mut self, f: F) -> HandlerSpec<'_, O>
    where
        F: Fn(Cx, O, Next<O>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O::Reply>> + Send + 'static,
    {
        self.chain(
            Rank::Specific,
            Arc::new(move |cx, op, next| Box::pin(f(cx, op, next))),
        )
    }
    fn chain<O: Op>(&mut self, rank: Rank, f: crate::op::HandlerFn<O>) -> HandlerSpec<'_, O> {
        let plugin = self.me();
        self.reg.seq += 1;
        let seq = self.reg.seq;
        let list = self
            .reg
            .ops
            .entry(TypeId::of::<O>())
            .or_insert_with(|| Box::new(Handlers::<O>(Vec::new())))
            .downcast_mut::<Handlers<O>>()
            .unwrap();
        list.0.push(Handler {
            f,
            claim: None,
            rank,
            plugin,
            seq,
        });
        let index = list.0.len() - 1;
        HandlerSpec {
            list: &mut list.0,
            index,
        }
    }
    /// Observe an event (after commit). Every event runs the handler at once
    /// (see [`ObserverSpec`] for other policies). Errors are shown as
    /// notifications; runs still going are cancelled when the plugin stops.
    pub fn on<E: Clone + Send + Sync + 'static, F, Fut>(&mut self, f: F) -> ObserverSpec<'_>
    where
        F: Fn(Cx, E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let plugin = self.plugin.unwrap_or(usize::MAX);
        let list = self.reg.observers.entry(TypeId::of::<E>()).or_default();
        list.push(Observer {
            plugin,
            f: Arc::new(move |cx, e| {
                e.downcast_ref::<E>()
                    .map(|e| Box::pin(f(cx, e.clone())) as BoxFut<()>)
            }),
            policy: Policy::Parallel,
        });
        ObserverSpec {
            observer: list.last_mut().expect("just pushed"),
        }
    }
    /// Run `f` with the new key after a commit that changed `key(snapshot)`,
    /// e.g. `p.watch(|s| s.get(&RULES), reload)`. The key at start is the baseline.
    pub fn watch<K, F, Fut>(
        &mut self,
        key: impl Fn(&Snapshot) -> K + Send + Sync + 'static,
        f: F,
    ) -> ObserverSpec<'_>
    where
        K: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(Cx, K) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let plugin = self.plugin.unwrap_or(usize::MAX);
        let last = Arc::new(std::sync::Mutex::new(None::<K>));
        let key = Arc::new(key);
        let (k, l) = (key.clone(), last.clone());
        self.reg
            .observers
            .entry(TypeId::of::<crate::Started>())
            .or_default()
            .push(Observer {
                plugin,
                f: Arc::new(move |cx, _| {
                    l.lock().unwrap().get_or_insert_with(|| k(&cx.snapshot()));
                    None
                }),
                policy: Policy::Parallel,
            });
        let f = Arc::new(f);
        let list = self
            .reg
            .observers
            .entry(TypeId::of::<crate::Changed>())
            .or_default();
        list.push(Observer {
            plugin,
            f: Arc::new(move |cx, _| {
                let now = key(&cx.snapshot());
                let mut last = last.lock().unwrap();
                // Unknown baseline (a commit before `Started`): take it, run nothing.
                let changed = last.as_ref().is_some_and(|l| *l != now);
                *last = Some(now.clone());
                changed.then(|| Box::pin(f(cx, now)) as BoxFut<()>)
            }),
            policy: Policy::Parallel,
        });
        ObserverSpec {
            observer: list.last_mut().expect("just pushed"),
        }
    }
    /// Answer a hook.
    pub fn hook<I: ?Sized + 'static, O: 'static>(
        &mut self,
        hook: &'static Hook<I, O>,
        f: impl Fn(&Snapshot, &I) -> O + Send + Sync + 'static,
    ) {
        if hook.exclusive
            && let Some(other) = self.reg.deciders.insert(hook.id, self.plugin)
            && other != self.plugin
        {
            self.reg.deciders.insert(hook.id, other);
            let name = |p: Option<usize>| p.map_or("Kernel", |p| self.reg.plugins[p].name);
            let warning = format!(
                "{} and {} both decide `{}`; {} is ignored. Turn one of them off.",
                name(other),
                name(self.plugin),
                hook.id,
                name(self.plugin),
            );
            self.reg.warnings.push(("Plugins", warning));
            return;
        }
        let fns = self
            .reg
            .hooks
            .entry(hook.id)
            .or_insert_with(|| Box::new(HookFns::<I, O>(Vec::new())));
        let Some(fns) = fns.downcast_mut::<HookFns<I, O>>() else {
            let warning = format!(
                "{} answers `{}` with other types than its owner; the answer is ignored.",
                self.plugin.map_or("Kernel", |p| self.reg.plugins[p].name),
                hook.id,
            );
            self.reg.warnings.push(("Plugins", warning));
            return;
        };
        fns.0.push(Arc::new(f));
    }
    /// Contribute a typed fragment of the mihomo configuration, recomputed after every commit.
    pub fn core(&mut self, f: impl Fn(&Snapshot) -> Fragment + Send + Sync + 'static) {
        let me = self.me();
        self.reg.fragments.push((me, Arc::new(f)));
    }
    /// Start `op` periodically. `every` is re-read after each commit; `None` pauses it.
    pub fn every<O: Op>(
        &mut self,
        name: &str,
        every: impl Fn(&Snapshot) -> Option<Duration> + Send + Sync + 'static,
        make: impl Fn() -> O + Send + Sync + 'static,
    ) {
        let plugin = self.me();
        let name = format!("{}/{name}", self.plugin_id());
        let make = Arc::new(make);
        let fallback = name.clone();
        let last_error = Arc::new(std::sync::Mutex::new(None));
        self.reg.schedules.push(crate::setup::Schedule {
            plugin,
            name,
            // A zero period means the schedule is off.
            every: Arc::new(move |s| every(s).filter(|d| !d.is_zero())),
            task: Arc::new(move |cx| {
                let make = make.clone();
                let op = make();
                let spec = op
                    .job()
                    .unwrap_or_else(|| crate::JobSpec::new(fallback.clone()));
                cx.task_with(
                    cx.job_key(&op, &spec.key),
                    spec,
                    Some(last_error.clone()),
                    move || make(),
                )
            }),
        });
    }

    /// Run a long-lived task (a server, a watcher) while `spec` returns `Some`.
    /// After each commit the spec is recomputed; when it changes the task is
    /// stopped and started again with the new spec. Kernel shutdown stops it.
    /// An error ends the task and is shown as a notice.
    pub fn service<S, F, Fut>(
        &mut self,
        name: &str,
        spec: impl Fn(&Snapshot) -> Option<S> + Send + Sync + 'static,
        run: F,
    ) -> ServiceSetup<'_, S>
    where
        S: Clone + PartialEq + Send + Sync + 'static,
        F: Fn(Cx, S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let plugin = self.me();
        let name = format!("{}/{name}", self.plugin_id());
        self.reg.services.push(Service {
            plugin,
            name,
            spec: Arc::new(move |snap| {
                spec(snap).map(|s| Arc::new(s) as Arc<dyn Any + Send + Sync>)
            }),
            same: Arc::new(|a, b| {
                a.downcast_ref::<S>()
                    .is_some_and(|a| b.downcast_ref::<S>() == Some(a))
            }),
            run: Arc::new(move |cx, s| {
                let s = s.downcast_ref::<S>().expect("service spec type").clone();
                Box::pin(run(cx, s))
            }),
            stop: None,
            restart: false,
        });
        ServiceSetup {
            service: self.reg.services.last_mut().expect("just pushed"),
            _spec: std::marker::PhantomData,
        }
    }

    /// Run a long-lived task (a follower, a poller) for the plugin's whole
    /// life: a [`Self::service`] whose spec never changes.
    pub fn task<F, Fut>(&mut self, name: &str, run: F) -> ServiceSetup<'_, ()>
    where
        F: Fn(Cx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.service(name, |_| Some(()), move |cx, ()| run(cx))
    }

    // ---- UI ---------------------------------------------------------------
    /// Add a contribution to a zone. `order` sorts contributions (lower first).
    pub fn fill<P: 'static>(
        &mut self,
        slot: &'static Slot<P>,
        id: &str,
        order: i32,
        f: impl Fn(&mut egui::Ui, &mut UiCx, &P) + Send + Sync + 'static,
    ) {
        let plugin = self.me();
        self.check_zone(slot.id);
        let id = format!("{}/{id}", self.plugin_id());
        self.reg.fills.push((slot.id, plugin));
        let list = self.reg.contributions_mut(slot);
        list.push(Contribution {
            id,
            plugin,
            order,
            view: Arc::new(f),
        });
        list.sort_by_key(|c| (c.order, c.plugin));
    }
    /// Replace another plugin's contribution (`"plugin/id"`).
    pub fn replace<P: 'static>(
        &mut self,
        slot: &'static Slot<P>,
        contribution: &'static str,
        f: impl Fn(&mut egui::Ui, &mut UiCx, &P) + Send + Sync + 'static,
    ) {
        let view: ViewFn<P> = Arc::new(f);
        self.reg.slot_edits.push(Box::new(move |reg| {
            for c in reg.target(slot, contribution) {
                c.view = view.clone();
            }
        }));
    }
    /// Wrap another plugin's contribution: `f(inner, ui, cx, props)`.
    pub fn wrap_view<P: 'static>(
        &mut self,
        slot: &'static Slot<P>,
        contribution: &'static str,
        f: impl Fn(&dyn Fn(&mut egui::Ui, &mut UiCx, &P), &mut egui::Ui, &mut UiCx, &P)
        + Send
        + Sync
        + 'static,
    ) {
        let f = Arc::new(f);
        self.reg.slot_edits.push(Box::new(move |reg| {
            for c in reg.target(slot, contribution) {
                let inner = c.view.clone();
                let f = f.clone();
                c.view = Arc::new(move |ui, cx, p| f(&|ui, cx, p| inner(ui, cx, p), ui, cx, p));
            }
        }));
    }
    /// Hide another plugin's contribution.
    pub fn hide<P: 'static>(&mut self, slot: &'static Slot<P>, contribution: &'static str) {
        self.reg.slot_edits.push(Box::new(move |reg| {
            let _ = reg.target(slot, contribution).count();
            reg.contributions_mut(slot).retain(|c| c.id != contribution);
        }));
    }
    /// The plugin's page, with its own per-session state.
    pub fn page<T: Page + Default + 'static>(&mut self, spec: PageSpec) {
        let (plugin, id) = (self.me(), self.plugin_id());
        self.reg.pages.push(PageEntry {
            plugin,
            id,
            spec,
            make: Arc::new(|| Box::new(T::default())),
        });
    }
    /// The plugin's page, stateless.
    pub fn page_fn(
        &mut self,
        spec: PageSpec,
        f: impl Fn(&mut egui::Ui, &mut UiCx) + Send + Sync + 'static,
    ) {
        let (plugin, id) = (self.me(), self.plugin_id());
        let f = Arc::new(f);
        self.reg.pages.push(PageEntry {
            plugin,
            id,
            spec,
            make: Arc::new(move || {
                let f = f.clone();
                Box::new(move |ui: &mut egui::Ui, cx: &mut UiCx| f(ui, cx))
            }),
        });
    }
}

/// Fine-tune a just-registered handler.
pub struct HandlerSpec<'r, O: Op> {
    list: &'r mut Vec<Handler<O>>,
    index: usize,
}
impl<O: Op> HandlerSpec<'_, O> {
    /// Only run for inputs this returns `Some` for; higher weight first.
    pub fn claim(self, f: impl Fn(&Snapshot, &O) -> Option<Claim> + Send + Sync + 'static) -> Self {
        self.list[self.index].claim = Some(Arc::new(f));
        self
    }
    pub fn rank(self, rank: Rank) -> Self {
        self.list[self.index].rank = rank;
        self
    }
}
