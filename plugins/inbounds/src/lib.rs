//! # inbounds
//! Local HTTP + SOCKS5 listeners (mihomo `mixed`) with optional login.
//! A listener that cannot bind is reported on its row; the others keep working.
//! Also wraps `subscriptions::Fetch`: when a provider is unreachable directly,
//! the download is retried through a healthy listener (i.e. via the selected server).
use plugin_servers as servers;
use plugin_subscriptions::Fetch;
use rproxy_kernel::{
    kit::{self, theme},
    mihomo::{self, Listener, ListenerKind},
    prelude::*,
    sql_text,
};
use std::{collections::HashMap, fmt, net::IpAddr, str::FromStr, time::Duration};

pub mod system_proxy;

/// How this device uses the listeners.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Device {
    /// Capture all traffic of the machine with a virtual interface (TUN).
    pub tun: bool,
    /// The listener the operating system's proxy settings point at.
    pub system_proxy: Option<Id>,
}
pub static DEVICE: State<Device> = State::new("inbounds.device");
/// Whether the tunnel is up (`None` until checked).
pub static TUN_UP: State<Option<bool>> = State::new("inbounds.tun-up");
/// Result of the last system proxy change: what was set, or why it failed.
pub static SYSTEM_PROXY: State<Option<std::result::Result<String, String>>> =
    State::new("inbounds.system-proxy");

struct ReadTun;
impl Op for ReadTun {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed("inbounds/tun", "Check the tunnel").quiet())
    }
}

/// A listener the system proxy can use: on, and without a login (system
/// settings cannot pass credentials to SOCKS5).
pub fn system_proxy_address(snap: &Snapshot) -> Option<std::net::SocketAddr> {
    if !device_control() { return None; }
    let id = snap.get(&DEVICE).system_proxy?;
    snap.get(&INBOUNDS)
        .iter()
        .find(|i| i.id == id && i.enabled && i.login.is_none())
        .map(|i| mihomo::local_address(i.listen.0, i.port))
}

/// System proxy and TUN need the host system: not inside Flatpak or Docker.
fn device_control() -> bool {
    !Platform::is_flatpak() && !Platform::is_docker()
}

pub static PLUGIN: Plugin = Plugin {
    id: "inbounds",
    name: "Listeners",
    description: "Local HTTP and SOCKS5 proxy ports for browsers and apps, with optional login.",
    category: categories::NETWORK,
    requires: &["servers"],
    required: false,
    default_enabled: true,
    platforms: Platform::ALL,
    setup,
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
sql_text!(Listen);

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "inbounds")]
pub struct Inbound {
    pub id: Id,
    pub listen: Listen,
    pub port: u16,
    pub enabled: bool,
    pub login: Option<String>,
    pub password: Option<String>,
}
impl Inbound {
    pub fn address(&self) -> String {
        std::net::SocketAddr::new(self.listen.0, self.port).to_string()
    }
}

pub static INBOUNDS: State<Vec<Inbound>> = State::new("inbounds.list");
static SEEDED: State<bool> = State::new("inbounds.seeded");

/// Why a listener is not running (from the last core apply).
pub fn error(cx_status: &rproxy_kernel::core::CoreStatus, i: &Inbound) -> Option<String> {
    cx_status.listener_errors.get(&i.id.to_string()).cloned()
}

fn validate(i: &mut Inbound, previous: Option<&Inbound>, others: &[Inbound]) -> Result<()> {
    if i.port == 0 || others.iter().any(|o| o.port == i.port) {
        bail!("Choose an unused port from 1 to 65535.");
    }
    match &mut i.login {
        Some(login) => {
            *login = login.trim().to_owned();
            if login.is_empty() || login.contains(':') {
                bail!("Enter a login without a colon.");
            }
            if i.password.as_deref().unwrap_or("").is_empty() {
                // An empty password keeps the saved one.
                i.password = previous.filter(|p| p.login.is_some()).and_then(|p| p.password.clone());
            }
            if i.password.is_none() {
                bail!("Enter a password.");
            }
        }
        None => i.password = None,
    }
    Ok(())
}

// Keep mutation ownership inside the blocking worker. Cancelling a service
// cannot let on_stop restore before its already queued apply has finished.
async fn apply_system_proxy(journal: std::path::PathBuf, address: Option<std::net::SocketAddr>) -> std::result::Result<String, String> {
    static ORDER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = ORDER.lock().await;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        system_proxy::apply(&journal, address)
    }).await.map_err(|e| e.to_string())?
}

/// macOS: install the setuid copy of the core that can create the tunnel.
struct GrantTun;
impl Op for GrantTun {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::keyed("inbounds/grant-tun", "Grant tunnel access").page("inbounds"))
    }
}

fn setup(p: &mut Setup) -> Result<()> {
    // What rproxy changed in the system proxy settings, kept across a crash or logoff.
    let journal = p.data_dir().join("system-proxy.json");
    p.list(&INBOUNDS).validate(|_, i, prev, others| validate(i, prev, others));
    p.setting(&SEEDED);
    // First start: one local listener on 127.0.0.1:20808. If that port is busy the
    // listener is still created, disabled, so it can be enabled once the port is free.
    p.on(|cx: Cx, _: Started| async move {
        if *cx.get(&SEEDED) {
            return Ok(());
        }
        let seed = |enabled: bool| {
            let cx = cx.clone();
            async move {
                cx.write(move |w| {
                    if w.get(&INBOUNDS).is_empty() {
                        w.update(&INBOUNDS, |l| {
                            l.push(Inbound { id: Id::random(), listen: Listen([127, 0, 0, 1].into()), port: 20808, enabled, login: None, password: None })
                        });
                    }
                    w.set(&SEEDED, true);
                    Ok(())
                })
                .await
            }
        };
        if let Err(e) = seed(true).await {
            seed(false).await?;
            cx.kernel().notify_error_at(cx.plugin_name(), &format!("The default listener 127.0.0.1:20808 is off: {e}"), Some("inbounds"));
        }
        Ok(())
    });
    p.editable_setting(&DEVICE);
    p.live(&TUN_UP, || None);
    p.live(&SYSTEM_PROXY, || None);
    p.handle(|cx: Cx, _: ReadTun| async move {
        let up = cx
            .core()
            .configs()
            .await
            .ok()
            .map(|c| c["tun"]["enable"].as_bool().unwrap_or(false));
        cx.set_live(&TUN_UP, up);
        Ok(())
    });
    p.handle(|cx: Cx, _: GrantTun| async move { cx.kernel().grant_core_access().await });
    p.every(
        "tun",
        |s| (s.get(&DEVICE).tun && device_control()).then_some(Duration::from_secs(3)),
        || ReadTun,
    );
    // Undo a system proxy left by a crash, a logoff or a killed process.
    if device_control() {
        let journal = journal.clone();
        p.on(move |_: Cx, _: Started| {
            let journal = journal.clone();
            async move {
                if journal.exists() {
                    apply_system_proxy(journal, None).await.map_err(Error::msg)?;
                }
                Ok(())
            }
        });
    }
    // Point the system proxy at the chosen listener; put it back when the choice
    // changes or the app quits (only if rproxy had changed it).
    let stop_journal = journal.clone();
    p.service("system-proxy", |s| Some((s.get(&DEVICE).system_proxy?, system_proxy_address(s)?)), move |cx: Cx, (id, address): (Id, std::net::SocketAddr)| {
        let journal = journal.clone();
        async move {
        let mut changes = cx.kernel().changes();
        let mut previous = None;
        loop {
            let status = cx.core().status();
            let ready = status.running() && !status.listener_errors.contains_key(&id.to_string());
            if previous != Some(ready) {
                previous = Some(ready);
                let result = apply_system_proxy(journal.clone(), ready.then_some(address)).await;
                let result = result.and_then(|label| if ready { Ok(label) } else {
                    Err("The selected listener is not running. Previous system proxy settings are restored until it becomes available.".into())
                });
                cx.set_live(&SYSTEM_PROXY, Some(result));
            }
            if changes.changed().await.is_err() { return Ok(()); }
        }
    }})
    .on_stop(move |cx: Cx, _| {
        let journal = stop_journal.clone();
        async move {
        let result = apply_system_proxy(journal, None).await;
        cx.set_live(&SYSTEM_PROXY, None);
        result.map(drop).map_err(Error::msg)
    }});
    p.core(|snap| {
        let device = snap.get(&DEVICE);
        let tunnel = (device.tun && device_control()).then_some(mihomo::Tun {
            strict_route: false,
        });
        let mut f =
            Fragment::listeners(snap.get(&INBOUNDS).iter().filter(|i| i.enabled).map(|i| {
                Listener {
                    name: i.id.to_string(),
                    kind: ListenerKind::Mixed,
                    listen: i.listen.0,
                    port: i.port,
                    udp: true,
                    users: i
                        .login
                        .clone()
                        .zip(i.password.clone())
                        .into_iter()
                        .collect(),
                    optional: true,
                }
            }));
        if tunnel.is_some() {
            // The tunnel answers DNS itself; the system resolver is the fallback.
            f.dns = Some(mihomo::Dns {
                enable: true,
                nameservers: vec!["https://1.1.1.1/dns-query".into(), "system".into()],
            });
            f.sniff = true;
        }
        f.tun = tunnel;
        f
    });
    // Unreachable provider: retry through a working listener (traffic leaves via the selected server).
    p.wrap(|cx: Cx, f: Fetch, next: Next<Fetch>| async move {
        let direct = next.run(&cx, f.clone()).await;
        let Err(e) = &direct else { return direct };
        if e.cancelled() || matches!(e.code.as_str(), "device-limit" | "device-id") || !f.url.starts_with("http") || cx.get(&servers::SELECTED).is_none() {
            return direct;
        }
        // Retry the way user traffic goes: through the selected server.
        let Ok(routed) = cx.core().routed_http().await else { return direct };
        plugin_subscriptions::fetch_http(&cx, &routed, &f.url).await
    });

    p.page::<InboundsPage>(PageSpec { id: "inbounds", title: "Listeners", icon: Icon::Listeners, order: 50 });
    p.fill(&STATUS, "listeners", 20, |ui, cx, _| {
        let list = cx.get(&INBOUNDS);
        let status = cx.core_status();
        let enabled: Vec<&Inbound> = list.iter().filter(|i| i.enabled).collect();
        let failed = enabled.iter().filter(|i| error(&status, i).is_some()).count();
        let pal = kit::colors(ui);
        let (color, text) = match (enabled.first(), failed) {
            (None, _) => (pal.faint, "No listener".to_owned()),
            (_, f) if f > 0 => (pal.danger, format!("{} failed", kit::plural(f, "listener", "listeners"))),
            (Some(i), _) if enabled.len() == 1 => (pal.muted, i.address()),
            (Some(_), _) => (pal.muted, kit::plural(enabled.len(), "listener", "listeners")),
        };
        if kit::chip(ui, Some(kit::Lead::Icon(Icon::Listeners, color)), &text, "Local HTTP / SOCKS5 listeners").clicked() {
            cx.toggle_panels();
        }
    });
    p.fill(&PANELS, "listeners", 20, |ui, cx, _| {
        let pal = kit::colors(ui);
        let status = cx.core_status();
        rproxy_kernel::shell::panel_title_page(ui, cx, "Listeners · HTTP and SOCKS5", "inbounds");
        let list = cx.get(&INBOUNDS);
        if !list.iter().any(|i| i.enabled) {
            kit::hint(ui, "No listener is on.");
        }
        for i in list.iter().filter(|i| i.enabled) {
            kit::row_line(ui, theme::CONTROL, |ui| {
                ui.spacing_mut().item_spacing.x = 6.;
                kit::dot(ui, if error(&status, i).is_some() { pal.danger } else { pal.success });
                ui.label(i.address());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if kit::icon_button(ui, Icon::Copy, "Copy address", true).clicked() {
                        ui.ctx().copy_text(mihomo::local_address(i.listen.0, i.port).to_string());
                        cx.notify("Address copied");
                    }
                    auth_label(ui, i);
                });
            });
            if let Some(e) = error(&status, i) {
                kit::tone_text(ui, e, kit::Tone::Danger);
            }
        }
    });
    Ok(())
}

/// Lock icon and "Login" / "No auth" (left to right in any parent layout).
fn auth_label(ui: &mut egui::Ui, i: &Inbound) {
    let pal = kit::colors(ui);
    let (icon, text) = if i.login.is_some() { (Icon::Lock, "Login") } else { (Icon::Unlock, "No auth") };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.;
        kit::icon(ui, icon, 14., pal.muted);
        ui.label(egui::RichText::new(text).size(theme::SMALL).color(pal.muted));
    });
}

/// Text fields of a listener being edited (the password is only kept here, never shown).
#[derive(Clone)]
struct Draft {
    ip: String,
    port: String,
    auth: bool,
    login: String,
    password: String,
}
impl Draft {
    fn of(i: &Inbound) -> Self {
        Self { ip: i.listen.to_string(), port: i.port.to_string(), auth: i.login.is_some(), login: i.login.clone().unwrap_or_default(), password: String::new() }
    }
    fn fresh(rows: &[Inbound]) -> Self {
        let port = (1080..=u16::MAX).find(|p| !rows.iter().any(|r| r.port == *p)).unwrap_or(1080);
        Self { ip: "127.0.0.1".into(), port: port.to_string(), auth: false, login: String::new(), password: String::new() }
    }
    fn apply(&self, base: &Inbound) -> std::result::Result<Inbound, String> {
        let listen = self.ip.parse::<Listen>().map_err(|_| "Enter an IPv4 or IPv6 address.".to_owned())?;
        let port = self.port.trim().parse::<u16>().map_err(|_| "Enter a port from 1 to 65535.".to_owned())?;
        Ok(Inbound {
            listen,
            port,
            login: self.auth.then(|| self.login.clone()),
            password: (self.auth && !self.password.is_empty()).then(|| self.password.clone()),
            ..base.clone()
        })
    }
}

#[derive(Default)]
struct InboundsPage {
    editing: HashMap<Id, Draft>,
    adding: Option<Draft>,
    add_error: Option<String>,
    errors: HashMap<Id, String>,
}

/// Toggle · address · auth · edit · delete.
const TOGGLE: f32 = 30.;
const AUTH: f32 = 84.;

/// TUN hint: what to do when the tunnel cannot start on this platform.
fn tun_hint(core: &std::path::Path) -> String {
    if Platform::appimage().is_some() {
        "The AppImage filesystem is read-only. Install the native package, or extract the AppImage to a writable folder before granting the core network-admin rights.".into()
    } else if cfg!(windows) {
        "The tunnel needs administrator rights: start rproxy as administrator.".into()
    } else if cfg!(target_os = "macos") {
        "The tunnel needs administrator rights for the core.".into()
    } else if core.starts_with("/opt/rproxy") {
        // The pacman package grants them on install; an update or a manual copy loses them.
        format!("The tunnel needs network admin rights. Reinstall the package, or run once: sudo setcap cap_net_admin,cap_net_bind_service+ep {}", core.display())
    } else {
        format!("The tunnel needs network admin rights. Run once: sudo setcap cap_net_admin,cap_net_bind_service+ep {}", core.display())
    }
}

/// System proxy and TUN: how this computer itself uses rproxy.
fn device_card(ui: &mut egui::Ui, cx: &mut UiCx, rows: &[Inbound]) {
    if Platform::is_flatpak() {
        kit::tone_text(ui, "System proxy and TUN unavailable in Flatpak", kit::Tone::Neutral)
            .on_hover_text("Available in the native package. Apps can still connect to the listeners.");
        return;
    }
    if Platform::is_docker() {
        kit::tone_text(ui, "System proxy and TUN are not used in Docker", kit::Tone::Neutral)
            .on_hover_text("Devices and apps connect to the listeners directly.");
        return;
    }
    let device = (*cx.get(&DEVICE)).clone();
    let eligible: Vec<&Inbound> = rows
        .iter()
        .filter(|i| i.enabled && i.login.is_none())
        .collect();
    kit::settings_card(ui, |ui| {
        let current = device
            .system_proxy
            .and_then(|id| rows.iter().find(|i| i.id == id));
        let label = current.map_or_else(|| "Off".to_owned(), |i| i.address());
        kit::settings_row(
            ui,
            "System proxy",
            "Requires a listener without authentication. Restores previous proxy settings when disabled.",
            |ui| {
                kit::select(ui, "system-proxy", &label, 170., |ui| {
                    if ui
                        .selectable_label(device.system_proxy.is_none(), "Off")
                        .clicked()
                    {
                        cx.update("system-proxy", &DEVICE, |d| d.system_proxy = None);
                    }
                    for i in &eligible {
                        let id = i.id;
                        if ui
                            .selectable_label(device.system_proxy == Some(id), i.address())
                            .clicked()
                        {
                            cx.update("system-proxy", &DEVICE, move |d| d.system_proxy = Some(id));
                        }
                    }
                });
            },
        );
        if device.system_proxy.is_some() {
            match (&*cx.live(&SYSTEM_PROXY), system_proxy_address(cx.snapshot())) {
                (_, None) => kit::tone_text(
                    ui,
                    "The chosen listener is off or needs a login, so the system proxy is not set.",
                    kit::Tone::Warning,
                ),
                (Some(Err(e)), _) => kit::tone_text(ui, e, kit::Tone::Danger),
                (Some(Ok(what)), _) => kit::tone_text(
                    ui,
                    format!("Active · {what}"),
                    kit::Tone::Neutral,
                ),
                (None, _) => kit::tone_text(ui, "Applying…", kit::Tone::Neutral),
            };
        }
        let mut tun = device.tun;
        kit::settings_row(
            ui,
            "Capture all traffic",
            "Route every app through rproxy with a virtual network interface (TUN)",
            |ui| {
                if kit::toggle(ui, &mut tun, "").changed() {
                    cx.update("tun", &DEVICE, move |d| d.tun = tun);
                }
            },
        );
        if device.tun {
            match *cx.live(&TUN_UP) {
                Some(true) => kit::tone_text(
                    ui,
                    "Tunnel active",
                    kit::Tone::Neutral,
                ),
                Some(false) => {
                    let core = cx.core().executable();
                    // The setuid copy already runs: rights are not what is missing.
                    let grant = cfg!(target_os = "macos") && !core.ends_with("mihomo-tun");
                    let hint = kit::tone_text(ui, tun_hint(&core), kit::Tone::Danger);
                    if grant {
                        let busy = cx.pending("grant-tun");
                        if kit::Button::secondary("Grant access").enabled(!busy).show(ui).on_hover_text("Asks for an administrator password once, so the core can create the tunnel.").clicked() {
                            cx.submit("grant-tun", GrantTun);
                        }
                    }
                    hint
                }
                None => kit::tone_text(ui, "Starting the tunnel…", kit::Tone::Neutral),
            };
        }
    });
}

impl Page for InboundsPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let rows = cx.get(&INBOUNDS);
        let status = cx.core_status();
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.editing.clear();
        }
        let counter = if rows.is_empty() { String::new() } else { kit::plural(rows.len(), "listener", "listeners") };
        kit::page_header(ui, "Listeners", &counter, |_| {});

        device_card(ui, cx, &rows);
        ui.add_space(theme::SECTION_GAP);
        // Add bar: address and port; authentication is set in the row editor.
        let add_key = ("inbound", "add");
        match cx.outcome(add_key) {
            Some(Ok(())) => {
                self.adding = None;
                self.add_error = None;
            }
            Some(Err(e)) => self.add_error = Some(e),
            None => {}
        }
        let draft = self.adding.get_or_insert_with(|| Draft::fresh(&rows));
        let busy = cx.pending(add_key);
        if kit::add_bar(ui, "Add listener", !busy, |ui, width| {
            let port_w = 84.;
            let ip_w = (width - port_w - theme::HORIZONTAL_GAP).max(80.);
            let a = kit::input(ui, "new-ip", &mut draft.ip, "Address, e.g. 127.0.0.1", ip_w, false).on_hover_text("0.0.0.0 accepts connections from other devices.");
            let b = kit::input(ui, "new-port", &mut draft.port, "Port", port_w, false);
            kit::submitted(ui, &a) || kit::submitted(ui, &b)
        }) {
            let base = Inbound { id: Id::random(), listen: Listen([127, 0, 0, 1].into()), port: 0, enabled: true, login: None, password: None };
            match draft.apply(&base) {
                Ok(next) => {
                    self.add_error = None;
                    cx.submit(add_key, Add::new(next));
                }
                Err(e) => self.add_error = Some(e),
            }
        }
        if let Some(e) = &self.add_error {
            kit::tone_text(ui, e, kit::Tone::Danger);
            ui.add_space(theme::SPACE_M);
        }
        kit::list_card(ui, |ui| {
            if rows.is_empty() {
                kit::empty(ui, Icon::Listeners, "No listeners");
                return;
            }
            let actions = 2. * theme::CONTROL;
            let gap = theme::HORIZONTAL_GAP;
            kit::list_header(ui, |ui| {
                let w = ui.available_width() - TOGGLE - AUTH - actions - 3. * gap;
                kit::cell(ui, TOGGLE, |ui| kit::caption(ui, "On"));
                kit::cell(ui, w, |ui| kit::caption(ui, "Address"));
                kit::cell(ui, AUTH, |ui| kit::caption(ui, "Auth"));
            });
            for i in rows.iter() {
                ui.push_id(i.id, |ui| self.row(ui, cx, &status, i, actions));
            }
        });
    }
}

impl InboundsPage {
    fn row(&mut self, ui: &mut egui::Ui, cx: &mut UiCx, status: &rproxy_kernel::core::CoreStatus, i: &Inbound, actions: f32) {
        let key = ("inbound", i.id);
        match cx.outcome(key) {
            Some(Ok(())) => {
                self.errors.remove(&i.id);
                self.editing.remove(&i.id);
            }
            Some(Err(e)) => {
                self.errors.insert(i.id, e);
            }
            None => {}
        }
        let busy = cx.pending(key);
        let open = self.editing.contains_key(&i.id);
        kit::row(ui, i.id, false, |ui, row| {
            let emphasis = if row.hovered || open { kit::Emphasis::Normal } else { kit::Emphasis::Quiet };
            ui.add_enabled_ui(!busy, |ui| {
                kit::row_line(ui, theme::ROW, |ui| {
                    let gap = theme::HORIZONTAL_GAP;
                    let w = ui.available_width() - TOGGLE - AUTH - actions - 3. * gap;
                    let mut on = i.enabled;
                    kit::cell(ui, TOGGLE, |ui| {
                        if kit::toggle_named(ui, &mut on, "", &i.address()).changed() {
                            cx.submit(key, Save(Inbound { enabled: on, ..i.clone() }));
                        }
                    });
                    let pal = kit::colors(ui);
                    kit::cell(ui, w, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(i.address()).color(if i.enabled { pal.foreground } else { pal.muted })).truncate());
                    });
                    kit::cell(ui, AUTH, |ui| auth_label(ui, i));
                    ui.spacing_mut().item_spacing.x = 0.;
                    if kit::icon_button_ex(ui, if open { Icon::Close } else { Icon::Edit }, if open { "Close editor" } else { "Edit address and login" }, true, emphasis, false).clicked() {
                        if open {
                            self.editing.remove(&i.id);
                        } else {
                            self.editing.insert(i.id, Draft::of(i));
                        }
                    }
                    if kit::remove(ui, emphasis) {
                        cx.submit(key, Remove::<Inbound>::new(i.id));
                    }
                });
                if let Some(e) = error(status, i).filter(|_| i.enabled) {
                    ui.horizontal(|ui| {
                        ui.add_space(TOGGLE + theme::HORIZONTAL_GAP);
                        kit::tone_text(ui, e, kit::Tone::Danger);
                        if kit::Button::ghost("Retry").tone(kit::Tone::Accent).show(ui).clicked() {
                            let k = cx.kernel().clone();
                            tokio::spawn(async move {
                                let _ = k.cx().core().reapply().await;
                            });
                        }
                    });
                    ui.add_space(4.);
                }
                if let Some(e) = self.errors.get(&i.id) {
                    ui.horizontal(|ui| {
                        ui.add_space(TOGGLE + theme::HORIZONTAL_GAP);
                        kit::tone_text(ui, e, kit::Tone::Danger);
                    });
                    ui.add_space(4.);
                }
                if let Some(draft) = self.editing.get_mut(&i.id) {
                    let mut save = false;
                    let mut cancel = false;
                    ui.horizontal(|ui| {
                        ui.add_space(TOGGLE + theme::HORIZONTAL_GAP);
                        let pal = kit::colors(ui);
                        egui::Frame::NONE
                            .fill(if pal.dark { pal.background } else { pal.hover })
                            .stroke(egui::Stroke::new(1_f32, pal.border))
                            .corner_radius(theme::RADIUS_CONTROL)
                            .inner_margin(egui::Margin::symmetric(10, 2))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width() - 4.);
                                ui.vertical(|ui| {
                                    save = editor(ui, draft, i.login.is_some());
                                    ui.horizontal(|ui| {
                                        save |= kit::action(ui, "Save", true);
                                        cancel = kit::button(ui, "Cancel", 0., true).clicked();
                                    });
                                    ui.add_space(8.);
                                });
                            });
                    });
                    ui.add_space(6.);
                    if cancel {
                        self.editing.remove(&i.id);
                    } else if save {
                        match draft.apply(i) {
                            Ok(next) => cx.submit(key, Save(next)),
                            Err(e) => {
                                self.errors.insert(i.id, e);
                            }
                        }
                    }
                }
            });
        });
    }
}

/// Address and authentication fields. Returns true on Enter.
fn editor(ui: &mut egui::Ui, d: &mut Draft, has_password: bool) -> bool {
    let mut enter = false;
    kit::settings_start(ui);
    kit::settings_row(ui, "Address", "0.0.0.0 accepts other devices on your network", |ui| {
        let r = kit::input(ui, "ip", &mut d.ip, "127.0.0.1", 140., false);
        enter |= kit::submitted(ui, &r);
        let r = kit::input(ui, "port", &mut d.port, "Port", 72., false);
        enter |= kit::submitted(ui, &r);
    });
    kit::settings_row(ui, "Require login", "", |ui| {
        kit::toggle(ui, &mut d.auth, "");
    });
    if d.auth {
        kit::settings_row(ui, "Login", "", |ui| {
            let r = kit::input(ui, "login", &mut d.login, "user", 218., false);
            enter |= kit::submitted(ui, &r);
        });
        kit::settings_row(ui, "Password", if has_password { "Leave empty to keep the saved password" } else { "" }, |ui| {
            let hint = if has_password { "Unchanged" } else { "Password" };
            let r = kit::input(ui, "password", &mut d.password, hint, 218., true);
            enter |= kit::submitted(ui, &r);
        });
    }
    ui.add_space(2.);
    enter
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inbound(port: u16) -> Inbound {
        Inbound { id: Id::random(), listen: Listen([127, 0, 0, 1].into()), port, enabled: true, login: None, password: None }
    }
    #[test]
    fn validation_keeps_password_and_rejects_conflicts() {
        let mut saved = inbound(1080);
        saved.login = Some("u".into());
        saved.password = Some("secret".into());
        let mut edit = Inbound { password: None, ..saved.clone() };
        validate(&mut edit, Some(&saved), &[]).unwrap();
        assert_eq!(edit.password.as_deref(), Some("secret"));
        let mut clash = inbound(1080);
        assert!(validate(&mut clash, None, &[saved.clone()]).is_err());
        let mut colon = Inbound { login: Some("a:b".into()), password: Some("x".into()), ..inbound(2000) };
        assert!(validate(&mut colon, None, &[]).is_err());
        let mut open = Inbound { password: Some("x".into()), ..inbound(2001) };
        validate(&mut open, None, &[]).unwrap();
        assert!(open.password.is_none());
    }
}
