//! UI side of a plugin: ordinary egui code with direct access to state and ops.
//! The same code draws the desktop window and every browser session.
mod list_editor;
pub use list_editor::{Column, ListEditor};

use crate::{Kernel, Op, Run, RunId, Snapshot, State, core::CoreStatus, kit::Icon, slot::Slot};
use std::{
    collections::HashMap,
    hash::Hash,
    sync::{Arc, Mutex},
};

/// Where a plugin's page appears in the navigation. A plugin has at most one
/// page; its id is the plugin id ([`UiCx::navigate`], [`JobSpec::page`](crate::JobSpec::page)).
#[derive(Clone, Copy)]
pub struct PageSpec {
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

pub(crate) enum Ticket {
    /// `generation`: a newer submit under the same key wins. `save`: the op
    /// changes saved data ([`Op::SAVES`]), shown as "Saving…", then "Saved".
    Pending { generation: u64, save: bool },
    /// The op's reply (its `Op::Reply`) or error text.
    Done {
        result: Result<Box<dyn std::any::Any + Send>, String>,
        save: bool,
    },
}

/// What became of an op started with [`UiCx::submit`] (see [`UiCx::request`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Request<R> {
    /// Never submitted, or the result was taken.
    Idle,
    Pending,
    Done(Result<R, String>),
}

/// A kernel notice shown as a toast in one session.
pub(crate) struct Toast {
    pub notice: crate::Notice,
    pub at: f64,
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
    pub file_action: Option<crate::configuration::FileAction>,
    pub can_exit: bool,
    pub exit_requested: bool,
    pub file_busy: bool,
    pub incoming: Option<Arc<crate::configuration::Import>>,
    /// An imported file being validated off the UI thread.
    pub checking:
        Option<tokio::sync::oneshot::Receiver<crate::Result<Arc<crate::configuration::Import>>>>,
    pub page: &'static str,
    pub pages: HashMap<&'static str, Box<dyn Page>>,
    pub tickets: Arc<Mutex<HashMap<egui::Id, (Ticket, f64)>>>,
    pub toasts: Vec<Toast>,
    pub last_notice: u64,
    pub panels_open: bool,
    pub panel_anchor: Option<PanelAnchor>,
    pub activity_open: bool,
    /// Failed tasks already seen in Activity (more mark its button).
    pub seen_failures: usize,
    pub theme: Option<crate::kernel::Theme>,
    pub now: f64,
    pub wide: bool,
    /// Where the Activity chip was drawn (the popover anchors to it).
    pub activity_chip: Option<egui::Rect>,
    /// Status chips that didn't fit the bar (by contribution index): the "+N"
    /// popover shows them above the panels.
    pub folded: Vec<usize>,
    /// A save whose outcome a page already took keeps "Saved" on screen.
    last_saved: Option<f64>,
}
impl SessionState {
    pub fn new(ctx: egui::Context, k: &Kernel) -> Self {
        Self {
            ctx,
            file_action: None,
            can_exit: false,
            exit_requested: false,
            file_busy: false,
            incoming: None,
            checking: None,
            page: k.reg.pages.first().map_or("settings", |p| p.id),
            pages: HashMap::new(),
            tickets: Arc::default(),
            toasts: Vec::new(),
            last_notice: k.last_notice(),
            panels_open: false,
            panel_anchor: None,
            activity_open: false,
            seen_failures: 0,
            folded: Vec::new(),
            theme: None,
            now: 0.,
            wide: false,
            activity_chip: None,
            last_saved: None,
        }
    }
    /// When the last op that changed saved state finished ("Saved").
    pub fn saved_at(&self) -> Option<f64> {
        let tickets = self.tickets.lock().unwrap();
        let saves = tickets.values().filter_map(|(t, at)| {
            matches!(
                t,
                Ticket::Done {
                    result: Ok(_),
                    save: true
                }
            )
            .then_some(*at)
        });
        saves.chain(self.last_saved).reduce(f64::max)
    }
    pub fn pending_since(&self) -> Option<f64> {
        self.tickets
            .lock()
            .unwrap()
            .values()
            .filter(|(t, _)| matches!(t, Ticket::Pending { save: true, .. }))
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
        Self {
            snap: k.snapshot(),
            k,
            s,
            plugin: None,
        }
    }
    pub(crate) fn scoped<R>(&mut self, plugin: Option<usize>, f: impl FnOnce(&mut UiCx) -> R) -> R {
        let prev = std::mem::replace(&mut self.plugin, plugin);
        let r = f(self);
        self.plugin = prev;
        r
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snap
    }
    pub fn get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.snap.get(s)
    }
    /// The latest value of a live state (see [`Setup::live`](crate::Setup::live)).
    pub fn live<T: Send + Sync + 'static>(&self, s: &State<T>) -> Arc<T> {
        self.snap.live(s)
    }
    pub fn try_get<T: Send + Sync + 'static>(&self, s: &State<T>) -> Option<Arc<T>> {
        self.snap.try_get(s)
    }
    fn cx(&self) -> crate::Cx {
        crate::Cx {
            k: self.k.clone(),
            plugin: self.plugin,
            job: None,
        }
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
        let ctx = self.s.ctx.clone();
        let call = cx.call(op);
        drop(self.k.track(&cx, async move {
            let r = call.await.map(drop);
            ctx.request_repaint();
            r
        }));
    }
    /// Run an op and remember its outcome under `key` (a widget, a row…).
    /// While it runs the status bar shows "Saving…"; read the outcome with [`Self::outcome`].
    pub fn submit<O: Op>(&mut self, key: impl Hash + std::fmt::Debug, op: O) {
        static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = self.ticket(key);
        let tickets = self.s.tickets.clone();
        let save = O::SAVES;
        {
            let mut t = tickets.lock().unwrap();
            // Finished tickets nobody read (e.g. setting changes) expire.
            let now = self.s.now;
            t.retain(|_, (ticket, at)| matches!(ticket, Ticket::Pending { .. }) || now - *at < 60.);
            t.insert(id, (Ticket::Pending { generation, save }, now));
        }
        let cx = self.cx();
        let ctx = self.s.ctx.clone();
        let call = cx.call(op);
        drop(self.k.track(&cx, async move {
            // A crash still ends the request (no endless "Saving…").
            let r = crate::kernel::caught(call).await;
            let now = ctx.input(|i| i.time);
            let mut t = tickets.lock().unwrap();
            if matches!(t.get(&id), Some((Ticket::Pending { generation: g, .. }, _)) if *g == generation) {
                match r {
                    // Overtaken (a stale result, a newer request): nothing to show.
                    Err(e) if e.cancelled() => drop(t.remove(&id)),
                    r => {
                        let reply = r.map(|reply| Box::new(reply) as Box<dyn std::any::Any + Send>);
                        let result = reply.map_err(|e| e.message);
                        t.insert(id, (Ticket::Done { result, save }, now));
                    }
                }
            }
            drop(t);
            ctx.request_repaint();
            Ok(())
        }));
    }
    /// A settings row bound to one field of an editable setting: shows the
    /// stored value in `control` and saves the change.
    /// `cx.setting(ui, "Refresh subscriptions", &SETTINGS, |s| &mut s.refresh_secs, |ui, v| kit::interval(ui, "refresh", v))`
    pub fn setting<S: Clone + Send + Sync + 'static, V: Clone + Send + 'static>(
        &mut self,
        ui: &mut egui::Ui,
        label: &'static str,
        state: &'static State<S>,
        field: fn(&mut S) -> &mut V,
        control: impl FnOnce(&mut egui::Ui, &mut V) -> bool,
    ) {
        let mut value = field(&mut (*self.get(state)).clone()).clone();
        let mut changed = false;
        crate::kit::settings_row(ui, label, "", |ui| changed = control(ui, &mut value));
        if changed {
            self.update(label, state, move |s| *field(s) = value);
        }
    }
    /// Change some fields of a setting registered with `Setup::editable_setting`.
    pub fn update<T: Send + Sync + 'static>(
        &mut self,
        key: impl Hash + std::fmt::Debug,
        state: &'static State<T>,
        f: impl FnOnce(&mut T) + Send + 'static,
    ) {
        self.submit(
            key,
            crate::Update {
                state,
                change: Box::new(f),
            },
        );
    }
    /// Keys are local to the plugin: two plugins may both use `"save"`.
    fn ticket(&self, key: impl Hash + std::fmt::Debug) -> egui::Id {
        egui::Id::unique((self.plugin, key))
    }
    /// True while a submitted op runs.
    pub fn pending(&self, key: impl Hash + std::fmt::Debug) -> bool {
        matches!(
            self.s.tickets.lock().unwrap().get(&self.ticket(key)),
            Some((Ticket::Pending { .. }, _))
        )
    }
    /// The state of the op `O` submitted under `key`, with its reply
    /// (`cx.request::<CheckServer>(key)`). It stays until the next submit; an
    /// op that was overtaken (a stale result) leaves it `Idle`.
    pub fn request<O: crate::Op>(&self, key: impl Hash + std::fmt::Debug) -> Request<O::Reply>
    where
        O::Reply: Clone,
    {
        let label = format!("{key:?}");
        match self.s.tickets.lock().unwrap().get(&self.ticket(key)) {
            None => Request::Idle,
            Some((Ticket::Pending { .. }, _)) => Request::Pending,
            Some((Ticket::Done { result: Err(e), .. }, _)) => Request::Done(Err(e.clone())),
            Some((
                Ticket::Done {
                    result: Ok(reply), ..
                },
                _,
            )) => match reply.downcast_ref::<O::Reply>() {
                Some(r) => Request::Done(Ok(r.clone())),
                // The key was submitted with another op: a bug in the page,
                // which must not take the window down.
                None => Request::Done(Err(format!(
                    "bug: request {label} is not a {}",
                    std::any::type_name::<O>()
                ))),
            },
        }
    }
    /// Take the finished outcome of a submitted op once (for saves: the
    /// reply is not needed, only success or the error text).
    pub fn outcome(&mut self, key: impl Hash + std::fmt::Debug) -> Option<Result<(), String>> {
        let id = self.ticket(key);
        let mut t = self.s.tickets.lock().unwrap();
        if !matches!(t.get(&id), Some((Ticket::Done { .. }, _))) {
            return None;
        }
        let Some((Ticket::Done { result, save }, at)) = t.remove(&id) else {
            return None;
        };
        if save && result.is_ok() {
            self.s.last_saved = Some(self.s.last_saved.map_or(at, |last| last.max(at)));
        }
        Some(result.map(drop))
    }

    /// The active job that `op` would join, if any.
    pub fn job_for<O: Op>(&self, op: &O) -> Option<Run> {
        self.cx().running(op)
    }
    pub fn cancel(&mut self, run: RunId) {
        let _ = self.k.jobs.cancel(run);
    }
    /// A button that starts `op` as a job and turns into Stop with progress while it runs.
    pub fn job_button<O: Op>(
        &mut self,
        ui: &mut egui::Ui,
        icon: Icon,
        idle: &str,
        enabled: bool,
        make: impl FnOnce() -> O,
    ) -> egui::Response {
        let op = make();
        let run = self.job_for(&op);
        let r = crate::kit::task_button(
            ui,
            icon,
            idle,
            run.as_ref().map(|r| (r.done, r.total)),
            enabled,
        );
        if r.clicked() {
            match run {
                Some(r) => self.cancel(r.id),
                None => self.run(op),
            }
        }
        r
    }
    pub fn notify(&mut self, text: &str) {
        self.k.notify(self.cx().plugin_name(), text)
    }
    /// An error toast with the plugin's name, also written to the log.
    pub fn notify_error(&mut self, text: &str) {
        let cx = self.cx();
        self.k.notice(cx.plugin, cx.plugin_name(), text)
    }
    /// The header of the calling plugin's page: the title from its
    /// [`PageSpec`], a muted counter and actions on the right.
    pub fn page_header(
        &mut self,
        ui: &mut egui::Ui,
        counter: &str,
        actions: impl FnOnce(&mut egui::Ui, &mut UiCx),
    ) {
        let title = (self.k.reg.pages.iter())
            .find(|e| Some(e.plugin) == self.plugin)
            .map_or("", |e| e.spec.title);
        crate::kit::page_header(ui, title, counter, |ui| actions(ui, self));
    }
    /// Switch the session to `plugin`'s page.
    pub fn navigate(&mut self, plugin: &'static crate::Plugin) {
        self.go(plugin.id);
    }
    pub(crate) fn go(&mut self, page: &'static str) {
        self.s.page = page;
        self.s.panels_open = false;
        self.s.activity_open = false;
    }
    /// Whether `plugin`'s page is shown.
    pub fn on_page(&self, plugin: &crate::Plugin) -> bool {
        self.s.page == plugin.id
    }
    /// Open or close the panels popover (narrow windows) with the calling
    /// plugin's panel (all panels when it has none), anchored to `chip`.
    pub(crate) fn toggle_panels_from(&mut self, chip: &egui::Response) {
        let rect = chip.rect;
        let same = self.s.panel_anchor.is_some_and(|a| a.plugin == self.plugin);
        if self.s.panels_open && same {
            self.s.panels_open = false;
            return;
        }
        self.s.panels_open = true;
        self.s.activity_open = false;
        self.s.panel_anchor = Some(PanelAnchor {
            plugin: self.plugin,
            rect,
        });
    }
    /// A chip in the status bar. A click opens the plugin's panel under the
    /// chip in a narrow window, else its page (in a wide window the panels are
    /// beside the page anyway); without either, all panels.
    pub fn status_chip(
        &mut self,
        ui: &mut egui::Ui,
        lead: crate::kit::Lead,
        text: &str,
        tooltip: &str,
    ) -> egui::Response {
        let r = crate::kit::chip(ui, Some(lead), text, tooltip);
        if r.clicked() {
            let mine = |p: usize| Some(p) == self.plugin;
            let panel = self
                .k
                .reg
                .contributions(&crate::shell::PANELS)
                .iter()
                .any(|c| mine(c.plugin));
            let page = self
                .k
                .reg
                .pages
                .iter()
                .find(|e| mine(e.plugin))
                .map(|e| e.id);
            // A chip opens its own panel (or page), never the others'.
            match page {
                Some(page) if self.wide() || !panel => self.go(page),
                _ if panel => self.toggle_panels_from(&r),
                _ => {}
            }
        }
        r
    }
    /// Shell layout only: plugins leave screen sizes to kit.
    pub(crate) fn wide(&self) -> bool {
        self.s.wide
    }
    pub fn core_status(&self) -> CoreStatus {
        self.k.core.status()
    }
    /// Typed controller API of the running core.
    pub fn core(&self) -> crate::core::CoreApi {
        self.k.core_api()
    }
    /// Draw every contribution to a zone.
    pub fn slot<P: 'static>(&mut self, ui: &mut egui::Ui, slot: &Slot<P>, props: &P) {
        let list: Vec<_> = self.k.reg.contributions(slot).to_vec();
        for c in list {
            ui.push_id(&c.id, |ui| {
                self.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, props))
            });
        }
    }
    /// Draw contributions last-to-first (for right-to-left layouts, so they still read in order).
    pub fn slot_rev<P: 'static>(&mut self, ui: &mut egui::Ui, slot: &Slot<P>, props: &P) {
        let list: Vec<_> = self.k.reg.contributions(slot).to_vec();
        for c in list.into_iter().rev() {
            ui.push_id(&c.id, |ui| {
                self.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, props))
            });
        }
    }
    /// Number of contributions to a zone.
    pub(crate) fn slot_len<P: 'static>(&self, slot: &Slot<P>) -> usize {
        self.k.reg.contributions(slot).len()
    }
}
