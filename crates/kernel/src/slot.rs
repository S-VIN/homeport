//! UI zones and synchronous questions that several plugins can answer.
use crate::{ui::UiCx, Snapshot};
use std::{marker::PhantomData, sync::Arc};

/// A place in the UI that plugins fill. `P` is what the zone passes to each
/// contribution (e.g. the server a row shows). The kernel's shell defines the
/// window zones ([`crate::shell`]); plugins define zones on their own screens.
pub struct Slot<P: 'static> {
    pub id: &'static str,
    /// One line for documentation: where the zone is and what belongs there.
    pub about: &'static str,
    _p: PhantomData<fn(&P)>,
}
impl<P> Slot<P> {
    pub const fn new(id: &'static str, about: &'static str) -> Self {
        Self { id, about, _p: PhantomData }
    }
}

pub(crate) type ViewFn<P> = Arc<dyn Fn(&mut egui::Ui, &mut UiCx, &P) + Send + Sync>;

pub(crate) struct Contribution<P> {
    /// `plugin/local`
    pub id: String,
    pub plugin: usize,
    pub order: i32,
    pub view: ViewFn<P>,
}
impl<P> Clone for Contribution<P> {
    fn clone(&self) -> Self {
        Self { id: self.id.clone(), plugin: self.plugin, order: self.order, view: self.view.clone() }
    }
}
pub(crate) struct Contributions<P>(pub Vec<Contribution<P>>);

/// A synchronous question every interested plugin answers, e.g.
/// "why is this server excluded?". Answers are collected in plugin order.
pub struct Hook<I: ?Sized + 'static, O: 'static> {
    pub id: &'static str,
    _p: PhantomData<fn(&I) -> O>,
}
impl<I: ?Sized, O> Hook<I, O> {
    pub const fn new(id: &'static str) -> Self {
        Self { id, _p: PhantomData }
    }
}
pub(crate) type HookFn<I, O> = Arc<dyn Fn(&Snapshot, &I) -> O + Send + Sync>;
pub(crate) struct HookFns<I: ?Sized, O>(pub Vec<HookFn<I, O>>);
