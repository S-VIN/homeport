//! rproxy: one process owns the kernel, plugins and mihomo. The desktop window
//! and every browser tab draw the same egui UI; browsers only paint frames.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#[cfg(all(feature = "desktop", target_os = "linux"))]
mod desktop;
#[cfg(feature = "desktop")]
mod icon;

#[cfg(all(feature = "desktop", target_os = "linux"))]
use desktop::{Link, Tray, WindowControl, WindowEvent};

#[cfg(all(feature = "desktop", target_os = "linux"))]
use rproxy_kernel::shell::TrayLink;
use rproxy_kernel::{Host, Session};
use rproxy_remote::host::{Asset, Frontend, FrontendSession};
use std::{path::PathBuf, sync::Arc};
use tokio::runtime::Runtime;

#[cfg(all(feature = "desktop", target_os = "linux"))]
use std::{sync::atomic::Ordering, time::Duration};

type LaunchResult = Result<(), Box<dyn std::error::Error>>;

const ASSETS: &[Asset] = &[
    Asset { path: "/", mime: "text/html; charset=utf-8", bytes: include_bytes!("../web/index.html") },
    Asset { path: "/rproxy_remote.js", mime: "text/javascript", bytes: include_bytes!("../web/pkg/rproxy_remote.js") },
    Asset { path: "/rproxy_remote_bg.wasm", mime: "application/wasm", bytes: include_bytes!("../web/pkg/rproxy_remote_bg.wasm") },
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
}

fn data_dir() -> std::io::Result<PathBuf> {
    // A portable package opts in by carrying this marker next to the executable.
    let exe = std::env::current_exe()?;
    let parent = exe.parent().ok_or_else(|| std::io::Error::other("Missing executable directory"))?;
    if parent.join("portable.txt").is_file() {
        return Ok(parent.join("data"));
    }
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    base.map(|p| p.join("rproxy")).ok_or_else(|| std::io::Error::other("Missing user data directory"))
}

fn main() -> LaunchResult {
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
            let addr = args.next().ok_or("Missing --listen IP:PORT")?.into_string().map_err(|_| "Invalid listen address")?.parse()?;
            if listen.replace(addr).is_some() {
                return Err("--listen given more than once.".into());
            }
        } else if arg == "--smoke-test" {
            smoke_test = true;
        } else if arg == "--background" {
            background = true;
        } else if arg == "--data-dir" {
            let path = args.next().filter(|p| !p.is_empty()).ok_or("Missing --data-dir path")?;
            if dir.replace(PathBuf::from(path)).is_some() {
                return Err("--data-dir given more than once.".into());
            }
        } else if arg == "--help" {
            println!("rproxy [desktop|web] [--listen IP:PORT] [--background] [--data-dir PATH]\nDesktop opens a window; web serves the same UI at http://127.0.0.1:8080.");
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
    if web && listen.is_none() && !smoke_test {
        listen = Some(([127, 0, 0, 1], 8080).into());
    }
    let rt = Runtime::new()?;
    let listener = listen.map(|addr| rt.block_on(tokio::net::TcpListener::bind(addr))).transpose()?;
    let host = rt.block_on(Host::start(rproxy::catalog(), &dir))?;
    if smoke_test {
        let running = host.kernel().core_status().running();
        rt.block_on(host.shutdown());
        return if running { Ok(()) } else { Err("Mihomo failed to start.".into()) };
    }
    let server = listener
        .map(|l| -> std::io::Result<_> {
            let addr = l.local_addr()?;
            println!("Web interface: http://{addr}");
            let mut app = rproxy_remote::host::router(Arc::new(Web(host.clone())), ASSETS);
            if let Ok(origin) = std::env::var("RPROXY_PUBLIC_ORIGIN") {
                let origin = public_origin(&origin)?;
                app = app.layer(axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
                    let origin = origin.clone();
                    async move {
                        if request.uri().path() == "/ws" && request.headers().get("origin").and_then(|h| h.to_str().ok()) != Some(origin.as_str()) {
                            return axum::response::IntoResponse::into_response(axum::http::StatusCode::FORBIDDEN);
                        }
                        next.run(request).await
                    }
                }));
            }
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
        let r = windows(&rt, &host, background);
        #[cfg(not(feature = "desktop"))]
        let r: LaunchResult = Err("Desktop support is not compiled in.".into());
        if let Some(s) = server {
            s.abort();
        }
        r
    };
    rt.block_on(host.shutdown());
    result
}

async fn signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! { _ = term.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(feature = "desktop")]
struct Window {
    session: Session,
}
#[cfg(feature = "desktop")]
impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        self.session.ui(ui);
    }
}

#[cfg(all(feature = "desktop", target_os = "linux"))]
fn windows(rt: &Runtime, host: &Arc<Host>, background: bool) -> LaunchResult {
    let _guard = rt.enter();
    let (control, events) = WindowControl::new();
    let tray = rt.block_on(Tray::start(control.clone()));
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
            let Some(k) = h.try_kernel() else { continue };
            let answers = k.snapshot().hooks(&rproxy_kernel::shell::TRAY, &());
            // The worst answer wins: a failed core or server beats "connected".
            let link = if !k.core_status().running() || answers.contains(&Some(TrayLink::Failed)) {
                Link::Failed
            } else if answers.contains(&Some(TrayLink::Connected)) {
                Link::Selected
            } else {
                Link::Idle
            };
            if last != Some(link) {
                if let Some(handle) = &t {
                    handle.update(|tray| tray.link = link).await;
                }
                last = Some(link);
            }
        }
    });
    let mut open = !background || !control.online.load(Ordering::Acquire);
    let result = (|| -> LaunchResult {
        loop {
            if control.quitting.load(Ordering::Acquire) {
                break;
            }
            if !open && events.recv()? == WindowEvent::Quit {
                break;
            }
            let opts = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_app_id("rproxy")
                    .with_title("rproxy")
                    .with_inner_size([640., 560.])
                    .with_min_inner_size([375., 360.])
                    .with_icon(desktop::icon(Link::Idle)),
                run_and_return: true,
                ..Default::default()
            };
            let window = control.clone();
            let host = host.clone();
            eframe::run_native(
                "rproxy",
                opts,
                Box::new(move |cc| {
                    *window.context.lock().unwrap() = Some(cc.egui_ctx.clone());
                    if window.quitting.load(Ordering::Acquire) {
                        cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Ok(Box::new(Window { session: host.session(&cc.egui_ctx) }))
                }),
            )?;
            *control.context.lock().unwrap() = None;
            if !control.online.load(Ordering::Acquire) {
                break;
            }
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
fn windows(rt: &Runtime, host: &Arc<Host>, _: bool) -> LaunchResult {
    let _guard = rt.enter();
    let host = host.clone();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_app_id("rproxy")
            .with_title("rproxy")
            .with_inner_size([640., 560.])
            .with_min_inner_size([375., 360.])
            .with_icon(icon::icon(icon::Link::Idle)),
        ..Default::default()
    };
    eframe::run_native("rproxy", opts, Box::new(move |cc| Ok(Box::new(Window { session: host.session(&cc.egui_ctx) }))))?;
    Ok(())
}

// Browsers omit :443 from Origin even when the configured URL includes it.
fn public_origin(value: &str) -> std::io::Result<String> {
    let invalid = || std::io::Error::other("RPROXY_PUBLIC_ORIGIN must be an HTTPS origin without credentials, query or fragment");
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
    fn public_origin_matches_browser_default_port() {
        assert_eq!(public_origin("https://panel.example.com:443").unwrap(), "https://panel.example.com");
        assert_eq!(public_origin("https://panel.example.com:9443").unwrap(), "https://panel.example.com:9443");
    }

    #[test]
    fn public_origin_rejects_non_origins() {
        for value in ["http://panel.example.com", "https://u:p@panel.example.com", "https://panel.example.com/path", "https://panel.example.com/?q=1"]
        {
            assert!(public_origin(value).is_err(), "{value}");
        }
    }
}
