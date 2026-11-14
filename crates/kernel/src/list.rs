//! Ready-made operations for ordered lists registered with [`Setup::list`].
//! Every op and event names its list, so two lists of one record type never mix.
//! Other plugins can wrap them like any op (e.g. load a subscription right after `Add`).
use crate::{Claim, Cx, Id, Op, Record, Result, Snapshot, State, setup::Setup};
use std::sync::{Arc, RwLock};

/// A list registered with [`Setup::list`].
pub type List<T> = &'static State<Vec<T>>;

/// Append (or insert at `at`) a new item. Replies with its id.
pub struct Add<T: 'static> {
    pub list: List<T>,
    pub item: T,
    pub at: Option<usize>,
}
impl<T> Add<T> {
    pub fn new(list: List<T>, item: T) -> Self {
        Self {
            list,
            item,
            at: None,
        }
    }
}
/// Replace an existing item with the same id; position is kept.
pub struct Save<T: 'static> {
    pub list: List<T>,
    pub item: T,
}
impl<T> Save<T> {
    pub fn new(list: List<T>, item: T) -> Self {
        Self { list, item }
    }
}
/// Change some fields of an item inside the transaction: fields changed
/// meanwhile by someone else keep their new values.
pub struct Edit<T: 'static> {
    pub list: List<T>,
    pub id: Id,
    pub change: Box<dyn FnOnce(&mut T) + Send>,
}
impl<T> Edit<T> {
    pub fn new(list: List<T>, id: Id, change: impl FnOnce(&mut T) + Send + 'static) -> Self {
        Self {
            list,
            id,
            change: Box::new(change),
        }
    }
}
/// Move an item one step up or down.
pub struct Move<T: 'static> {
    pub list: List<T>,
    pub id: Id,
    pub up: bool,
}
impl<T> Move<T> {
    pub fn new(list: List<T>, id: Id, up: bool) -> Self {
        Self { list, id, up }
    }
}
/// Delete an item.
pub struct Remove<T: 'static> {
    pub list: List<T>,
    pub id: Id,
}
impl<T> Remove<T> {
    pub fn new(list: List<T>, id: Id) -> Self {
        Self { list, id }
    }
}
impl<T: Record> Op for Add<T> {
    type Reply = Id;
}
impl<T: Record> Op for Save<T> {
    type Reply = ();
}
impl<T: Record> Op for Edit<T> {
    type Reply = ();
}
impl<T: Record> Op for Move<T> {
    type Reply = ();
}
impl<T: Record> Op for Remove<T> {
    type Reply = ();
}

/// Events emitted by the default handlers after commit. Observers of a type
/// used by several lists check `list`.
#[derive(Clone)]
pub struct Added<T: 'static> {
    pub list: List<T>,
    pub item: T,
}
#[derive(Clone)]
pub struct Saved<T: 'static> {
    pub list: List<T>,
    pub before: T,
    pub item: T,
}
#[derive(Clone)]
pub struct Removed<T: 'static> {
    pub list: List<T>,
    pub item: T,
}

/// `(snapshot, item, previous version, other items)`.
type Validator<T> = Arc<dyn Fn(&Snapshot, &mut T, Option<&T>, &[T]) -> Result<()> + Send + Sync>;

/// Returned by [`Setup::list`] to attach validation.
pub struct ListSpec<'s, 'a, T: Record> {
    setup: &'s mut Setup<'a>,
    list: List<T>,
    validator: Arc<RwLock<Option<Validator<T>>>>,
}
impl<T: Record + serde::de::DeserializeOwned> ListSpec<'_, '_, T> {
    /// Keep the rows in `table` instead of the record type's own table: needed
    /// for a second list of the same record type.
    pub fn table(self, table: &'static str) -> Self {
        self.setup.list_table::<T>(self.list, table);
        self
    }
    /// Check and normalize an item before Add/Save. `previous` is the stored
    /// version (None when adding); `others` excludes the item itself.
    pub fn validate(
        self,
        f: impl Fn(&Snapshot, &mut T, Option<&T>, &[T]) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        *self.validator.write().unwrap() = Some(Arc::new(f));
        self
    }
}

fn check<T: Record>(
    v: &RwLock<Option<Validator<T>>>,
    snap: &Snapshot,
    item: &mut T,
    all: &[T],
) -> Result<()> {
    let others: Vec<T> = all
        .iter()
        .filter(|o| o.id() != item.id())
        .cloned()
        .collect();
    let previous = all.iter().find(|o| o.id() == item.id()).cloned();
    match &*v.read().unwrap() {
        Some(f) => f(snap, item, previous.as_ref(), &others),
        None => Ok(()),
    }
}

pub(crate) fn register<'s, 'a, T: Record>(
    setup: &'s mut Setup<'a>,
    list: List<T>,
) -> ListSpec<'s, 'a, T> {
    let validator: Arc<RwLock<Option<Validator<T>>>> = Arc::default();
    let mine = move |l: List<T>| (l.name == list.name).then_some(Claim::OWNED);
    let v = validator.clone();
    setup
        .handle(move |cx: Cx, op: Add<T>| {
            let v = v.clone();
            async move {
                let mut item = op.item;
                cx.write(|w| {
                    let all = w.get(list).into_owned();
                    check(&v, w.base(), &mut item, &all)?;
                    if all.iter().any(|o| o.id() == item.id()) {
                        crate::bail!("This item already exists.");
                    }
                    let id = item.id();
                    w.update(list, |rows| {
                        rows.insert(op.at.unwrap_or(rows.len()).min(rows.len()), item.clone())
                    });
                    w.emit(Added { list, item });
                    Ok(id)
                })
                .await
            }
        })
        .claim(move |_, op| mine(op.list));
    let v = validator.clone();
    setup
        .handle(move |cx: Cx, op: Save<T>| {
            let v = v.clone();
            async move { save(&cx, &v, list, op.item.id(), |item| *item = op.item).await }
        })
        .claim(move |_, op| mine(op.list));
    let v = validator.clone();
    setup
        .handle(move |cx: Cx, op: Edit<T>| {
            let v = v.clone();
            async move { save(&cx, &v, list, op.id, op.change).await }
        })
        .claim(move |_, op| mine(op.list));
    setup
        .handle(move |cx: Cx, op: Move<T>| async move {
            cx.write(|w| {
                let all = w.get(list).into_owned();
                let Some(i) = all.iter().position(|o| o.id() == op.id) else {
                    crate::bail!("This item no longer exists.");
                };
                let j = if op.up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(all.len() - 1)
                };
                if i != j {
                    w.update(list, |rows| rows.swap(i, j));
                }
                Ok(())
            })
            .await
        })
        .claim(move |_, op| mine(op.list));
    setup
        .handle(move |cx: Cx, op: Remove<T>| async move {
            cx.write(|w| {
                let all = w.get(list).into_owned();
                let Some(i) = all.iter().position(|o| o.id() == op.id) else {
                    crate::bail!("This item no longer exists.");
                };
                let item = all[i].clone();
                w.update(list, |rows| rows.remove(i));
                w.emit(Removed { list, item });
                Ok(())
            })
            .await
        })
        .claim(move |_, op| mine(op.list));
    ListSpec {
        setup,
        list,
        validator,
    }
}

/// Change the stored item `id` in one transaction, validate it, emit `Saved`.
async fn save<T: Record>(
    cx: &Cx,
    v: &RwLock<Option<Validator<T>>>,
    list: List<T>,
    id: Id,
    change: impl FnOnce(&mut T),
) -> Result<()> {
    cx.write(|w| {
        let all = w.get(list).into_owned();
        let Some(i) = all.iter().position(|o| o.id() == id) else {
            crate::bail!("This item no longer exists.");
        };
        let mut item = all[i].clone();
        change(&mut item);
        check(v, w.base(), &mut item, &all)?;
        if all[i] != item {
            let before = all[i].clone();
            w.update(list, |rows| rows[i] = item.clone());
            w.emit(Saved { list, before, item });
        }
        Ok(())
    })
    .await
}
