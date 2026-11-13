//! UI side of a plugin: ordinary egui code with direct access to state and ops.
//! The same code draws the desktop window and every browser session.
mod list_editor;
pub use list_editor::{Column, ListEditor};

use crate::{
    core::CoreStatus, kit::Icon, slot::Slot, Kernel, Op, Run, RunId, Snapshot, State,
};
use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex},
};

/// Where a page appears in the bottom navigation.
#[derive(Clone, Copy)]
pub struct PageSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: Icon,
    /// Lower comes first. Core pages use 10…60; Settings is always last.
    pub order: i32,
}

/// A page with per-session state.
pub trait Page: Send {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx);
}
impl<F: FnMut(&mut egui::Ui, &mut UiCx) + Send> Page for F {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        self(ui, cx)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ticket {
    /// Generation of the request; a newer submit under the same key wins.
    Pending(u64),
    Done(Result<(), String>),
}

/// What a toast's button does in the session that shows it (e.g. Undo).
pub(crate) type ToastAction = Box<dyn FnOnce(&mut UiCx) + Send>;
/// A toast in one session: a kernel notice or a local message with an action.
pub(crate) struct Toast {
    pub notice: crate::Notice,
    pub at: f64,
    pub action: Option<(String, ToastAction)>,
}

/// Which status chip opened the panels popover, and where it is.
#[derive(Clone, Copy)]
pub(crate) struct PanelAnchor {
    pub plugin: Option<usize>,
    pub rect: egui::Rect,
}

/// Per window / browser tab state owned by the shell.
pub(crate) struct SessionState {
    pub ctx: egui::Context,
    pub page: &'static str,
    pub pages: HashMap<&'static str, Box<dyn Page>>,
    pub tickets: Arc<Mutex<HashMap<egui::Id, (Ticket, f64)>>>,
    pub saved_at: Option<f64>,
    pub toasts: Vec<Toast>,
    pub last_notice: u64,
    pub panels_open: bool,
    pub panel_anchor: Option<PanelAnchor>,
    pub activity_open: bool,
    pub theme: Option<crate::kernel::Theme>,
    pub now: f64,
    pub wide: bool,
    /// Height of the toast strip in the last frame (its room is reserved first).
    pub toast_height: f32,
    /// Where the Activity chip was drawn (the popover anchors to it).
    pub activity_chip: Option<egui::Rect>,
}
impl SessionState {
    pub fn new(ctx: egui::Context, k: &Kernel) -> Self {
        Self {
            ctx,
            page: k.reg.pages.first().map_or("settings", |p| p.spec.id),
            pages: HashMap::new(),
            tickets: Arc::default(),
            saved_at: None,
            toasts: Vec::new(),
            last_notice: k.last_notice(),
            panels_open: false,
            panel_anchor: None,
            activity_open: false,
            theme: None,
            now: 0.,
            wide: false,
            toast_height: 0.,
            activity_chip: None,
        }
    }
    pub fn pending_since(&self) -> Option<f64> {
        self.tickets
            .lock()
            .unwrap()
            .values()
            .filter(|(t, _)| matches!(t, Ticket::Pending(_)))
            .map(|(_, at)| *at)
            .reduce(f64::min)
    }
}

/// What plugin UI code gets each frame.
pub struct UiCx<'a> {
    pub(crate) k: &'a Arc<Kernel>,
    pub(crate) s: &'a mut SessionState,
    pub(crate) snap: Snapshot,
    pub(crate) plugin: Option<usize>,
}

impl<'a> UiCx<'a> {
    pub(crate) fn new(k: &'a Arc<Kernel>, s: &'a mut SessionState) -> Self {
        Self { snap: k.snapshot(), k, s, plugin: None }
    }
    pub(crate) fn scoped<R>(&mut self, plugin: Option<usize>, f: impl FnOnce(&mut UiCx) -> R) -> R {
        let prev = std::mem::replace(&mut self.plugin, plugin);
        let r = f(self);
        self.plugin = prev;
        r
    }
    pub fn kernel(&self) -> &Arc<Kernel> {
        self.k
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snap
    }
    pub fn get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.snap.get(s)
    }
    pub fn try_get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Option<Arc<T>> {
        self.snap.try_get(s)
    }
    fn cx(&self) -> crate::Cx {
        crate::Cx { k: self.k.clone(), plugin: self.plugin, job: None }
    }

    /// Run an op in the background, fire and forget. Ops with a [`JobSpec`](crate::JobSpec)
    /// become jobs; errors appear as notifications.
    pub fn run<O: Op>(&mut self, op: O) {
        let cx = self.cx();
        if op.job().is_some() {
            if let Err(e) = cx.spawn(op) {
                self.k.notify_error("Could not start", &e.message);
            }
            return;
        }
        let k = self.k.clone();
        let ctx = self.s.ctx.clone();
        tokio::spawn(async move {
            if let Err(e) = cx.call(op).await {
                k.notify_error(cx.plugin_name(), &e.message);
            }
            ctx.request_repaint();
        });
    }
    /// Run an op and remember its outcome under `key` (a widget, a row…).
    /// While it runs the status bar shows "Saving…"; read the outcome with [`Self::outcome`].
    pub fn submit<O: Op>(&mut self, key: impl Hash + std::fmt::Debug, op: O) {
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = egui::Id::unique(key);
        let tickets = self.s.tickets.clone();
        tickets.lock().unwrap().insert(id, (Ticket::Pending(generation), self.s.now));
        let cx = self.cx();
        let ctx = self.s.ctx.clone();
        tokio::spawn(async move {
            let r = cx.call(op).await.map(drop).map_err(|e| e.message);
            let now = ctx.input(|i| i.time);
            let mut t = tickets.lock().unwrap();
            if matches!(t.get(&id), Some((Ticket::Pending(g), _)) if *g == generation) {
                t.insert(id, (Ticket::Done(r), now));
            }
            drop(t);
            ctx.request_repaint();
        });
    }
    /// Change some fields of a setting registered with `Setup::editable_setting`.
    pub fn update<T: Send + Sync + 'static>(&mut self, key: impl Hash + std::fmt::Debug, _state: &'static State<T>, f: impl FnOnce(&mut T) + Send + 'static) {
        self.submit(key, crate::Update::<T>(Box::new(f)));
    }
    /// True while a submitted op runs.
    pub fn pending(&self, key: impl Hash + std::fmt::Debug) -> bool {
        matches!(self.s.tickets.lock().unwrap().get(&egui::Id::unique(key)), Some((Ticket::Pending(_), _)))
    }
    /// Take the finished outcome of a submitted op once.
    pub fn outcome(&mut self, key: impl Hash + std::fmt::Debug) -> Option<Result<(), String>> {
        let id = egui::Id::unique(key);
        let mut t = self.s.tickets.lock().unwrap();
        match t.get(&id) {
            Some((Ticket::Done(_), _)) => match t.remove(&id) {
                Some((Ticket::Done(r), _)) => {
                    if r.is_ok() {
                        self.s.saved_at = Some(self.s.now);
                    }
                    Some(r)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The active job with this key (see [`crate::JobSpec`]).
    pub fn job(&self, key: &str) -> Option<Run> {
        self.k.jobs.board().runs.into_iter().find(|r| r.key == key && r.phase.active())
    }
    /// The active job that `op` would join, if any.
    pub fn job_for<O: Op>(&self, op: &O) -> Option<Run> {
        op.job().and_then(|j| self.job(&j.key))
    }
    pub fn cancel(&mut self, run: RunId) {
        let _ = self.k.jobs.cancel(run);
    }
    /// A button that starts `op` as a job and turns into Stop with progress while it runs.
    pub fn job_button<O: Op>(&mut self, ui: &mut egui::Ui, icon: Icon, idle: &str, width: f32, enabled: bool, make: impl FnOnce() -> O) {
        let op = make();
        let key = op.job().map(|j| j.key).unwrap_or_default();
        let run = self.job(&key);
        if crate::kit::task_button(ui, icon, idle, width, run.as_ref().map(|r| (r.done, r.total)), enabled).clicked() {
            match run {
                Some(r) => self.cancel(r.id),
                None => self.run(op),
            }
        }
    }
    pub fn notify(&mut self, text: &str) {
        self.k.notify(self.cx().plugin_name(), text)
    }
    pub fn notify_error(&mut self, text: &str) {
        self.k.notify_error(self.cx().plugin_name(), text)
    }
    /// Switch the session to another page.
    pub fn navigate(&mut self, page: &'static str) {
        self.s.page = page;
    }
    /// The page currently shown.
    pub fn page(&self) -> &'static str {
        self.s.page
    }
    /// Open or close the panels popover (narrow windows) with the calling
    /// plugin's panel (all panels when it has none). Chips use [`Self::toggle_panels_from`].
    pub fn toggle_panels(&mut self) {
        self.toggle_panels_at(None);
    }
    fn toggle_panels_at(&mut self, rect: Option<egui::Rect>) {
        let same = self.s.panel_anchor.is_some_and(|a| a.plugin == self.plugin);
        if self.s.panels_open && same {
            self.s.panels_open = false;
            return;
        }
        self.s.panels_open = true;
        self.s.activity_open = false;
        self.s.panel_anchor = Some(PanelAnchor { plugin: self.plugin, rect: rect.unwrap_or(egui::Rect::NOTHING) });
    }
    /// [`Self::toggle_panels`] for a chip's response: the popover anchors to it.
    pub fn toggle_panels_from(&mut self, chip: &egui::Response) {
        self.toggle_panels_at(Some(chip.rect));
    }
    /// A toast in this session with an Undo button; `undo` runs when it is clicked.
    /// Use it after deleting or excluding something instead of asking first.
    pub fn undo(&mut self, title: &str, text: &str, undo: impl FnOnce(&mut UiCx) + Send + 'static) {
        let notice = crate::Notice { id: self.k.next_notice_id(), title: title.into(), text: text.into(), error: false, page: None };
        self.s.toasts.push(Toast { notice, at: self.s.now, action: Some(("Undo".into(), Box::new(undo))) });
    }
    pub fn wide(&self) -> bool {
        self.s.wide
    }
    pub fn core_status(&self) -> CoreStatus {
        self.k.core.status()
    }
    /// Draw every contribution to a zone.
    pub fn slot<P: 'static>(&mut self, ui: &mut egui::Ui, slot: &Slot<P>, props: &P) {
        let list: Vec<_> = self.k.reg.contributions(slot).to_vec();
        for c in list {
            ui.push_id(&c.id, |ui| self.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, props)));
        }
    }
    /// Draw contributions last-to-first (for right-to-left layouts, so they still read in order).
    pub fn slot_rev<P: 'static>(&mut self, ui: &mut egui::Ui, slot: &Slot<P>, props: &P) {
        let list: Vec<_> = self.k.reg.contributions(slot).to_vec();
        for c in list.into_iter().rev() {
            ui.push_id(&c.id, |ui| self.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, props)));
        }
    }
    /// Draw contributions with a callback around each (e.g. columns of a row).
    pub fn slot_each<P: 'static>(&mut self, ui: &mut egui::Ui, slot: &Slot<P>, props: &P, mut each: impl FnMut(&mut egui::Ui, &mut UiCx, &dyn Fn(&mut egui::Ui, &mut UiCx))) {
        let list: Vec<_> = self.k.reg.contributions(slot).to_vec();
        for c in list {
            ui.push_id(&c.id, |ui| {
                self.scoped(Some(c.plugin), |cx| each(ui, cx, &|ui, cx| (c.view)(ui, cx, props)))
            });
        }
    }
    /// Number of contributions to a zone.
    pub fn slot_len<P: 'static>(&self, slot: &Slot<P>) -> usize {
        self.k.reg.contributions(slot).len()
    }
}
