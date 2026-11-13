//! The process-wide owner of the data directory and the running kernel.
//! Restarting (after plugin changes) replaces the kernel; open windows and
//! browser sessions keep running and simply draw the new one.
use crate::{shell, store::Store, ui::SessionState, Error, Kernel, Plugin, Result};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, RwLock, Weak,
    },
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

pub struct Host {
    catalog: Vec<&'static Plugin>,
    dir: PathBuf,
    store: Store,
    _lock: fs::File,
    current: RwLock<Option<Arc<Kernel>>>,
    epoch: AtomicU64,
    contexts: Mutex<Vec<(u64, egui::Context)>>,
    next_session: AtomicU64,
    restarting: tokio::sync::Mutex<()>,
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
        let lock = fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("instance.lock")).map_err(io)?;
        lock.try_lock().map_err(|_| Error::msg("This data directory is already open in another rproxy instance."))?;
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
            failure: Mutex::default(),
            me: me.clone(),
        });
        host.launch().await?;
        Ok(host)
    }

    async fn launch(&self) -> Result<()> {
        let me = self.me.clone();
        let restart = Arc::new(move || {
            if let Some(h) = me.upgrade() {
                tokio::spawn(async move { h.restart().await });
            }
        });
        let me = self.me.clone();
        let restore = Arc::new(move |path: PathBuf| {
            if let Some(h) = me.upgrade() {
                tokio::spawn(async move { h.restore(&path).await });
            }
        });
        let k = Kernel::start(
            self.catalog.clone(),
            self.store.clone(),
            self.dir.clone(),
            restart,
            restore,
        )
        .await?;
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
            let old = self.current.write().unwrap().take();
            if let Some(k) = old {
                k.shutdown().await;
            }
            let result = self.launch().await;
            *self.failure.lock().unwrap() = result.err().map(|e| e.message);
            self.repaint();
        })
    }

    /// Replace the database with the backup at `path` and start the kernel again.
    /// The state before the restore is kept as an automatic backup.
    pub fn restore<'a>(
        &'a self,
        path: &'a Path,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(self.restore_inner(path))
    }
    async fn restore_inner(&self, path: &Path) {
        let _r = self.restarting.lock().await;
        // Keep the current state first; if that fails, nothing changes.
        if let Some(k) = self.try_kernel() {
            if let Err(e) = k.back_up_keeping(true, Some(path)) {
                k.notify_error("Couldn't restore the backup", &format!("The current settings could not be saved first: {}", e.message));
                return;
            }
        }
        let old = self.current.write().unwrap().take();
        if let Some(k) = &old {
            k.shutdown().await;
        }
        drop(old);
        let result = self.store.restore_from(path);
        let launched = self.launch().await;
        *self.failure.lock().unwrap() = launched.err().map(|e| e.message);
        if let (Err(e), Some(k)) = (result, self.try_kernel()) {
            k.notify_error("Couldn't restore the backup", &e.message);
        } else if let Some(k) = self.try_kernel() {
            k.notify(
                "Settings restored",
                "The backup replaced the current settings.",
            );
        }
        self.repaint();
    }

    pub async fn shutdown(&self) {
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
        // Fonts take effect from the next pass; the first frame only installs them.
        crate::kit::theme::install_fonts(ctx);
        ctx.set_style_of(egui::Theme::Light, crate::kit::theme::style(false, false));
        ctx.set_style_of(egui::Theme::Dark, crate::kit::theme::style(true, false));
        Session { host: self.clone(), id, epoch: 0, state: None, ctx: ctx.clone(), warm: false }
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
}
impl Session {
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
            if let Some(page) = page {
                s.page = page;
            }
            self.state = Some(s);
            self.epoch = epoch;
        }
        shell::frame(ui, &k, self.state.as_mut().unwrap());
    }
    pub fn host(&self) -> &Arc<Host> {
        &self.host
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.host.contexts.lock().unwrap().retain(|(id, _)| *id != self.id);
    }
}
