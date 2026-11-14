//! rproxy: one process owns the kernel, plugins and mihomo. The desktop window
//! and every browser tab draw the same egui UI; browsers only paint frames.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[cfg(all(feature = "desktop", target_os = "linux"))]
mod desktop;
#[cfg(feature = "desktop")]
mod icon;
mod instance;
#[cfg(all(feature = "desktop", target_os = "macos"))]
mod macos;
#[cfg(all(feature = "desktop", any(target_os = "windows", target_os = "macos")))]
mod native_tray;

#[cfg(all(feature = "desktop", target_os = "linux"))]
use desktop::{Link, Tray, WindowControl, WindowEvent};

use rproxy_kernel::{Host, Session};
use rproxy_remote::host::{Asset, Frontend, FrontendSession};
use std::{path::PathBuf, sync::Arc};
use tokio::runtime::Runtime;

#[cfg(all(feature = "desktop", target_os = "linux"))]
use std::{sync::atomic::Ordering, time::Duration};

type LaunchResult = Result<(), Box<dyn std::error::Error>>;

const ASSETS: &[Asset] = &[
    Asset {
        path: "/",
        mime: "text/html; charset=utf-8",
        bytes: include_bytes!("../web/index.html"),
    },
    Asset {
        path: "/rproxy_remote.js",
        mime: "text/javascript",
        bytes: include_bytes!("../web/pkg/rproxy_remote.js"),
    },
    Asset {
        path: "/rproxy_remote_bg.wasm",
        mime: "application/wasm",
        bytes: include_bytes!("../web/pkg/rproxy_remote_bg.wasm"),
    },
];

struct Web(Arc<Host>);
struct WebSession(Session);
impl Frontend for Web {
    fn open(&self, ctx: &egui::Context) -> Box<dyn FrontendSession> {
        Box::new(WebSession(self.0.session(ctx)))
    }
}
impl FrontendSession for WebSession {
    fn ui(&mut self, ui: &mut egui::Ui) {
        self.0.ui(ui)
    }
    fn file_dialog(&mut self) -> Option<rproxy_remote::FileDialog> {
        use rproxy_kernel::configuration::{FILE_NAME, FileAction};
        match self.0.take_file_action()? {
            FileAction::Import => Some(rproxy_remote::FileDialog::Open),
            FileAction::Export => match self.0.host().export_configuration() {
                Ok(bytes) => Some(rproxy_remote::FileDialog::Save {
                    name: FILE_NAME.into(),
                    bytes,
                }),
                Err(e) => {
                    self.0.file_finished(Err(e));
                    None
                }
            },
        }
    }
    fn file_reply(&mut self, reply: rproxy_remote::FileReply) {
        use rproxy_remote::FileReply;
        self.0.file_finished(match reply {
            FileReply::Finished => Ok(None),
            FileReply::Opened { name, bytes } => Ok(Some((name, bytes))),
            FileReply::Failed(e) => Err(rproxy_kernel::Error::msg(e)),
        });
    }
}

fn data_dir() -> std::io::Result<PathBuf> {
    // A portable package opts in by carrying this marker next to the executable.
    let exe = std::env::current_exe()?;
    let parent = exe
        .parent()
        .ok_or_else(|| std::io::Error::other("Missing executable directory"))?;
    if parent.join("portable.txt").is_file() {
        return Ok(parent.join("data"));
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.map(|p| p.join("rproxy"))
        .ok_or_else(|| std::io::Error::other("Missing user data directory"))
}

fn main() -> LaunchResult {
    let mut internal = std::env::args_os().skip(1);
    if internal
        .next()
        .is_some_and(|arg| arg == rproxy_kernel::mihomo::supervisor::ARG)
    {
        return rproxy_kernel::mihomo::supervisor::run(internal).map_err(Into::into);
    }
    rproxy_kernel::mihomo::supervisor::configure(std::env::current_exe()?);
    let mut mode = None;
    let mut listen: Option<std::net::SocketAddr> = None;
    let mut background = false;
    let mut smoke_test = false;
    let mut dir = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "web" || arg == "desktop" {
            if mode.replace(arg == "web").is_some() {
                return Err("Choose desktop or web.".into());
            }
        } else if arg == "--listen" {
            let addr = args
                .next()
                .ok_or("Missing --listen IP:PORT")?
                .into_string()
                .map_err(|_| "Invalid listen address")?
                .parse()?;
            if listen.replace(addr).is_some() {
                return Err("--listen given more than once.".into());
            }
        } else if arg == "--smoke-test" {
            smoke_test = true;
        } else if arg == "--background" {
            background = true;
        } else if arg == "--data-dir" {
            let path = args
                .next()
                .filter(|p| !p.is_empty())
                .ok_or("Missing --data-dir path")?;
            if dir.replace(PathBuf::from(path)).is_some() {
                return Err("--data-dir given more than once.".into());
            }
        } else if arg == "--help" {
            attach_console();
            println!(
                "rproxy [desktop|web] [--listen IP:PORT] [--background] [--data-dir PATH]\nDesktop opens a window; web serves the same UI at http://127.0.0.1:8080."
            );
            return Ok(());
        } else {
            return Err("Unknown option. Use --help.".into());
        }
    }
    let dir = match dir {
        Some(d) => d,
        None => data_dir()?,
    };
    let web = mode.unwrap_or(!cfg!(feature = "desktop"));
    if !web && !cfg!(feature = "desktop") {
        return Err("This build supports web mode only.".into());
    }
    if web && background {
        return Err("--background requires desktop mode.".into());
    }
    if web {
        attach_console();
    }
    if web && listen.is_none() && !smoke_test {
        listen = Some(([127, 0, 0, 1], 8080).into());
    }
    let instance_dir = if web || smoke_test {
        dir.clone()
    } else {
        instance::desktop_directory()?
    };
    let Some(_instance) = instance::Instance::acquire(&instance_dir)? else {
        return Ok(());
    };
    let rt = Runtime::new()?;
    let listener = listen
        .map(|addr| rt.block_on(tokio::net::TcpListener::bind(addr)))
        .transpose()?;
    // A login item starts the desktop app; the web UI does not offer one.
    let catalog = rproxy::catalog()
        .into_iter()
        .filter(|p| !web || p.id != "autostart")
        .collect();
    let host = rt.block_on(Host::start(catalog, &dir))?;
    if smoke_test {
        let running = host.kernel().core_status().running();
        rt.block_on(host.shutdown());
        return if running {
            Ok(())
        } else {
            Err("Mihomo failed to start.".into())
        };
    }
    let server = listener
        .map(|l| -> std::io::Result<_> {
            let addr = l.local_addr()?;
            println!("Web interface: http://{addr}");
            let mut app = rproxy_remote::host::router(Arc::new(Web(host.clone())), ASSETS);
            let configured_origin = std::env::var("RPROXY_PUBLIC_ORIGIN")
                .ok()
                .map(|v| public_origin(&v))
                .transpose()?;
            app = app.layer(axum::middleware::from_fn(
                move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let configured_origin = configured_origin.clone();
                    async move {
                        if request.uri().path() == "/ws"
                            && !allowed_origin(
                                request.headers(),
                                configured_origin.as_deref(),
                                addr,
                            )
                        {
                            return axum::response::IntoResponse::into_response(
                                axum::http::StatusCode::FORBIDDEN,
                            );
                        }
                        next.run(request).await
                    }
                },
            ));
            Ok(rt.spawn(async move { axum::serve(l, app).await }))
        })
        .transpose()?;
    let result = if web {
        rt.block_on(async {
            let mut server = server.unwrap();
            tokio::select! {
                r = &mut server => r?.map_err(Into::into),
                _ = signal() => { server.abort(); Ok(()) },
            }
        })
    } else {
        #[cfg(feature = "desktop")]
        let r = windows(&rt, &host, background, &_instance);
        #[cfg(not(feature = "desktop"))]
        let r: LaunchResult = Err("Desktop support is not compiled in.".into());
        if let Some(s) = server {
            s.abort();
        }
        r
    };
    _instance.stop_accepting();
    rt.block_on(host.shutdown());
    result
}

/// Windows: the app has no console of its own (GUI subsystem). Started from a
/// terminal, `rproxy web` and `--help` print there, and Ctrl+C stops it.
fn attach_console() {
    #[cfg(windows)]
    // SAFETY: no arguments to validate; failing (no parent console) is harmless.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
}

/// SIGTERM, SIGHUP (the terminal or session closed) and Ctrl+C end the app gracefully.
async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut hangup = signal(SignalKind::hangup()).expect("SIGHUP handler");
        tokio::select! { _ = term.recv() => {}, _ = hangup.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(feature = "desktop")]
type FileSelection = rproxy_kernel::Result<Option<(String, Vec<u8>)>>;
#[cfg(feature = "desktop")]
type SharedWindowState = std::rc::Rc<std::cell::RefCell<WindowState>>;
#[cfg(feature = "desktop")]
struct WindowState {
    session: Session,
    files: Option<std::sync::mpsc::Receiver<FileSelection>>,
    started: std::time::Instant,
}
#[cfg(feature = "desktop")]
impl WindowState {
    fn new(session: Session) -> SharedWindowState {
        std::rc::Rc::new(std::cell::RefCell::new(Self {
            session,
            files: None,
            started: std::time::Instant::now(),
        }))
    }
}
#[cfg(feature = "desktop")]
struct Window {
    state: SharedWindowState,
    #[cfg(target_os = "linux")]
    system_theme: desktop::SystemTheme,
    #[cfg(target_os = "linux")]
    control: Option<WindowControl>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    tray: Option<native_tray::NativeTray>,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    tray_attempted: bool,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    background: bool,
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    activation: Option<Arc<std::sync::atomic::AtomicBool>>,
    quitting: bool,
}
#[cfg(feature = "desktop")]
impl Window {
    fn new(state: SharedWindowState, _ctx: &egui::Context, _background: bool) -> Self {
        Self {
            state,
            quitting: false,
            #[cfg(target_os = "linux")]
            system_theme: desktop::SystemTheme::start(_ctx.clone()),
            #[cfg(target_os = "linux")]
            control: None,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            tray: None,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            tray_attempted: false,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            background: _background,
            #[cfg(any(target_os = "windows", target_os = "macos"))]
            activation: None,
        }
    }
    fn file_dialogs(&mut self, ctx: &egui::Context) {
        let mut state = self.state.borrow_mut();
        if state.files.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if let Some(result) = state.files.as_ref().and_then(|rx| rx.try_recv().ok()) {
            state.files = None;
            state.session.file_finished(result);
        }
        let Some(action) = state.session.take_file_action() else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        state.files = Some(rx);
        let host = state.session.host().clone();
        let ctx = ctx.clone();
        tokio::spawn(async move {
            use rproxy_kernel::configuration::{FILE_NAME, FileAction, MAX_BYTES};
            let started = std::time::Instant::now();
            // Without a portal (or zenity) rfd returns at once, as if cancelled.
            let closed = move || -> FileSelection {
                if cfg!(target_os = "linux")
                    && started.elapsed() < std::time::Duration::from_millis(300)
                {
                    return Err(rproxy_kernel::Error::msg(
                        "No dialog? Install xdg-desktop-portal-gtk or zenity.",
                    ));
                }
                Ok(None)
            };
            let result = async {
                let dialog =
                    rfd::AsyncFileDialog::new().add_filter("rproxy configuration", &["sqlite3"]);
                match action {
                    FileAction::Export => {
                        let Some(file) = dialog
                            .set_title("Export configuration")
                            .set_file_name(FILE_NAME)
                            .save_file()
                            .await
                        else {
                            return closed();
                        };
                        host.export_configuration_to(file.path())?;
                        if let Some(k) = host.try_kernel() {
                            k.notify(
                                "Configuration exported",
                                "The file was saved to the selected location.",
                            );
                        }
                        Ok(None)
                    }
                    FileAction::Import => {
                        let Some(file) = dialog.set_title("Import configuration").pick_file().await
                        else {
                            return closed();
                        };
                        use tokio::io::AsyncReadExt;
                        let input = tokio::fs::File::open(file.path())
                            .await
                            .map_err(|e| rproxy_kernel::Error::msg(e.to_string()))?;
                        let mut bytes = Vec::new();
                        input
                            .take(MAX_BYTES as u64 + 1)
                            .read_to_end(&mut bytes)
                            .await
                            .map_err(|e| rproxy_kernel::Error::msg(e.to_string()))?;
                        if bytes.len() > MAX_BYTES {
                            return Err(rproxy_kernel::Error::msg(
                                "Configuration exceeds the 32 MiB file limit.",
                            ));
                        }
                        Ok(Some((file.file_name(), bytes)))
                    }
                }
            }
            .await;
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
}
#[cfg(feature = "desktop")]
impl eframe::App for Window {
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        #[cfg(target_os = "linux")]
        if let Some(control) = &self.control {
            control.handle_open(ctx);
        }
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            if !self.tray_attempted {
                self.tray_attempted = true;
                match native_tray::NativeTray::new(ctx) {
                    Ok(tray) => self.tray = Some(tray),
                    Err(_) => {
                        if let Some(k) = self.state.borrow().session.host().try_kernel() {
                            k.notify_error(
                                "Tray unavailable",
                                "Closing the window will minimize it while routing continues.",
                            );
                        }
                    }
                }
            }
            if let Some(tray) = &mut self.tray {
                tray.update(icon::status(self.state.borrow().session.host()));
                if self.background {
                    self.background = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
                if ctx.input(|i| i.viewport().close_requested())
                    && !self.quitting
                    && !tray.quitting.load(std::sync::atomic::Ordering::Acquire)
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
            } else if !self.quitting && ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            if self
                .activation
                .as_ref()
                .is_some_and(|flag| flag.swap(false, std::sync::atomic::Ordering::AcqRel))
            {
                native_tray::show(ctx);
            }
            // App::logic is also called on repaint while the window is hidden.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }
    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        // Cmd+Q, Dock Quit and logout end the process inside AppKit: run_native
        // never returns, so restore the system proxy and stop the core here.
        // Host::shutdown is idempotent; main calls it again when run_native returns.
        #[cfg(target_os = "macos")]
        {
            let host = self.state.borrow().session.host().clone();
            tokio::runtime::Handle::current().block_on(host.shutdown());
        }
    }
    fn raw_input_hook(&mut self, _: &egui::Context, input: &mut egui::RawInput) {
        // Recreated windows share the same clock as retained toasts and jobs.
        input.time = Some(self.state.borrow().started.elapsed().as_secs_f64());
        #[cfg(target_os = "linux")]
        if let Some(theme) = self.system_theme.get() {
            input.system_theme = Some(theme);
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        #[cfg(target_os = "linux")]
        let no_tray = self
            .control
            .as_ref()
            .is_some_and(|c| !c.online.load(Ordering::Acquire));
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let no_tray = self.tray_attempted && self.tray.is_none();
        {
            let mut state = self.state.borrow_mut();
            state.session.set_exit_available(no_tray);
            state.session.ui(ui);
            if state.session.take_exit_request() {
                self.quitting = true;
                #[cfg(target_os = "linux")]
                if let Some(control) = &self.control {
                    control.send(WindowEvent::Quit);
                }
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.file_dialogs(ui.ctx());
    }
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
fn windows(
    rt: &Runtime,
    host: &Arc<Host>,
    background: bool,
    instance: &instance::Instance,
) -> LaunchResult {
    let _guard = rt.enter();
    let (control, events) = WindowControl::new();
    let activation = control.clone();
    instance.on_open(move || activation.send(WindowEvent::Open));
    let mut tray = rt.block_on(Tray::start(control.clone()));
    // At login the panel's tray host may start after rproxy: wait up to 10 s
    // before falling back to showing the window.
    for _ in 0..20 {
        if tray.is_some() || !background {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
        tray = rt.block_on(Tray::start(control.clone()));
    }
    let stop = control.clone();
    let signal_task = rt.spawn(async move {
        signal().await;
        stop.send(WindowEvent::Quit);
    });
    let h = host.clone();
    let t = tray.clone();
    let monitor = rt.spawn(async move {
        let mut last = None;
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        loop {
            poll.tick().await;
            let link = icon::status(&h);
            if last != Some(link) {
                if let Some(handle) = &t {
                    handle.update(|tray| tray.link = link).await;
                }
                last = Some(link);
            }
        }
    });
    let retained = std::rc::Rc::new(std::cell::RefCell::new(None::<SharedWindowState>));
    let mut open = !background || !control.online.load(Ordering::Acquire);
    let result = (|| -> LaunchResult {
        loop {
            if control.quitting.load(Ordering::Acquire) {
                break;
            }
            if !open {
                loop {
                    match events.recv()? {
                        desktop::Wake::Quit => return Ok(()),
                        desktop::Wake::Open(sequence)
                            if sequence > control.handled_open.load(Ordering::Acquire) =>
                        {
                            break;
                        }
                        _ => {}
                    }
                }
            }
            let opts = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_app_id("io.github.rproxy.Rproxy")
                    .with_title("rproxy")
                    .with_decorations(true)
                    .with_inner_size([640., 560.])
                    .with_min_inner_size([375., 360.])
                    .with_icon(desktop::icon(Link::Idle)),
                run_and_return: true,
                ..Default::default()
            };
            let window = control.clone();
            let retained = retained.clone();
            let host = host.clone();
            eframe::run_native(
                "rproxy",
                opts,
                Box::new(move |cc| {
                    {
                        use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
                        let mut context = window.context.lock().unwrap();
                        window.wayland.store(
                            cc.display_handle()
                                .is_ok_and(|d| matches!(d.as_raw(), RawDisplayHandle::Wayland(_))),
                            Ordering::Release,
                        );
                        window.handled_open.store(
                            window.open_sequence.load(Ordering::Acquire),
                            Ordering::Release,
                        );
                        *context = Some(cc.egui_ctx.clone());
                    }
                    if window.quitting.load(Ordering::Acquire) {
                        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    let state = retained
                        .borrow()
                        .clone()
                        .unwrap_or_else(|| WindowState::new(host.session(&cc.egui_ctx)));
                    state.borrow_mut().session.rebind(&cc.egui_ctx);
                    *retained.borrow_mut() = Some(state.clone());
                    let mut app = Window::new(state, &cc.egui_ctx, false);
                    app.control = Some(window.clone());
                    Ok(Box::new(app))
                }),
            )?;
            *control.context.lock().unwrap() = None;
            open = false;
        }
        Ok(())
    })();
    monitor.abort();
    signal_task.abort();
    if let Some(tray) = tray {
        rt.block_on(tray.shutdown());
    }
    result
}

#[cfg(all(feature = "desktop", not(target_os = "linux")))]
fn windows(
    rt: &Runtime,
    host: &Arc<Host>,
    background: bool,
    instance: &instance::Instance,
) -> LaunchResult {
    let _guard = rt.enter();
    let host = host.clone();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("io.github.rproxy.Rproxy")
            .with_title("rproxy")
            .with_decorations(true)
            .with_inner_size([640., 560.])
            .with_min_inner_size([375., 360.])
            .with_icon(icon::icon(icon::Link::Idle)),
        ..Default::default()
    };
    eframe::run_native(
        "rproxy",
        opts,
        Box::new(move |cc| {
            let context = cc.egui_ctx.clone();
            instance.on_open(move || context.request_repaint());
            let mut window = Window::new(
                WindowState::new(host.session(&cc.egui_ctx)),
                &cc.egui_ctx,
                background,
            );
            window.activation = Some(instance.activation_flag());
            Ok(Box::new(window))
        }),
    )?;
    Ok(())
}

// Local web mode is same-origin too. Hostname/DNS based deployments must set
// their explicit HTTPS origin; arbitrary DNS names cannot rebind to loopback.
fn allowed_origin(
    headers: &axum::http::HeaderMap,
    configured: Option<&str>,
    listen: std::net::SocketAddr,
) -> bool {
    let Some(raw) = headers.get("origin").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    if let Some(expected) = configured {
        return raw == expected;
    }
    let Ok(origin) = url::Url::parse(raw) else {
        return false;
    };
    let Some(host) = headers.get("host").and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Ok(request) = url::Url::parse(&format!("http://{host}")) else {
        return false;
    };
    let address_ok = match origin.host() {
        Some(url::Host::Ipv4(ip)) => {
            listen.ip().is_unspecified() || listen.ip() == std::net::IpAddr::V4(ip)
        }
        Some(url::Host::Ipv6(ip)) => {
            listen.ip().is_unspecified() || listen.ip() == std::net::IpAddr::V6(ip)
        }
        Some(url::Host::Domain("localhost")) => listen.ip().is_loopback(),
        _ => false,
    };
    address_ok
        && origin.scheme() == "http"
        && origin.origin() == request.origin()
        && origin.port_or_known_default() == Some(listen.port())
        && origin.username().is_empty()
        && origin.password().is_none()
        && origin.path() == "/"
        && origin.query().is_none()
        && origin.fragment().is_none()
}

// Browsers omit :443 from Origin even when the configured URL includes it.
fn public_origin(value: &str) -> std::io::Result<String> {
    let invalid = || {
        std::io::Error::other(
            "RPROXY_PUBLIC_ORIGIN must be an HTTPS origin without credentials, query or fragment",
        )
    };
    let url = url::Url::parse(value).map_err(|_| invalid())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::public_origin;

    #[test]
    fn web_rejects_foreign_and_rebinding_origins_without_special_configuration() {
        let listen = "127.0.0.1:8080".parse().unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "127.0.0.1:8080".parse().unwrap());
        assert!(!super::allowed_origin(&headers, None, listen));
        for value in [
            "https://evil.example",
            "null",
            "http://127.0.0.1:8081",
            "http://u:p@127.0.0.1:8080",
        ] {
            headers.insert("origin", value.parse().unwrap());
            assert!(!super::allowed_origin(&headers, None, listen));
        }
        headers.insert("origin", "http://127.0.0.1:8080".parse().unwrap());
        assert!(super::allowed_origin(&headers, None, listen));
        headers.insert("host", "localhost:8080".parse().unwrap());
        headers.insert("origin", "http://localhost:8080".parse().unwrap());
        assert!(super::allowed_origin(&headers, None, listen));
        headers.insert("host", "evil.example:8080".parse().unwrap());
        headers.insert("origin", "http://evil.example:8080".parse().unwrap());
        assert!(!super::allowed_origin(&headers, None, listen));
        headers.insert("origin", "https://panel.example".parse().unwrap());
        assert!(super::allowed_origin(
            &headers,
            Some("https://panel.example"),
            listen
        ));
    }

    #[test]
    fn public_origin_matches_browser_default_port() {
        assert_eq!(
            public_origin("https://panel.example.com:443").unwrap(),
            "https://panel.example.com"
        );
        assert_eq!(
            public_origin("https://panel.example.com:9443").unwrap(),
            "https://panel.example.com:9443"
        );
    }

    #[test]
    fn public_origin_rejects_non_origins() {
        for value in [
            "http://panel.example.com",
            "https://u:p@panel.example.com",
            "https://panel.example.com/path",
            "https://panel.example.com/?q=1",
        ] {
            assert!(public_origin(value).is_err(), "{value}");
        }
    }
}
