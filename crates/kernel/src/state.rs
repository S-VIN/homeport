//! Typed state cells, immutable snapshots and copy-on-write transactions.
use crate::{
    setup::Registry,
    store::{Store, Table, Tx},
    Id, Kernel, Result,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{any::Any, collections::HashMap, marker::PhantomData, sync::Arc};

/// A typed key into the shared state. Declare it as a `pub static` so other
/// plugins can read it; only the owning plugin may write it.
///
/// ```ignore
/// pub static RULES: State<Vec<Rule>> = State::new("routing.rules");
/// ```
pub struct State<T: 'static> {
    pub name: &'static str,
    _t: PhantomData<fn() -> T>,
}
impl<T> State<T> {
    /// The name must start with the owning plugin id and a dot.
    pub const fn new(name: &'static str) -> Self {
        Self { name, _t: PhantomData }
    }
}

/// A persisted row with a stable [`Id`]: derive [`Table`](crate::Table) plus `Clone, PartialEq`.
pub trait Record: Table<Key = Id> + Clone + PartialEq + Send + Sync + 'static {
    fn id(&self) -> Id {
        *self.key()
    }
}
impl<T: Table<Key = Id> + Clone + PartialEq + Send + Sync + 'static> Record for T {}

pub(crate) type Value = Arc<dyn Any + Send + Sync>;

#[derive(Clone)]
pub(crate) struct Cell {
    pub value: Value,
}

/// A live state's cell: one per kernel, shared by every snapshot, replaced in place.
pub(crate) struct Live<T>(pub(crate) std::sync::RwLock<Arc<T>>);
impl<T> Live<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(std::sync::RwLock::new(Arc::new(value)))
    }
}

/// An immutable view of all state at one revision. Cheap to clone.
#[derive(Clone)]
pub struct Snapshot {
    pub(crate) cells: Arc<HashMap<&'static str, Cell>>,
    pub(crate) reg: Arc<Registry>,
    pub(crate) rev: u64,
}
impl Snapshot {
    /// Read a state cell. Panics if its plugin is not enabled — list it in `requires`.
    pub fn get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.try_get(s).unwrap_or_else(|| {
            panic!("state {} is not available; is its plugin enabled and listed in `requires`?", s.name)
        })
    }
    /// Read a state cell of an optional plugin.
    pub fn try_get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Option<Arc<T>> {
        self.cells.get(s.name).map(|c| {
            c.value
                .clone()
                .downcast::<T>()
                .unwrap_or_else(|_| panic!("state {} read with the wrong type (a live state is read with `live`)", s.name))
        })
    }
    /// The latest value of a live state (see [`Setup::live`](crate::Setup::live)).
    pub fn live<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.live_cell(s).0.read().unwrap().clone()
    }
    pub(crate) fn live_cell<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<Live<T>> {
        self.get(&State::<Live<T>>::new(s.name))
    }
    pub fn rev(&self) -> u64 {
        self.rev
    }
    /// Ask every plugin that answers a [`Hook`](crate::Hook).
    pub fn hooks<I: ?Sized + 'static, O: 'static>(&self, hook: &crate::Hook<I, O>, input: &I) -> Vec<O> {
        self.reg.hook_fns(hook).iter().map(|f| f(self, input)).collect()
    }
    pub(crate) fn with(&self, changed: HashMap<&'static str, Value>) -> Snapshot {
        let rev = self.rev + 1;
        let mut cells = (*self.cells).clone();
        for (k, v) in changed {
            cells.insert(k, Cell { value: v });
        }
        Snapshot { cells: Arc::new(cells), reg: self.reg.clone(), rev }
    }
}

pub(crate) type PendingEvent = Box<dyn FnOnce(&Arc<Kernel>) + Send>;

/// A transaction over the latest snapshot. Changes become visible to everyone
/// at once after the core accepted the new configuration and SQLite committed.
pub struct Write<'a> {
    base: &'a Snapshot,
    pub(crate) changed: HashMap<&'static str, Box<dyn Any + Send + Sync>>,
    pub(crate) events: Vec<PendingEvent>,
    pub(crate) plugin: Option<usize>,
}
impl<'a> Write<'a> {
    pub(crate) fn new(base: &'a Snapshot, plugin: Option<usize>) -> Self {
        Self { base, changed: HashMap::new(), events: Vec::new(), plugin }
    }
    /// The snapshot this transaction started from.
    pub fn base(&self) -> &Snapshot {
        self.base
    }
    /// Read a cell, including changes made earlier in this transaction.
    pub fn get<T: Clone + Send + Sync + 'static>(&self, s: &State<T>) -> std::borrow::Cow<'_, T> {
        match self.changed.get(s.name) {
            Some(v) => std::borrow::Cow::Borrowed(v.downcast_ref::<T>().expect("state type")),
            None => std::borrow::Cow::Owned((*self.base.get(s)).clone()),
        }
    }
    /// Modify a cell this plugin owns.
    pub fn update<T: Clone + Send + Sync + 'static, R>(&mut self, s: &State<T>, f: impl FnOnce(&mut T) -> R) -> R {
        self.check(s.name);
        let base = self.base;
        let entry = self
            .changed
            .entry(s.name)
            .or_insert_with(|| Box::new((*base.get(s)).clone()));
        f(entry.downcast_mut::<T>().expect("state type"))
    }
    pub fn set<T: Clone + Send + Sync + 'static>(&mut self, s: &State<T>, value: T) {
        self.check(s.name);
        self.changed.insert(s.name, Box::new(value));
    }
    /// Deliver an event to observers after this transaction commits.
    pub fn emit<E: Clone + Send + Sync + 'static>(&mut self, event: E) {
        self.events.push(Box::new(move |k| k.dispatch(event)));
    }
    fn check(&self, name: &str) {
        if let Some(me) = self.plugin {
            let owner = self.base.reg.owner(name);
            if owner != Some(me) {
                panic!(
                    "plugin {} cannot write state {name}; call an Op of its owner instead",
                    self.base.reg.plugins[me].id
                );
            }
        }
    }
}

/// Emitted after every commit with the names of the changed cells.
#[derive(Clone, Debug)]
pub struct Changed {
    pub states: Arc<Vec<&'static str>>,
}

// ---- persistence ----------------------------------------------------------

pub(crate) struct Persist {
    pub register: Box<dyn Fn(&Store) -> Result<()> + Send + Sync>,
    pub load: Box<dyn Fn(&Tx<'_>) -> Result<Value> + Send + Sync>,
    pub save: Box<dyn Fn(&Tx<'_>, &Value, &Value) -> Result<()> + Send + Sync>,
}

#[derive(crate::Table, Serialize, Deserialize, Clone, PartialEq)]
#[table(name = "kernel_order")]
pub(crate) struct OrderRow {
    pub id: String,
    pub list: String,
    pub position: u32,
}
#[derive(crate::Table, Serialize, Deserialize, Clone, PartialEq)]
#[table(name = "kernel_settings")]
pub(crate) struct SettingRow {
    pub id: String,
    pub json: String,
}
#[derive(crate::Table, Serialize, Deserialize, Clone, PartialEq)]
#[table(name = "kernel_plugins")]
pub(crate) struct PluginRow {
    pub id: String,
    pub enabled: bool,
}

fn typed<T: 'static>(v: &Value) -> &T {
    v.downcast_ref::<T>().expect("persisted state type")
}

fn sync_rows<T: Record>(tx: &Tx<'_>, before: &[T], after: &[T]) -> Result<()> {
    let mut old: HashMap<Id, &T> = before.iter().map(|v| (v.id(), v)).collect();
    for v in after {
        if old.remove(&v.id()) != Some(v) {
            tx.put(v)?;
        }
    }
    for id in old.keys() {
        tx.delete::<T>(id)?;
    }
    Ok(())
}

pub(crate) fn list<T: Record + DeserializeOwned>(name: &'static str, ordered: bool) -> Persist {
    Persist {
        register: Box::new(|s| s.register::<T>()),
        load: Box::new(move |tx| {
            let mut rows: Vec<T> = tx.all()?;
            if ordered {
                let pos: HashMap<String, u32> = tx
                    .all::<OrderRow>()?
                    .into_iter()
                    .filter(|r| r.list == name)
                    .map(|r| (r.id, r.position))
                    .collect();
                rows.sort_by_key(|r| pos.get(&format!("{name}|{}", r.id())).copied().unwrap_or(u32::MAX));
            }
            Ok(Arc::new(rows) as Value)
        }),
        save: Box::new(move |tx, before, after| {
            let (before, after) = (typed::<Vec<T>>(before), typed::<Vec<T>>(after));
            sync_rows(tx, before, after)?;
            if ordered {
                let ids = |v: &[T]| v.iter().map(|r| r.id()).collect::<Vec<_>>();
                if ids(before) != ids(after) {
                    for r in before {
                        tx.delete::<OrderRow>(format!("{name}|{}", r.id()).as_str())?;
                    }
                    for (i, r) in after.iter().enumerate() {
                        tx.put(&OrderRow { id: format!("{name}|{}", r.id()), list: name.into(), position: i as u32 })?;
                    }
                }
            }
            Ok(())
        }),
    }
}

pub(crate) fn setting<T: Serialize + DeserializeOwned + Default + Send + Sync + 'static>(name: &'static str) -> Persist {
    Persist {
        register: Box::new(|_| Ok(())),
        load: Box::new(move |tx| {
            let value: T = match tx.get::<SettingRow>(name)? {
                Some(row) => serde_json::from_str(&row.json)
                    .map_err(|_| crate::Error::msg(format!("Invalid saved setting: {name}")))?,
                None => T::default(),
            };
            Ok(Arc::new(value) as Value)
        }),
        save: Box::new(move |tx, _, after| {
            let json = serde_json::to_string(typed::<T>(after)).map_err(|e| crate::Error::msg(e.to_string()))?;
            tx.put(&SettingRow { id: name.into(), json })
        }),
    }
}
