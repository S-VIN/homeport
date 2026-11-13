//! Registration: the only API a plugin's `setup` function uses.
use crate::{
    list,
    mihomo::Fragment,
    op::{BoxFut, Handler, Handlers},
    slot::{Contribution, Contributions, HookFn, HookFns, ViewFn},
    state::{self, Persist, Value},
    ui::{Page, PageSpec, UiCx},
    Claim, Cx, Hook, Id, Next, Op, Plugin, Rank, Record, Result, Slot, Snapshot, State,
};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    any::{Any, TypeId},
    collections::HashMap,
    future::Future,
    sync::Arc,
    time::Duration,
};

pub(crate) type ObserverFn = Arc<dyn Fn(Cx, &(dyn Any + Send + Sync)) -> Option<BoxFut<()>> + Send + Sync>;
pub(crate) struct Observer {
    pub plugin: usize,
    pub f: ObserverFn,
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
}

/// Returned by [`Setup::service`] to add a cleanup step.
pub struct ServiceSetup<'a, S> {
    service: &'a mut Service,
    _spec: std::marker::PhantomData<fn(S)>,
}
impl<S: Clone + Send + Sync + 'static> ServiceSetup<'_, S> {
    /// Undo what a run did, after it stopped: its spec changed, it was turned
    /// off, or the app quits. The next run starts only after this finished.
    pub fn on_stop<F, Fut>(self, f: F)
    where
        F: Fn(Cx, S) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.service.stop = Some(Arc::new(move |cx, s| {
            let s = s.downcast_ref::<S>().expect("service spec type").clone();
            Box::pin(f(cx, s))
        }));
    }
}
pub(crate) struct PageEntry {
    pub plugin: usize,
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
    pub memory: Vec<(&'static str, Box<dyn Fn() -> Value + Send + Sync>)>,
    pub ops: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    pub observers: HashMap<TypeId, Vec<Observer>>,
    pub hooks: HashMap<&'static str, Box<dyn Any + Send + Sync>>,
    pub slots: HashMap<&'static str, Box<dyn Any + Send + Sync>>,
    pub slot_edits: Vec<SlotEdit>,
    /// `(slot id, plugin)` of every contribution: where a plugin shows its UI.
    pub fills: Vec<(&'static str, usize)>,
    pub pages: Vec<PageEntry>,
    pub fragments: Vec<(usize, Arc<dyn Fn(&Snapshot) -> Fragment + Send + Sync>)>,
    pub schedules: Vec<Schedule>,
    pub services: Vec<Service>,
    pub cascades: Vec<Cascade>,
    pub migrations: Vec<(usize, Box<dyn Fn(&crate::store::rusqlite::Connection) -> Result<()> + Send + Sync>)>,
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
    fn contributions_mut<P: 'static>(&mut self, slot: &'static Slot<P>) -> &mut Vec<Contribution<P>> {
        &mut self
            .slots
            .entry(slot.id)
            .or_insert_with(|| Box::new(Contributions::<P>(Vec::new())))
            .downcast_mut::<Contributions<P>>()
            .expect("slot props type")
            .0
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
        assert!(self.owners.insert(name, plugin).is_none(), "state {name} is declared twice");
    }
    pub(crate) fn finish(&mut self) {
        for edit in std::mem::take(&mut self.slot_edits) {
            edit(self);
        }
        self.pages.sort_by_key(|p| p.spec.order);
    }
}

/// Registration context handed to a plugin's `setup`.
pub struct Setup<'a> {
    pub(crate) reg: &'a mut Registry,
    pub(crate) plugin: Option<usize>,
}

impl<'a> Setup<'a> {
    fn me(&self) -> usize {
        self.plugin.expect("kernel registrations use internal APIs")
    }
    pub fn plugin_id(&self) -> &'static str {
        self.plugin.map_or("kernel", |p| self.reg.plugins[p].id)
    }
    /// The app's data folder, for files a plugin must keep outside SQLite
    /// (certificates, caches). Use a subfolder named after the plugin id.
    pub fn data_dir(&self) -> std::path::PathBuf {
        self.reg.data_dir.clone()
    }
    /// True if another plugin is enabled (for optional integrations).
    pub fn enabled(&self, plugin: &str) -> bool {
        self.reg.plugin_index(plugin).is_some()
    }

    // ---- state ----------------------------------------------------------
    /// An ordered, persisted list with ready-made Add/Save/Move/Remove ops.
    pub fn list<T: Record + DeserializeOwned>(&mut self, s: &'static State<Vec<T>>) -> list::ListSpec<'_, 'a, T> {
        self.reg.declare(s.name, self.plugin);
        self.reg.persist.push((s.name, self.plugin, state::list::<T>(s.name, true)));
        list::register(self, s)
    }
    /// A persisted collection in insertion order (no position, no default ops).
    pub fn collection<T: Record + DeserializeOwned>(&mut self, s: &'static State<Vec<T>>) {
        self.reg.declare(s.name, self.plugin);
        self.reg.persist.push((s.name, self.plugin, state::list::<T>(s.name, false)));
    }
    /// A persisted value (stored as JSON), `Default` on first start.
    pub fn setting<T: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static>(&mut self, s: &'static State<T>) {
        self.reg.declare(s.name, self.plugin);
        self.reg.persist.push((s.name, self.plugin, state::setting::<T>(s.name)));
    }
    /// A persisted setting the UI may change directly with [`Set`](crate::Set)`(value)`.
    /// Use one distinct type per editable setting.
    pub fn editable_setting<T: Serialize + DeserializeOwned + Clone + Default + Send + Sync + 'static>(&mut self, s: &'static State<T>) {
        self.setting(s);
        self.handle(move |cx: Cx, crate::Set(value): crate::Set<T>| async move {
            cx.write(|w| {
                w.set(s, value);
                Ok(())
            })
            .await
        });
        self.handle(move |cx: Cx, crate::Update(f): crate::Update<T>| async move {
            cx.write(|w| {
                w.update(s, f);
                Ok(())
            })
            .await
        });
    }
    /// Memory-only state (errors, live readings), reset on restart.
    pub fn memory<T: Clone + Send + Sync + 'static>(&mut self, s: &'static State<T>, init: impl Fn() -> T + Send + Sync + 'static) {
        self.reg.declare(s.name, self.plugin);
        self.reg.memory.push((s.name, Box::new(move || Arc::new(init()) as Value)));
    }
    /// Live state: readings that change often and are never saved (traffic,
    /// what is being checked right now). No transaction and no core config:
    /// `cx.set_live` / `cx.update_live` replace it and repaint the UI; read it
    /// with `cx.live(&S)` (or `snap.live`). Not available to `p.core`.
    pub fn live<T: Send + Sync + 'static>(&mut self, s: &'static State<T>, init: impl Fn() -> T + Send + Sync + 'static) {
        self.reg.declare(s.name, self.plugin);
        self.reg.memory.push((s.name, Box::new(move || Arc::new(crate::state::Live::new(init())) as Value)));
    }
    /// Custom persistence for data that does not fit one table.
    pub fn persist<T: Send + Sync + 'static>(
        &mut self,
        s: &'static State<T>,
        register: impl Fn(&crate::store::Store) -> Result<()> + Send + Sync + 'static,
        load: impl Fn(&crate::store::Tx<'_>) -> Result<T> + Send + Sync + 'static,
        save: impl Fn(&crate::store::Tx<'_>, &T, &T) -> Result<()> + Send + Sync + 'static,
    ) {
        self.reg.declare(s.name, self.plugin);
        self.reg.persist.push((
            s.name,
            self.plugin,
            Persist {
                register: Box::new(register),
                load: Box::new(move |tx| load(tx).map(|v| Arc::new(v) as Value)),
                save: Box::new(move |tx, a, b| save(tx, a.downcast_ref().unwrap(), b.downcast_ref().unwrap())),
            },
        ));
    }
    /// Run raw SQL once at startup before state is loaded (schema fixes).
    pub fn migrate(&mut self, f: impl Fn(&crate::store::rusqlite::Connection) -> Result<()> + Send + Sync + 'static) {
        let me = self.me();
        self.reg.migrations.push((me, Box::new(f)));
    }
    /// Delete child rows whose parent row disappeared, in the same transaction.
    pub fn cascade<P: Record, C: Record>(&mut self, parent: &'static State<Vec<P>>, child: &'static State<Vec<C>>, key: fn(&C) -> Id) {
        self.reg.cascades.push(Arc::new(move |base, w| {
            let (Some(before), true) = (base.try_get(parent), base.try_get(child).is_some()) else { return };
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
        self.chain(Rank::Default, Arc::new(move |cx, op, _next| Box::pin(f(cx, op))))
    }
    /// Put a handler in front of the existing ones. Call `next.run(&cx, op)` to continue.
    pub fn wrap<O: Op, F, Fut>(&mut self, f: F) -> HandlerSpec<'_, O>
    where
        F: Fn(Cx, O, Next<O>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<O::Reply>> + Send + 'static,
    {
        self.chain(Rank::Specific, Arc::new(move |cx, op, next| Box::pin(f(cx, op, next))))
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
        list.0.push(Handler { f, claim: None, rank, plugin, seq });
        let index = list.0.len() - 1;
        HandlerSpec { list: &mut list.0, index }
    }
    /// Observe an event (after commit). Errors are shown as notifications.
    pub fn on<E: Clone + Send + Sync + 'static, F, Fut>(&mut self, f: F)
    where
        F: Fn(Cx, E) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let plugin = self.plugin.unwrap_or(usize::MAX);
        self.reg.observers.entry(TypeId::of::<E>()).or_default().push(Observer {
            plugin,
            f: Arc::new(move |cx, e| e.downcast_ref::<E>().map(|e| Box::pin(f(cx, e.clone())) as BoxFut<()>)),
        });
    }
    /// Observe commits that touched any of the given states, e.g. `[RULES.name]`.
    pub fn on_change<F, Fut>(&mut self, states: impl IntoIterator<Item = &'static str>, f: F)
    where
        F: Fn(Cx) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        let states: Arc<Vec<&'static str>> = Arc::new(states.into_iter().collect());
        let f = Arc::new(f);
        self.on(move |cx, e: crate::Changed| {
            let hit = e.states.iter().any(|s| states.contains(s));
            let f = f.clone();
            async move {
                if hit {
                    f(cx).await
                } else {
                    Ok(())
                }
            }
        });
    }
    /// Answer a hook.
    pub fn hook<I: ?Sized + 'static, O: 'static>(&mut self, hook: &'static Hook<I, O>, f: impl Fn(&Snapshot, &I) -> O + Send + Sync + 'static) {
        self.reg
            .hooks
            .entry(hook.id)
            .or_insert_with(|| Box::new(HookFns::<I, O>(Vec::new())))
            .downcast_mut::<HookFns<I, O>>()
            .expect("hook types")
            .0
            .push(Arc::new(f));
    }
    /// Contribute a typed fragment of the mihomo configuration, recomputed after every commit.
    pub fn core(&mut self, f: impl Fn(&Snapshot) -> Fragment + Send + Sync + 'static) {
        let me = self.me();
        self.reg.fragments.push((me, Arc::new(f)));
    }
    /// Start `op` periodically. `every` is re-read after each commit; `None` pauses it.
    pub fn every<O: Op>(&mut self, name: &str, every: impl Fn(&Snapshot) -> Option<Duration> + Send + Sync + 'static, make: impl Fn() -> O + Send + Sync + 'static) {
        let plugin = self.me();
        let name = format!("{}/{name}", self.plugin_id());
        let make = Arc::new(make);
        let fallback = name.clone();
        let last_error = Arc::new(std::sync::Mutex::new(None));
        self.reg.schedules.push(crate::setup::Schedule {
            plugin,
            name,
            every: Arc::new(every),
            task: Arc::new(move |cx| {
                let make = make.clone();
                cx.task_with(make().job().unwrap_or_else(|| crate::JobSpec::new(fallback.clone())), Some(last_error.clone()), move || make())
            }),
        });
    }

    /// Run a long-lived task (a server, a watcher) while `spec` returns `Some`.
    /// After each commit the spec is recomputed; when it changes the task is
    /// stopped and started again with the new spec. Kernel shutdown stops it.
    /// An error ends the task and is shown as a notice.
    pub fn service<S, F, Fut>(&mut self, name: &str, spec: impl Fn(&Snapshot) -> Option<S> + Send + Sync + 'static, run: F) -> ServiceSetup<'_, S>
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
            spec: Arc::new(move |snap| spec(snap).map(|s| Arc::new(s) as Arc<dyn Any + Send + Sync>)),
            same: Arc::new(|a, b| a.downcast_ref::<S>().is_some_and(|a| b.downcast_ref::<S>() == Some(a))),
            run: Arc::new(move |cx, s| {
                let s = s.downcast_ref::<S>().expect("service spec type").clone();
                Box::pin(run(cx, s))
            }),
            stop: None,
        });
        ServiceSetup { service: self.reg.services.last_mut().expect("just pushed"), _spec: std::marker::PhantomData }
    }

    // ---- UI ---------------------------------------------------------------
    /// Add a contribution to a zone. `order` sorts contributions (lower first).
    pub fn fill<P: 'static>(&mut self, slot: &'static Slot<P>, id: &str, order: i32, f: impl Fn(&mut egui::Ui, &mut UiCx, &P) + Send + Sync + 'static) {
        let plugin = self.me();
        let id = format!("{}/{id}", self.plugin_id());
        self.reg.fills.push((slot.id, plugin));
        let list = self.reg.contributions_mut(slot);
        list.push(Contribution { id, plugin, order, view: Arc::new(f) });
        list.sort_by_key(|c| (c.order, c.plugin));
    }
    /// Replace another plugin's contribution (`"plugin/id"`).
    pub fn replace<P: 'static>(&mut self, slot: &'static Slot<P>, contribution: &'static str, f: impl Fn(&mut egui::Ui, &mut UiCx, &P) + Send + Sync + 'static) {
        let view: ViewFn<P> = Arc::new(f);
        self.reg.slot_edits.push(Box::new(move |reg| {
            for c in reg.contributions_mut(slot).iter_mut().filter(|c| c.id == contribution) {
                c.view = view.clone();
            }
        }));
    }
    /// Wrap another plugin's contribution: `f(inner, ui, cx, props)`.
    pub fn wrap_view<P: 'static>(&mut self, slot: &'static Slot<P>, contribution: &'static str, f: impl Fn(&dyn Fn(&mut egui::Ui, &mut UiCx, &P), &mut egui::Ui, &mut UiCx, &P) + Send + Sync + 'static) {
        let f = Arc::new(f);
        self.reg.slot_edits.push(Box::new(move |reg| {
            for c in reg.contributions_mut(slot).iter_mut().filter(|c| c.id == contribution) {
                let inner = c.view.clone();
                let f = f.clone();
                c.view = Arc::new(move |ui, cx, p| f(&|ui, cx, p| inner(ui, cx, p), ui, cx, p));
            }
        }));
    }
    /// Hide another plugin's contribution.
    pub fn hide<P: 'static>(&mut self, slot: &'static Slot<P>, contribution: &'static str) {
        self.reg.slot_edits.push(Box::new(move |reg| {
            reg.contributions_mut(slot).retain(|c| c.id != contribution);
        }));
    }
    /// A page in the bottom navigation with its own per-session state.
    pub fn page<T: Page + Default + 'static>(&mut self, spec: PageSpec) {
        let plugin = self.me();
        self.reg.pages.push(PageEntry { plugin, spec, make: Arc::new(|| Box::new(T::default())) });
    }
    /// A stateless page.
    pub fn page_fn(&mut self, spec: PageSpec, f: impl Fn(&mut egui::Ui, &mut UiCx) + Send + Sync + 'static) {
        let plugin = self.me();
        let f = Arc::new(f);
        self.reg.pages.push(PageEntry {
            plugin,
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
