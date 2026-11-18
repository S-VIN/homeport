//! # local-proxy
//! One local HTTP + SOCKS5 proxy (mihomo `mixed`) with optional login, the
//! system proxy pointed at it and TUN. A port that cannot bind is reported on
//! the page; the rest keeps working.
use homeport_kernel::{
    kit::{self, theme},
    mihomo::{self, Listener, ListenerKind},
    prelude::*,
};
use std::{fmt, net::IpAddr, str::FromStr, time::Duration};

pub mod system_proxy;

/// The local proxy and how this device uses it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Proxy {
    pub enabled: bool,
    pub listen: Listen,
    pub port: u16,
    pub login: Option<String>,
    pub password: Option<String>,
    /// Point the operating system's proxy settings at this port.
    pub system_proxy: bool,
    /// Capture all traffic of the machine with a virtual interface (TUN).
    pub tun: bool,
}
impl Default for Proxy {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: Listen([127, 0, 0, 1].into()),
            port: 20808,
            login: None,
            password: None,
            system_proxy: false,
            tun: false,
        }
    }
}
impl Proxy {
    pub fn address(&self) -> String {
        std::net::SocketAddr::new(self.listen.0, self.port).to_string()
    }
}
pub static PROXY: State<Proxy> = State::new("local-proxy.settings");
/// Whether the tunnel is up (`None` until checked).
pub static TUN_UP: State<Option<bool>> = State::new("local-proxy.tun-up");
/// Result of the last system proxy change: what was set, or why it failed.
pub static SYSTEM_PROXY: State<Option<std::result::Result<String, String>>> =
    State::new("local-proxy.system-proxy");

/// Save the address and login (checked first; an empty password keeps the saved one).
pub struct SaveProxy(pub Proxy);
impl Op for SaveProxy {
    type Reply = ();
    const SAVES: bool = true;
}

struct ReadTun;
/// Apply the configuration again: the core retries the port.
struct RetryPort;
impl Op for RetryPort {
    type Reply = ();
}
impl Op for ReadTun {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Check the tunnel").quiet())
    }
}

/// The mihomo listener of the local proxy.
const LISTENER: &str = "local-proxy";

/// Where the system proxy points: the port when it is on and has no login
/// (system settings cannot pass credentials to SOCKS5).
pub fn system_proxy_address(snap: &Snapshot) -> Option<std::net::SocketAddr> {
    let p = snap.get(&PROXY);
    (device_control() && p.system_proxy && p.enabled && p.login.is_none())
        .then(|| mihomo::local_address(p.listen.0, p.port))
}

/// System proxy integration is unavailable inside Flatpak and Docker.
fn device_control() -> bool {
    !Platform::is_flatpak() && !Platform::is_docker()
}

/// Flatpak runs its authorized tunnel worker on the host.
fn tun_control() -> bool {
    !Platform::is_docker()
}

pub static PLUGIN: Plugin = Plugin {
    id: "local-proxy",
    name: "Local proxy",
    description: "A local HTTP and SOCKS5 proxy for browsers and apps, the system proxy and TUN.",
    setup,
    ..Plugin::DEFAULT
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Listen(pub IpAddr);
impl fmt::Display for Listen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for Listen {
    type Err = std::net::AddrParseError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.trim().trim_matches(['[', ']']).parse().map(Self)
    }
}

/// Why the port is not running (from the last core apply).
pub fn error(status: &homeport_kernel::core::CoreStatus) -> Option<String> {
    status.listener_errors.get(LISTENER).cloned()
}

fn validate(p: &mut Proxy, previous: &Proxy) -> Result<()> {
    if p.port == 0 {
        bail!("Enter a port from 1 to 65535.");
    }
    match &mut p.login {
        Some(login) => {
            *login = login.trim().to_owned();
            if login.is_empty() || login.contains(':') {
                bail!("Enter a login without a colon.");
            }
            if p.password.as_deref().unwrap_or("").is_empty() {
                p.password = previous
                    .login
                    .is_some()
                    .then(|| previous.password.clone())
                    .flatten();
            }
            if p.password.is_none() {
                bail!("Enter a password.");
            }
        }
        None => p.password = None,
    }
    Ok(())
}

async fn save_proxy(cx: Cx, SaveProxy(mut next): SaveProxy) -> Result<()> {
    let p = cx
        .write(move |w| {
            validate(&mut next, &w.get(&PROXY))?;
            w.set(&PROXY, next.clone());
            Ok(next)
        })
        .await?;
    match p.enabled {
        true => info!("Proxy port {}:{} saved", p.listen.0, p.port),
        false => info!("Proxy port turned off"),
    }
    Ok(())
}

// Keep mutation ownership inside the blocking worker. Cancelling a service
// cannot let on_stop restore before its already queued apply has finished.
async fn apply_system_proxy(
    journal: std::path::PathBuf,
    address: Option<std::net::SocketAddr>,
) -> std::result::Result<String, String> {
    static ORDER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = ORDER.lock().await;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        system_proxy::apply(&journal, address)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn setup(p: &mut Setup) -> Result<()> {
    p.editable_setting(&PROXY);
    p.handle(save_proxy);
    p.handle(|cx: Cx, _: RetryPort| async move { cx.core().reapply().await });
    p.live(&TUN_UP, || None);
    p.live(&SYSTEM_PROXY, || None);
    p.handle(read_tun);
    p.every(
        "tun",
        |s| (s.get(&PROXY).tun && tun_control()).then_some(Duration::from_secs(3)),
        || ReadTun,
    );
    if device_control() {
        p.on(restore_after_crash);
    }
    p.service("system-proxy", system_proxy_address, system_proxy)
        .on_stop(|cx: Cx, _| async move {
            let result = apply_system_proxy(journal(&cx), None).await;
            if result.is_ok() {
                info!("System proxy restored");
            }
            cx.set_live(&SYSTEM_PROXY, None);
            result.map(drop).map_err(Error::msg)
        });
    p.core(fragment);
    p.page::<ProxyPage>(PageSpec {
        title: "Local proxy",
        icon: Icon::Listeners,
        order: 50,
    });
    p.fill(&STATUS, "listeners", 20, listeners_chip);
    p.fill(&PANELS, "listeners", 20, listeners_panel);
    Ok(())
}

/// What homeport changed in the system proxy settings, kept across a crash or logoff.
fn journal(cx: &Cx) -> std::path::PathBuf {
    cx.data_dir().join("system-proxy.json")
}

async fn read_tun(cx: Cx, _: ReadTun) -> Result<()> {
    let up = cx.core().tun_enabled().await.ok();
    if up.is_some() && up != *cx.live(&TUN_UP) {
        info!("TUN is {}", if up == Some(true) { "up" } else { "down" });
    }
    cx.set_live(&TUN_UP, up);
    Ok(())
}

/// Undo a system proxy left by a crash, a logoff or a killed process.
async fn restore_after_crash(cx: Cx, _: Started) -> Result<()> {
    let journal = journal(&cx);
    if journal.exists() {
        apply_system_proxy(journal, None)
            .await
            .map_err(Error::msg)?;
        info!("System proxy left by the last run restored");
    }
    Ok(())
}

/// Point the system proxy at the chosen listener while it runs; the previous
/// settings come back when it stops (`on_stop`) or is not running.
async fn system_proxy(cx: Cx, address: std::net::SocketAddr) -> Result<()> {
    let mut changes = cx.core().changes();
    let mut previous = None;
    loop {
        let status = cx.core().status();
        let ready = status.running() && !status.listener_errors.contains_key(LISTENER);
        if previous != Some(ready) {
            previous = Some(ready);
            let result = apply_system_proxy(journal(&cx), ready.then_some(address)).await.and_then(|label| {
                if ready {
                    Ok(label)
                } else {
                    Err("The proxy port is not running. Previous system proxy settings are restored until it starts.".into())
                }
            });
            match &result {
                Ok(_) => info!("System proxy set to {address}"),
                Err(e) => warn!("System proxy not set: {e}"),
            }
            cx.set_live(&SYSTEM_PROXY, Some(result));
        }
        if changes.changed().await.is_err() {
            return Ok(());
        }
    }
}

/// The listener, and the tunnel when traffic capture is on.
fn fragment(snap: &Snapshot) -> Fragment {
    let p = snap.get(&PROXY);
    let tunnel = (p.tun && tun_control()).then_some(mihomo::Tun {
        strict_route: false,
    });
    let listener = p.enabled.then(|| Listener {
        name: LISTENER.into(),
        kind: ListenerKind::Mixed,
        listen: p.listen.0,
        port: p.port,
        udp: true,
        users: p
            .login
            .clone()
            .zip(p.password.clone())
            .into_iter()
            .collect(),
        optional: true,
    });
    let mut f = Fragment::listeners(listener);
    // Domain rules see names in captured traffic. DNS is the DNS plugin's:
    // without it queries leave through the tunnel to the system's server.
    f.sniff = tunnel.is_some();
    f.tun = tunnel;
    f
}

fn listeners_chip(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let p = cx.get(&PROXY);
    let pal = kit::colors(ui);
    let (color, text) = if !p.enabled {
        (pal.faint, "Proxy off".to_owned())
    } else if error(&cx.core_status()).is_some() {
        (pal.danger, "Port failed".to_owned())
    } else {
        (pal.muted, p.address())
    };
    cx.status_chip(
        ui,
        kit::Lead::Icon(Icon::Listeners, color),
        &text,
        "Local proxy (HTTP and SOCKS5)",
    );
}

fn listeners_panel(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let pal = kit::colors(ui);
    homeport_kernel::shell::panel_title_page(ui, cx, "Local proxy", &PLUGIN);
    let p = cx.get(&PROXY);
    if !p.enabled {
        kit::hint(ui, "Off");
        return;
    }
    let failed = error(&cx.core_status());
    kit::row_line(ui, theme::CONTROL, |ui| {
        ui.spacing_mut().item_spacing.x = 6.;
        kit::dot(
            ui,
            if failed.is_some() {
                pal.danger
            } else {
                pal.success
            },
        );
        ui.label(p.address());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            copy_button(ui, cx, &p);
            auth_label(ui, &p);
        });
    });
    if let Some(e) = failed {
        kit::tone_text(ui, e, kit::Tone::Danger);
    }
}

fn copy_button(ui: &mut egui::Ui, cx: &mut UiCx, p: &Proxy) {
    if kit::icon_button(ui, Icon::Copy, "Copy address", p.enabled).clicked() {
        ui.ctx()
            .copy_text(mihomo::local_address(p.listen.0, p.port).to_string());
        cx.notify("Address copied");
    }
}

/// Lock icon and "Login" / "No login".
fn auth_label(ui: &mut egui::Ui, p: &Proxy) {
    let pal = kit::colors(ui);
    let (icon, text) = if p.login.is_some() {
        (Icon::Lock, "Login")
    } else {
        (Icon::Unlock, "No login")
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.;
        kit::icon(ui, icon, 14., pal.muted);
        ui.label(
            egui::RichText::new(text)
                .size(theme::SMALL)
                .color(pal.muted),
        );
    });
}

/// The address and login being edited (the password is only kept here, never shown).
#[derive(Clone, PartialEq)]
struct Draft {
    ip: String,
    port: String,
    auth: bool,
    login: String,
    password: String,
}
impl Draft {
    fn of(p: &Proxy) -> Self {
        Self {
            ip: p.listen.to_string(),
            port: p.port.to_string(),
            auth: p.login.is_some(),
            login: p.login.clone().unwrap_or_default(),
            password: String::new(),
        }
    }
    fn apply(&self, base: &Proxy) -> std::result::Result<Proxy, String> {
        let listen = self
            .ip
            .parse::<Listen>()
            .map_err(|_| "Enter an IPv4 or IPv6 address.".to_owned())?;
        let port = self
            .port
            .trim()
            .parse::<u16>()
            .map_err(|_| "Enter a port from 1 to 65535.".to_owned())?;
        Ok(Proxy {
            listen,
            port,
            login: self.auth.then(|| self.login.clone()),
            password: (self.auth && !self.password.is_empty()).then(|| self.password.clone()),
            ..base.clone()
        })
    }
}

#[derive(Default)]
struct ProxyPage {
    /// Edits not saved yet; `None` shows the saved values.
    draft: Option<Draft>,
    error: Option<String>,
}

impl Page for ProxyPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let saved = (*cx.get(&PROXY)).clone();
        cx.page_header(ui, "", |_, _| {});
        kit::page_scroll(ui, "local-proxy", |ui| {
            self.proxy_card(ui, cx, &saved);
            device_card(ui, cx, &saved);
        });
    }
}

/// Widths of the address fields; a single field spans both.
const IP: f32 = 140.;
const PORT: f32 = 72.;
const FIELD: f32 = IP + theme::HORIZONTAL_GAP + PORT;

impl ProxyPage {
    fn proxy_card(&mut self, ui: &mut egui::Ui, cx: &mut UiCx, saved: &Proxy) {
        match cx.outcome("save") {
            Some(Ok(())) => {
                self.draft = None;
                self.error = None;
            }
            Some(Err(e)) => self.error = Some(e),
            None => {}
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.draft = None;
            self.error = None;
        }
        let mut draft = self.draft.clone().unwrap_or_else(|| Draft::of(saved));
        let mut enter = false;
        let busy = cx.pending("save");
        kit::settings_card(ui, |ui| {
            let mut on = saved.enabled;
            kit::settings_row(ui, "Proxy", "HTTP and SOCKS5 on one port", |ui| {
                if kit::toggle_named(ui, &mut on, "", "Proxy").changed() {
                    cx.update("enabled", &PROXY, move |p| p.enabled = on);
                }
            });
            ui.add_enabled_ui(!busy, |ui| {
                kit::settings_row(
                    ui,
                    "Address",
                    "0.0.0.0 accepts other devices on your network",
                    |ui| {
                        // The copy button leads, so the fields end where the others do.
                        copy_button(ui, cx, saved);
                        let r = kit::input(ui, "ip", &mut draft.ip, "127.0.0.1", IP);
                        enter |= kit::submitted(ui, &r);
                        let r = kit::input(ui, "port", &mut draft.port, "Port", PORT);
                        enter |= kit::submitted(ui, &r);
                    },
                );
                kit::settings_row(ui, "Require login", "", |ui| {
                    kit::toggle_named(ui, &mut draft.auth, "", "Require login");
                });
                if draft.auth {
                    kit::settings_row(ui, "Login", "", |ui| {
                        let r = kit::input(ui, "login", &mut draft.login, "user", FIELD);
                        enter |= kit::submitted(ui, &r);
                    });
                    let has = saved.login.is_some();
                    let help = if has {
                        "Leave empty to keep the saved password"
                    } else {
                        ""
                    };
                    kit::settings_row(ui, "Password", help, |ui| {
                        let hint = if has { "Unchanged" } else { "Password" };
                        let r = kit::password(ui, "password", &mut draft.password, hint, FIELD);
                        enter |= kit::submitted(ui, &r);
                    });
                }
            });
            if saved.enabled
                && let Some(e) = error(&cx.core_status())
            {
                ui.add_space(theme::SPACE_S);
                ui.horizontal(|ui| {
                    kit::tone_text(ui, e, kit::Tone::Danger);
                    // The core could not open the port: retry once it is free.
                    if kit::Button::ghost("Retry")
                        .tone(kit::Tone::Accent)
                        .show(ui)
                        .clicked()
                    {
                        cx.run(RetryPort);
                    }
                });
            }
            if let Some(e) = &self.error {
                ui.add_space(theme::SPACE_S);
                kit::tone_text(ui, e, kit::Tone::Danger);
            }
            let dirty = draft != Draft::of(saved);
            if dirty {
                ui.add_space(theme::SPACE_M);
                // On the right, under the controls they apply.
                let (save, cancel) = ui
                    .allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), theme::CONTROL),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let save = kit::Button::primary("Save")
                                .enabled(!busy)
                                .show(ui)
                                .clicked();
                            (save, kit::Button::ghost("Cancel").show(ui).clicked())
                        },
                    )
                    .inner;
                ui.add_space(theme::SPACE_M);
                if cancel {
                    self.draft = None;
                    self.error = None;
                    return;
                }
                if save || enter {
                    match draft.apply(saved) {
                        Ok(next) => {
                            self.error = None;
                            cx.submit("save", SaveProxy(next));
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            }
            self.draft = dirty.then_some(draft);
        });
    }
}

/// System proxy and TUN: how this computer itself uses homeport.
fn device_card(ui: &mut egui::Ui, cx: &mut UiCx, p: &Proxy) {
    if Platform::is_docker() {
        kit::tone_text(
            ui,
            "System proxy and TUN are not used in Docker",
            kit::Tone::Neutral,
        )
        .on_hover_text("Devices and apps connect to the proxy port directly.");
        return;
    }
    kit::settings_card(ui, |ui| {
        if !Platform::is_flatpak() {
            let mut on = p.system_proxy;
            kit::settings_row(
                ui,
                "System proxy",
                "Points the system proxy settings at this port; the previous settings come back when turned off. Works without a login only.",
                |ui| {
                    if kit::toggle_named(ui, &mut on, "", "System proxy").changed() {
                        cx.update("system-proxy", &PROXY, move |p| p.system_proxy = on);
                    }
                },
            );
            if p.system_proxy {
                let status = match (
                    &*cx.live(&SYSTEM_PROXY),
                    system_proxy_address(cx.snapshot()),
                ) {
                    (_, None) if !p.enabled => ("The proxy is off".into(), kit::Tone::Warning),
                    (_, None) => (
                        "Not set: the proxy requires a login".into(),
                        kit::Tone::Warning,
                    ),
                    (Some(Err(e)), _) => (e.clone(), kit::Tone::Danger),
                    (Some(Ok(what)), _) => (format!("Active · {what}"), kit::Tone::Neutral),
                    (None, _) => ("Applying…".into(), kit::Tone::Neutral),
                };
                kit::tone_text(ui, status.0, status.1);
            }
        }
        let mut tun = p.tun;
        kit::settings_row(
            ui,
            "TUN mode",
            "Route every app through homeport with a virtual network interface (TUN)",
            |ui| {
                ui.add_enabled_ui(!cx.pending("tun"), |ui| {
                    if kit::toggle_named(ui, &mut tun, "", "TUN mode").changed() {
                        cx.update("tun", &PROXY, move |p| p.tun = tun);
                    }
                });
            },
        );
        if cx.pending("tun") {
            kit::tone_text(
                ui,
                "Applying TUN mode… Confirm the system authorization dialog if prompted.",
                kit::Tone::Neutral,
            );
        } else if let homeport_kernel::ui::Request::Done(Err(e)) =
            cx.request::<Update<Proxy>>("tun")
        {
            kit::tone_text(ui, e, kit::Tone::Danger);
        } else if p.tun {
            match (cx.core_status().tun_error, *cx.live(&TUN_UP)) {
                (Some(refused), _) => kit::tone_text(ui, refused, kit::Tone::Danger),
                (None, Some(true)) => kit::tone_text(ui, "Tunnel active", kit::Tone::Neutral),
                (None, Some(false)) => kit::tone_text(
                    ui,
                    "The tunnel could not start. Check the core log, then turn TUN off and on to retry.",
                    kit::Tone::Danger,
                ),
                (None, None) => kit::tone_text(ui, "Starting the tunnel…", kit::Tone::Neutral),
            };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_keeps_password_and_rejects_bad_logins() {
        let saved = Proxy {
            login: Some("u".into()),
            password: Some("secret".into()),
            ..Proxy::default()
        };
        let mut edit = Proxy {
            password: None,
            ..saved.clone()
        };
        validate(&mut edit, &saved).unwrap();
        assert_eq!(edit.password.as_deref(), Some("secret"));
        let mut colon = Proxy {
            login: Some("a:b".into()),
            password: Some("x".into()),
            ..Proxy::default()
        };
        assert!(validate(&mut colon, &saved).is_err());
        let mut open = Proxy {
            password: Some("x".into()),
            ..Proxy::default()
        };
        validate(&mut open, &saved).unwrap();
        assert!(open.password.is_none());
        let mut fresh = Proxy {
            login: Some("new".into()),
            ..Proxy::default()
        };
        assert!(validate(&mut fresh, &Proxy::default()).is_err());
    }
}
