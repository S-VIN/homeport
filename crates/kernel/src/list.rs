//! Ready-made operations for ordered lists registered with [`Setup::list`].
//! Other plugins can wrap them like any op (e.g. load a subscription right after `Add`).
use crate::{setup::Setup, Cx, Id, Op, Record, Result, Snapshot, State};
use std::{marker::PhantomData, sync::Arc, sync::RwLock};

/// Append (or insert at `at`) a new item. Replies with its id.
pub struct Add<T> {
    pub item: T,
    pub at: Option<usize>,
}
impl<T> Add<T> {
    pub fn new(item: T) -> Self {
        Self { item, at: None }
    }
}
/// Replace an existing item with the same id; position is kept.
pub struct Save<T>(pub T);
/// Move an item one step up or down.
pub struct Move<T> {
    pub id: Id,
    pub up: bool,
    _t: PhantomData<fn() -> T>,
}
impl<T> Move<T> {
    pub fn new(id: Id, up: bool) -> Self {
        Self { id, up, _t: PhantomData }
    }
}
/// Delete an item.
pub struct Remove<T> {
    pub id: Id,
    _t: PhantomData<fn() -> T>,
}
impl<T> Remove<T> {
    pub fn new(id: Id) -> Self {
        Self { id, _t: PhantomData }
    }
}
impl<T: Record> Op for Add<T> {
    type Reply = Id;
}
impl<T: Record> Op for Save<T> {
    type Reply = ();
}
impl<T: Record> Op for Move<T> {
    type Reply = ();
}
impl<T: Record> Op for Remove<T> {
    type Reply = ();
}

/// Events emitted by the default handlers after commit.
#[derive(Clone)]
pub struct Added<T> {
    pub item: T,
}
#[derive(Clone)]
pub struct Saved<T> {
    pub before: T,
    pub item: T,
}
#[derive(Clone)]
pub struct Removed<T> {
    pub item: T,
}

/// `(snapshot, item, previous version, other items)`.
type Validator<T> = Arc<dyn Fn(&Snapshot, &mut T, Option<&T>, &[T]) -> Result<()> + Send + Sync>;

/// Returned by [`Setup::list`] to attach validation.
pub struct ListSpec<'s, 'a, T: Record> {
    _setup: &'s mut Setup<'a>,
    validator: Arc<RwLock<Option<Validator<T>>>>,
}
impl<T: Record> ListSpec<'_, '_, T> {
    /// Check and normalize an item before Add/Save. `previous` is the stored
    /// version (None when adding); `others` excludes the item itself.
    pub fn validate(self, f: impl Fn(&Snapshot, &mut T, Option<&T>, &[T]) -> Result<()> + Send + Sync + 'static) -> Self {
        *self.validator.write().unwrap() = Some(Arc::new(f));
        self
    }
}

fn check<T: Record>(v: &RwLock<Option<Validator<T>>>, snap: &Snapshot, item: &mut T, all: &[T]) -> Result<()> {
    let others: Vec<T> = all.iter().filter(|o| o.id() != item.id()).cloned().collect();
    let previous = all.iter().find(|o| o.id() == item.id()).cloned();
    match &*v.read().unwrap() {
        Some(f) => f(snap, item, previous.as_ref(), &others),
        None => Ok(()),
    }
}

pub(crate) fn register<'s, 'a, T: Record>(setup: &'s mut Setup<'a>, s: &'static State<Vec<T>>) -> ListSpec<'s, 'a, T> {
    let validator: Arc<RwLock<Option<Validator<T>>>> = Arc::default();
    let v = validator.clone();
    setup.handle(move |cx: Cx, op: Add<T>| {
        let v = v.clone();
        async move {
            let mut item = op.item;
            cx.write(|w| {
                let all = w.get(s).into_owned();
                check(&v, w.base(), &mut item, &all)?;
                if all.iter().any(|o| o.id() == item.id()) {
                    crate::bail!("This item already exists.");
                }
                let id = item.id();
                w.update(s, |rows| rows.insert(op.at.unwrap_or(rows.len()).min(rows.len()), item.clone()));
                w.emit(Added { item });
                Ok(id)
            })
            .await
        }
    });
    let v = validator.clone();
    setup.handle(move |cx: Cx, Save(mut item): Save<T>| {
        let v = v.clone();
        async move {
            cx.write(|w| {
                let all = w.get(s).into_owned();
                check(&v, w.base(), &mut item, &all)?;
                let Some(i) = all.iter().position(|o| o.id() == item.id()) else {
                    crate::bail!("This item no longer exists.");
                };
                if all[i] != item {
                    let before = all[i].clone();
                    w.update(s, |rows| rows[i] = item.clone());
                    w.emit(Saved { before, item });
                }
                Ok(())
            })
            .await
        }
    });
    setup.handle(move |cx: Cx, op: Move<T>| async move {
        cx.write(|w| {
            let all = w.get(s).into_owned();
            let Some(i) = all.iter().position(|o| o.id() == op.id) else {
                crate::bail!("This item no longer exists.");
            };
            let j = if op.up { i.saturating_sub(1) } else { (i + 1).min(all.len() - 1) };
            if i != j {
                w.update(s, |rows| rows.swap(i, j));
            }
            Ok(())
        })
        .await
    });
    setup.handle(move |cx: Cx, op: Remove<T>| async move {
        cx.write(|w| {
            let all = w.get(s).into_owned();
            let Some(i) = all.iter().position(|o| o.id() == op.id) else {
                crate::bail!("This item no longer exists.");
            };
            let item = all[i].clone();
            w.update(s, |rows| rows.remove(i));
            w.emit(Removed { item });
            Ok(())
        })
        .await
    });
    ListSpec { _setup: setup, validator }
}
