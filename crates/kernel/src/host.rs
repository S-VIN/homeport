//! The process-wide owner of the data directory and the running kernel.
//! Restarting (after plugin changes) replaces the kernel; open windows and
//! browser sessions keep running and simply draw the new one.
use crate::{Error, Kernel, Plugin, Result, shell, store::Store, ui::SessionState};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

pub struct Host {
    pub(crate) catalog: Vec<&'static Plugin>,
    pub(crate) dir: PathBuf,
    pub(crate) store: Store,
    _lock: fs::File,
    current: RwLock<Option<Arc<Kernel>>>,
    epoch: AtomicU64,
    contexts: Mutex<Vec<(u64, egui::Context)>>,
    next_session: AtomicU64,
    restarting: tokio::sync::Mutex<()>,
    closing: AtomicBool,
    failure: Mutex<Option<String>>,
    me: Weak<Host>,
}

/// File name of the plugin-era database inside the data directory.
pub const DATABASE: &str = "rproxy-plugins.sqlite3";

impl Host {
    /// Lock `dir`, open the database and start the kernel with the enabled plugins.
    pub async fn start(catalog: Vec<&'static Plugin>, dir: &Path) -> Result<Arc<Host>> {
        let io = |e: std::io::Error| Error::msg(e.to_string());
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(dir).map_err(io)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("instance.lock"))
            .map_err(io)?;
        lock.try_lock().map_err(|_| {
            Error::msg("This data directory is already open in another rproxy instance.")
        })?;
        let path = dir.join(DATABASE);
        let mut options = fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        options.open(&path).map_err(io)?;
        let store = Store::open(&path)?;
        let host = Arc::new_cyclic(|me| Host {
            catalog,
            dir: dir.to_owned(),
            store,
            _lock: lock,
            current: RwLock::new(None),
            epoch: AtomicU64::new(0),
            contexts: Mutex::default(),
            next_session: AtomicU64::new(1),
            restarting: tokio::sync::Mutex::new(()),
            closing: AtomicBool::new(false),
            failure: Mutex::default(),
            me: me.clone(),
        });
        host.launch().await?;
        Ok(host)
    }

    async fn launch(&self) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::msg("rproxy is shutting down."));
        }
        let me = self.me.clone();
        let restart = Arc::new(move || {
            if let Some(h) = me.upgrade() {
                tokio::spawn(async move { h.restart().await });
            }
        });
        let me = self.me.clone();
        let restore = Arc::new(move |config: Arc<crate::configuration::Import>| {
            if let Some(h) = me.upgrade() {
                tokio::spawn(async move {
                    if let Err(e) = h.import_configuration(config).await
                        && let Some(k) = h.try_kernel()
                    {
                        k.notify_error("Import failed", &e.message);
                    }
                });
            }
        });
        let me = self.me.clone();
        let ui_open = Arc::new(move || {
            me.upgrade()
                .is_some_and(|h| !h.contexts.lock().unwrap().is_empty())
        });
        let k = Kernel::start(
            self.catalog.clone(),
            self.store.clone(),
            self.dir.clone(),
            restart,
            restore,
            ui_open,
        )
        .await?;
        if self.closing.load(Ordering::Acquire) {
            k.shutdown().await;
            return Err(Error::msg("rproxy is shutting down."));
        }
        // Repaint every open session when state, jobs or notices change.
        let mut changes = k.changes();
        let me = self.me.clone();
        let weak = Arc::downgrade(&k);
        tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                if weak.upgrade().is_none_or(|k| k.closed()) {
                    return;
                }
                let Some(h) = me.upgrade() else { return };
                for (_, ctx) in h.contexts.lock().unwrap().iter() {
                    ctx.request_repaint();
                }
            }
        });
        *self.current.write().unwrap() = Some(k);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.repaint();
        Ok(())
    }

    /// The running kernel. Panics while a restart is in progress; UI code uses [`Self::try_kernel`].
    pub fn kernel(&self) -> Arc<Kernel> {
        self.try_kernel().expect("kernel running")
    }
    pub fn try_kernel(&self) -> Option<Arc<Kernel>> {
        self.current.read().unwrap().clone()
    }

    /// Stop the kernel and start it again with the saved plugin choices.
    pub fn restart(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            let _r = self.restarting.lock().await;
            if self.closing.load(Ordering::Acquire) {
                return;
            }
            let old = self.current.write().unwrap().take();
            if let Some(k) = old {
                k.shutdown().await;
            }
            self.relaunch().await;
        })
    }

    /// Start a kernel from the saved profile; a failure is shown instead of the UI.
    async fn relaunch(&self) {
        let result = self.launch().await;
        *self.failure.lock().unwrap() = result.err().map(|e| e.message);
        self.repaint();
    }

    /// Apply a previously validated configuration, rolling back on startup failure.
    /// The rollback copy exists only during this operation and is then deleted.
    pub fn import_configuration(
        &self,
        config: Arc<crate::configuration::Import>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(self.import_inner(config))
    }
    async fn import_inner(&self, config: Arc<crate::configuration::Import>) -> Result<()> {
        let _r = self.restarting.lock().await;
        if self.closing.load(Ordering::Acquire) {
            return Err(Error::msg("rproxy is shutting down."));
        }
        let previous = tempfile::tempdir().map_err(crate::configuration::io)?;
        let rollback = previous.path().join("rollback.sqlite3");
        let old = self.current.write().unwrap().take();
        let was_running = old.as_ref().is_some_and(|k| k.core_status().running());
        if let Some(k) = &old {
            k.shutdown().await;
        }
        // The stopped writer cannot commit after this snapshot.
        if let Err(e) = self.store.backup_to(&rollback) {
            self.relaunch().await;
            return Err(e);
        }
        let replaced = self.store.restore_from(config.file.path());
        let result = match replaced {
            Ok(()) => self.launch().await.and_then(|_| {
                if was_running && self.try_kernel().is_none_or(|k| !k.core_status().running()) {
                    Err(Error::msg(
                        "The imported configuration could not start the core.",
                    ))
                } else {
                    Ok(())
                }
            }),
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            let failed = self.current.write().unwrap().take();
            if let Some(k) = failed {
                k.shutdown().await;
            }
            self.store.restore_from(&rollback)?;
            self.relaunch().await;
            return Err(Error::msg(format!(
                "{} Previous settings were restored.",
                e.message
            )));
        }
        *self.failure.lock().unwrap() = None;
        if let Some(k) = self.try_kernel() {
            k.notify(
                "Configuration imported",
                "Your settings and lists have been replaced.",
            );
        }
        self.repaint();
        Ok(())
    }

    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        let _r = self.restarting.lock().await;
        let old = self.current.write().unwrap().take();
        if let Some(k) = old {
            k.shutdown().await;
        }
    }

    fn repaint(&self) {
        for (_, ctx) in self.contexts.lock().unwrap().iter() {
            ctx.request_repaint();
        }
    }

    /// A UI session for a window or browser tab drawing into `ctx`.
    pub fn session(self: &Arc<Self>, ctx: &egui::Context) -> Session {
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        self.contexts.lock().unwrap().push((id, ctx.clone()));
        style(ctx);
        Session {
            host: self.clone(),
            id,
            epoch: 0,
            state: None,
            ctx: ctx.clone(),
            warm: false,
            can_exit: false,
        }
    }
}

/// One window or browser tab. Call [`Session::ui`] every frame.
pub struct Session {
    host: Arc<Host>,
    id: u64,
    epoch: u64,
    state: Option<SessionState>,
    ctx: egui::Context,
    warm: bool,
    can_exit: bool,
}
impl Session {
    /// Reattach a retained desktop session when the OS needs a new native window.
    /// Plugin drafts, current page and pending configuration dialogs stay intact.
    pub fn rebind(&mut self, ctx: &egui::Context) {
        let mut data = self.ctx.data(Clone::clone);
        // Drafts and widget state survive; GPU handles belong to the old renderer.
        data.remove_by_type::<egui::TextureHandle>();
        ctx.data_mut(|target| *target = data);
        self.ctx = ctx.clone();
        self.warm = false;
        if let Some(state) = &mut self.state {
            state.ctx = ctx.clone();
            state.theme = None;
        }
        if let Some((_, context)) = self
            .host
            .contexts
            .lock()
            .unwrap()
            .iter_mut()
            .find(|(id, _)| *id == self.id)
        {
            *context = ctx.clone();
        }
        style(ctx);
    }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = &ui.ctx().clone();
        if !std::mem::replace(&mut self.warm, true) {
            ctx.request_repaint();
            return;
        }
        let Some(k) = self.host.try_kernel() else {
            let failure = self.host.failure.lock().unwrap().clone();
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 3.);
                    match failure {
                        Some(e) => {
                            ui.label(crate::kit::bold("rproxy could not start its plugins", 15.));
                            ui.label(e);
                            if crate::kit::action(ui, "Try again", true) {
                                let h = self.host.clone();
                                tokio::spawn(async move { h.restart().await });
                            }
                        }
                        None => {
                            ui.spinner();
                            ui.label("Restarting…");
                        }
                    }
                });
            });
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        };
        let epoch = self.host.epoch.load(Ordering::SeqCst);
        if self.epoch != epoch || self.state.is_none() {
            let page = self.state.as_ref().map(|s| s.page);
            let mut s = SessionState::new(self.ctx.clone(), &k);
            s.can_exit = self.can_exit;
            if let Some(page) = page {
                s.page = page;
            }
            self.state = Some(s);
            self.epoch = epoch;
        }
        let state = self.state.as_mut().unwrap();
        if let Some(result) = state.checking.as_mut().and_then(|c| c.try_recv().ok()) {
            state.checking = None;
            state.file_busy = false;
            match result {
                Ok(import) => {
                    state.incoming = Some(import);
                    state.page = "settings";
                }
                Err(e) => k.notify_error("Configuration", &e.message),
            }
        }
        shell::frame(ui, &k, state);
    }
    /// Native-only fallback when the desktop cannot provide a tray menu.
    pub fn set_exit_available(&mut self, available: bool) {
        self.can_exit = available;
        if let Some(state) = &mut self.state {
            state.can_exit = available;
        }
    }
    pub fn take_exit_request(&mut self) -> bool {
        self.state
            .as_mut()
            .is_some_and(|state| std::mem::take(&mut state.exit_requested))
    }
    /// Consumed by the native or browser frontend to open its own file dialog.
    pub fn take_file_action(&mut self) -> Option<crate::configuration::FileAction> {
        self.state.as_mut().and_then(|s| s.file_action.take())
    }
    pub fn file_finished(&mut self, file: Result<Option<(String, Vec<u8>)>>) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match file {
            Ok(Some((name, bytes))) => {
                // Loading a large file takes a moment: validate it off the UI thread.
                let (done, checking) = tokio::sync::oneshot::channel();
                let (host, ctx) = (self.host.clone(), self.ctx.clone());
                tokio::task::spawn_blocking(move || {
                    let _ = done.send(host.prepare_import(name, &bytes));
                    ctx.request_repaint();
                });
                state.checking = Some(checking);
            }
            Ok(None) => state.file_busy = false,
            Err(e) => {
                state.file_busy = false;
                if let Some(k) = self.host.try_kernel() {
                    k.notify_error("Configuration", &e.message);
                }
            }
        }
        self.ctx.request_repaint();
    }
    pub fn host(&self) -> &Arc<Host> {
        &self.host
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.host
            .contexts
            .lock()
            .unwrap()
            .retain(|(id, _)| *id != self.id);
    }
}

/// Fonts and both themes. Fonts take effect from the next pass, so the first
/// frame of a session only installs them.
fn style(ctx: &egui::Context) {
    crate::kit::theme::install_fonts(ctx);
    ctx.set_style_of(egui::Theme::Light, crate::kit::theme::style(false, false));
    ctx.set_style_of(egui::Theme::Dark, crate::kit::theme::style(true, false));
}
