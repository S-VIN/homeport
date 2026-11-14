//! # exit-server
//! A second hop after the selected server: this device → selected server (entry)
//! → exit server → internet. The entry is whatever the user or Auto select
//! connects to; the exit is one stable server added on this plugin's page.
//!
//! Mechanics: the exit becomes the mihomo proxy `exit`, dialed through the
//! `main` group (`dialer-proxy`), and answers `servers::ROUTE`, so every rule
//! that sends traffic "via server" goes through both hops. It fails closed:
//! with no entry, or an exit that cannot be used, traffic is blocked rather
//! than sent past the exit.
//!
//! Cloudflare WARP can be the exit, or the backup exit when the exit server
//! does not answer (order: exit → WARP → blocked). The entry can also be "this
//! machine": on a node that serves clients, the exit is then dialed directly,
//! so client traffic never leaves from the node's own address.
use kit::Tone;
use plugin_servers as servers;
use rproxy_kernel::{
    kit::{self, theme},
    mihomo,
    prelude::*,
    ui::{Column, ListEditor},
};
use serde_json::json;
use std::time::Duration;

pub static PLUGIN: Plugin = Plugin {
    id: "exit-server",
    name: "Proxy chain",
    description: "Send proxied traffic through the current server, then one stable exit server.",
    // Links are parsed with format-uri's parser; the plugin itself may be off (a node).
    requires: &["servers"],
    setup,
    ..Plugin::DEFAULT
};

pub mod warp;

/// mihomo names of the exit proxy, the WARP proxy and the exit-then-WARP group.
pub const EXIT: &str = "exit";
pub const WARP: &str = "warp";
pub const EXIT_ROUTE: &str = "exit-route";
const PROBE_URL: &str = "https://www.gstatic.com/generate_204";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "exit_servers")]
pub struct Exit {
    pub id: Id,
    pub name: String,
    /// The server's share link (`vless://…`); the single source of its settings.
    pub link: String,
}
impl Exit {
    pub fn address(&self) -> String {
        proxy(&self.link)
            .map(|p| format!("{}:{}", p.server, p.port))
            .unwrap_or_else(|_| "Invalid link".into())
    }
}

pub static EXITS: State<Vec<Exit>> = State::new("exit-server.list");
/// The exit in use; `None` = off (one hop, as without this plugin).
pub static ACTIVE: State<Option<Id>> = State::new("exit-server.active");
/// Last end-to-end probe through both hops: `(result, unix time)`.
pub static PROBE: State<Option<(std::result::Result<u32, String>, u64)>> =
    State::new("exit-server.probe");

choices!(pub enum Entry {
    Selected => "Current server" | "Selected server",
    Direct => "Directly" | "This machine",
});
choices!(pub enum WarpUse {
    Off => "Off",
    Backup => "If exit fails" | "Backup",
    Only => "Always" | "Exit",
});
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// What the exit is dialed through.
    pub entry: Entry,
    pub warp: WarpUse,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            entry: Entry::Selected,
            warp: WarpUse::Off,
        }
    }
}
pub static SETTINGS: State<Settings> = State::new("exit-server.settings");
/// The registered WARP device, if set up.
pub static WARP_ACCOUNT: State<Option<warp::Account>> = State::new("exit-server.warp");

/// Register a WARP device (job "Set up WARP").
pub struct SetUpWarp;
impl Op for SetUpWarp {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Set up WARP").page("exit-server"))
    }
}
/// Forget the WARP device.
pub struct RemoveWarp;
impl Op for RemoveWarp {
    type Reply = ();
}

/// What carries traffic after the entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Exit(Exit),
    Warp,
    /// The exit server; WARP when it does not answer.
    ExitThenWarp(Exit),
}
impl Plan {
    /// The mihomo name traffic is sent to.
    pub fn target(&self) -> &'static str {
        match self {
            Self::Exit(_) => EXIT,
            Self::Warp => WARP,
            Self::ExitThenWarp(_) => EXIT_ROUTE,
        }
    }
    pub fn name(&self) -> String {
        match self {
            Self::Exit(e) => e.name.clone(),
            Self::Warp => "Cloudflare WARP".into(),
            Self::ExitThenWarp(e) => format!("{} (WARP if it fails)", e.name),
        }
    }
}
pub fn plan(snap: &Snapshot) -> Option<Plan> {
    let warp = snap.get(&WARP_ACCOUNT).is_some();
    let use_warp = snap.get(&SETTINGS).warp;
    match (active(snap), warp, use_warp) {
        (_, true, WarpUse::Only) => Some(Plan::Warp),
        (Some(e), true, WarpUse::Backup) => Some(Plan::ExitThenWarp(e)),
        (Some(e), _, _) => Some(Plan::Exit(e)),
        (None, true, WarpUse::Backup) => Some(Plan::Warp),
        _ => None,
    }
}
fn through_entry(snap: &Snapshot) -> bool {
    snap.get(&SETTINGS).entry == Entry::Selected
}

/// Use an exit server, or turn the second hop off with `None`.
pub struct Use(pub Option<Id>);
impl Op for Use {
    type Reply = ();
    const SAVES: bool = true;
}
/// Measure latency through the entry and the exit (job "Check exit").
pub struct CheckExit;
impl Op for CheckExit {
    type Reply = u32;
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Test proxy chain").quiet().page("exit-server"))
    }
}

fn proxy(link: &str) -> Result<servers::Proxy> {
    let mut list = plugin_format_uri::parse_links(link.trim())?;
    match (list.pop(), list.is_empty()) {
        (Some(p), true) => Ok(p),
        _ => bail!("Paste one server link."),
    }
}

pub fn active(snap: &Snapshot) -> Option<Exit> {
    let id = (*snap.get(&ACTIVE))?;
    snap.get(&EXITS).iter().find(|e| e.id == id).cloned()
}

fn setup(p: &mut Setup) -> Result<()> {
    p.list(&EXITS).validate(|_, e, _, others| {
        e.link = e.link.trim().to_owned();
        let px = proxy(&e.link)?;
        e.name = e.name.trim().to_owned();
        if e.name.is_empty() {
            e.name = if px.name.trim().is_empty() {
                px.server.clone()
            } else {
                px.name.trim().to_owned()
            };
        }
        if others.iter().any(|o| o.link == e.link) {
            bail!("This exit server is already added.");
        }
        Ok(())
    });
    p.setting(&ACTIVE);
    p.editable_setting(&SETTINGS);
    p.setting(&WARP_ACCOUNT);
    p.live(&PROBE, || None);
    p.handle(set_up_warp);
    p.handle(remove_warp);
    // The first exit added is the one in use.
    p.on(|cx: Cx, e: Added<Exit>| async move {
        if cx.get(&ACTIVE).is_none() {
            cx.call(Use(Some(e.item.id))).await?;
        }
        Ok(())
    });
    p.on(|cx: Cx, e: Removed<Exit>| async move {
        if *cx.get(&ACTIVE) == Some(e.item.id) {
            cx.call(Use(None)).await?;
        }
        Ok(())
    });
    p.handle(use_exit);
    p.handle(check_exit);
    p.every(
        "check",
        // Only while there is something to check: no entry is shown on the page, not as an error.
        |s| {
            plan(s)
                .filter(|_| !through_entry(s) || s.get(&servers::SELECTED).is_some())
                .map(|_| Duration::from_secs(120))
        },
        || CheckExit,
    );
    p.wrap(redial_on_new_entry);
    p.core(fragment);
    p.hook(&servers::ROUTE, |snap, _| route(snap));
    ui(p);
    Ok(())
}

async fn set_up_warp(cx: Cx, _: SetUpWarp) -> Result<()> {
    let account = warp::register(&cx).await?;
    cx.write(move |w| {
        w.set(&WARP_ACCOUNT, Some(account));
        if w.get(&SETTINGS).warp == WarpUse::Off {
            let warp = if active(w.base()).is_some() {
                WarpUse::Backup
            } else {
                WarpUse::Only
            };
            w.update(&SETTINGS, |s| s.warp = warp);
        }
        Ok(())
    })
    .await?;
    cx.set_live(&PROBE, None);
    cx.summary("WARP is ready");
    cx.spawn(CheckExit)?;
    Ok(())
}

async fn remove_warp(cx: Cx, _: RemoveWarp) -> Result<()> {
    cx.write(|w| {
        w.set(&WARP_ACCOUNT, None);
        w.update(&SETTINGS, |s| s.warp = WarpUse::Off);
        Ok(())
    })
    .await?;
    cx.set_live(&PROBE, None);
    Ok(())
}

async fn use_exit(cx: Cx, Use(id): Use) -> Result<()> {
    cx.write(move |w| {
        if id.is_some_and(|id| !w.get(&EXITS).iter().any(|e| e.id == id)) {
            bail!("This exit server no longer exists.");
        }
        w.set(&ACTIVE, id);
        Ok(())
    })
    .await?;
    cx.set_live(&PROBE, None);
    if id.is_some() {
        cx.spawn(CheckExit)?;
    }
    Ok(())
}

async fn check_exit(cx: Cx, _: CheckExit) -> Result<u32> {
    let snap = cx.snapshot();
    let Some(plan) = plan(&snap) else {
        bail!("No exit server is in use.");
    };
    if through_entry(&snap) && snap.get(&servers::SELECTED).is_none() {
        bail!("Connect to a server first: it is the entry to the exit server.");
    }
    let result = cx
        .core()
        .delay(plan.target(), PROBE_URL, 6000)
        .await
        .map_err(|e| e.message);
    cx.set_live(&PROBE, Some((result.clone(), now())));
    result.map_err(|_| Error::msg("The exit did not answer."))
}

/// The exit keeps its transport connections (XHTTP reuse) open for a long
/// time. A new entry or a disconnect must take effect now: rebuild the exit
/// so new connections are dialed through the current entry, or not at all.
async fn redial_on_new_entry(
    cx: Cx,
    op: servers::Select,
    next: Next<servers::Select>,
) -> Result<()> {
    let before = *cx.get(&servers::SELECTED);
    next.run(&cx, op).await?;
    let snap = cx.snapshot();
    if *snap.get(&servers::SELECTED) != before && plan(&snap).is_some() && through_entry(&snap) {
        cx.core().reapply().await?;
    }
    Ok(())
}

fn fragment(snap: &Snapshot) -> Fragment {
    let Some(plan) = plan(snap) else {
        return Fragment::default();
    };
    let dial = through_entry(snap).then_some(servers::MAIN);
    let mut f = Fragment::default();
    if let Plan::Exit(e) | Plan::ExitThenWarp(e) = &plan {
        let Ok(px) = proxy(&e.link) else { return f };
        let mut options = px.options;
        if let Some(via) = dial {
            options.insert("dialer-proxy".into(), json!(via));
        }
        f.proxies.push(mihomo::Proxy {
            name: EXIT.into(),
            protocol: px.protocol.to_string(),
            server: px.server,
            port: px.port,
            options,
        });
    }
    if let (Plan::Warp | Plan::ExitThenWarp(_), Some(account)) = (&plan, &*snap.get(&WARP_ACCOUNT))
    {
        f.proxies.push(warp::proxy(WARP, account, dial));
    }
    if matches!(plan, Plan::ExitThenWarp(_)) {
        f.groups.push(mihomo::Group {
            name: EXIT_ROUTE.into(),
            kind: mihomo::GroupKind::Fallback {
                url: PROBE_URL.into(),
                interval_secs: 60,
            },
            members: vec![Target::Proxy(EXIT.into()), Target::Proxy(WARP.into())],
            selected: None,
        });
    }
    f
}

/// Fail closed: an exit in use that cannot be built blocks instead of skipping it.
fn route(snap: &Snapshot) -> Option<Target> {
    let plan = plan(snap)?;
    let broken = matches!(&plan, Plan::Exit(e) | Plan::ExitThenWarp(e) if proxy(&e.link).is_err());
    Some(if broken {
        Target::Reject
    } else if matches!(plan, Plan::ExitThenWarp(_)) {
        Target::Group(EXIT_ROUTE.into())
    } else {
        Target::Proxy(plan.target().into())
    })
}

fn latency_text(snap: &Snapshot) -> (String, Tone) {
    match &*snap.live(&PROBE) {
        None => ("Not tested yet".into(), Tone::Neutral),
        Some((Ok(ms), at)) => (
            format!("{ms} ms through both hops · {}", kit::age(*at)),
            Tone::Neutral,
        ),
        Some((Err(_), at)) => (
            format!("No answer through both hops · {}", kit::age(*at)),
            Tone::Danger,
        ),
    }
}

/// `This device → entry → exit → Internet` as one line of steps.
fn route_line(ui: &mut egui::Ui, snap: &Snapshot, plan: &Plan) {
    let pal = kit::colors(ui);
    let entry = servers::selected(snap).map(|s| s.name);
    let direct = !through_entry(snap);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4., 4.);
        let step = |ui: &mut egui::Ui, text: &str, color: egui::Color32| {
            ui.label(egui::RichText::new(text).color(color));
        };
        let arrow = |ui: &mut egui::Ui| {
            kit::icon(ui, Icon::ChevronRight, 12., pal.faint);
        };
        step(ui, "This device", pal.muted);
        arrow(ui);
        if !direct {
            match &entry {
                Some(name) => step(ui, name, pal.foreground),
                None => step(ui, "No server selected", pal.danger),
            }
            arrow(ui);
        }
        step(ui, &plan.name(), pal.foreground);
        arrow(ui);
        step(ui, "Internet", pal.muted);
    });
}

fn ui(p: &mut Setup) {
    p.page_fn(
        PageSpec {
            id: "exit-server",
            title: "Proxy chain",
            icon: Icon::Exit,
            order: 45,
        },
        |ui, cx| {
            let snap = cx.snapshot().clone();
            let exits = snap.get(&EXITS);
            let current = plan(&snap);
            let counter = match &current {
                Some(p) => format!("Using {}", p.name()),
                None if exits.is_empty() && snap.get(&WARP_ACCOUNT).is_none() => String::new(),
                None => "Off".into(),
            };
            kit::page_header(ui, "Proxy chain", &counter, |ui| {
                if current.is_some() {
                    cx.job_button(
                        ui,
                        Icon::Checks,
                        "Test",
                        cx.core_status().running(),
                        || CheckExit,
                    );
                }
            });
            kit::page_note(
                ui,
                "Proxied traffic goes through your current server, then the exit server, so sites see the exit's address.",
            );

            kit::page_scroll(ui, "exit-server", |ui| {
                body(ui, cx, &snap, current.as_ref())
            });
        },
    );
    status(p);
}

fn body(ui: &mut egui::Ui, cx: &mut UiCx, snap: &Snapshot, current: Option<&Plan>) {
    let settings = (*snap.get(&SETTINGS)).clone();
    if let Some(plan) = current {
        kit::card(ui, |ui| {
            route_line(ui, snap, plan);
            let (text, tone) = latency_text(snap);
            kit::tone_text(ui, text, tone);
        });
        ui.add_space(theme::SECTION_GAP);
    }
    // Like every page: the add bar and list first, then the settings.
    exits(ui, cx);
    ui.add_space(theme::SECTION_GAP);
    route_settings(ui, cx, snap, &settings);
}

fn route_settings(ui: &mut egui::Ui, cx: &mut UiCx, snap: &Snapshot, settings: &Settings) {
    kit::settings_card(ui, |ui| {
        let mut entry = settings.entry;
        kit::settings_row(
            ui,
            "Reach the exit",
            "Through the current server (two hops) or directly from this device. If the exit is unavailable, proxied traffic stops.",
            |ui| {
                if kit::segmented(
                    ui,
                    &mut entry,
                    &[
                        (Entry::Selected, "Via current server"),
                        (Entry::Direct, "Directly"),
                    ],
                ) {
                    cx.update("entry", &SETTINGS, move |s| s.entry = entry);
                }
            },
        );
        match &*snap.get(&WARP_ACCOUNT) {
            None => kit::settings_row(
                ui,
                "Cloudflare WARP",
                "A free exit on a Cloudflare address, or a fallback when the exit server is down",
                |ui| {
                    cx.job_button(ui, Icon::Add, "Set up WARP", true, || SetUpWarp);
                },
            ),
            Some(_) => {
                let mut use_warp = settings.warp;
                kit::settings_row(
                    ui,
                    "Cloudflare WARP",
                    "If exit fails: used when the exit server does not answer. Always: WARP is the exit",
                    |ui| {
                        if kit::Button::ghost("Remove").show(ui).clicked() {
                            cx.submit("remove-warp", RemoveWarp);
                        }
                        if kit::segmented(
                            ui,
                            &mut use_warp,
                            &[
                                (WarpUse::Off, "Off"),
                                (WarpUse::Backup, "If exit fails"),
                                (WarpUse::Only, "Always"),
                            ],
                        ) {
                            cx.update("warp", &SETTINGS, move |s| s.warp = use_warp);
                        }
                    },
                );
            }
        }
    });
}

fn exits(ui: &mut egui::Ui, cx: &mut UiCx) {
    ListEditor::new(&EXITS)
        .unordered()
        .add_form(
            "Add exit server",
            || Exit {
                id: Id::random(),
                name: String::new(),
                link: String::new(),
            },
            |ui, e, width| {
                let r = kit::input(
                    ui,
                    "link",
                    &mut e.link,
                    "Exit server link (vless://, hysteria2://…)",
                    width,
                    false,
                );
                kit::submitted(ui, &r) && !e.link.trim().is_empty()
            },
        )
        .leading(16., |ui, cx, e: &Exit| {
            let pal = kit::colors(ui);
            let on = *cx.get(&ACTIVE) == Some(e.id);
            kit::icon(
                ui,
                if on { Icon::Connected } else { Icon::Circle },
                16.,
                if on { pal.success } else { pal.faint },
            );
        })
        .column(Column::edit("Name", "Exit server name", |e: &mut Exit| {
            &mut e.name
        }))
        .column(Column::text("Address", Exit::address).width(150.))
        .column(
            Column::custom("", 84., |ui, cx, e: &Exit| {
                let on = *cx.get(&ACTIVE) == Some(e.id);
                let (label, next) = if on {
                    ("Turn off", None)
                } else {
                    ("Use", Some(e.id))
                };
                if kit::Button::ghost(label).show(ui).clicked() {
                    cx.submit("use", Use(next));
                }
            })
            .right(),
        )
        .empty(
            Icon::Exit,
            "No exit server",
            "Paste the link of a server to leave through, or set up WARP.",
        )
        .show(ui, cx);
}

fn status(p: &mut Setup) {
    p.fill(&STATUS, "exit", 8, |ui, cx, _| {
        let Some(e) = plan(cx.snapshot()) else {
            return;
        };
        let pal = kit::colors(ui);
        let failed = matches!(&*cx.live(&PROBE), Some((Err(_), _)));
        let color = if failed { pal.danger } else { pal.accent };
        let text = format!("Chain · {}", e.name());
        cx.status_chip(
            ui,
            kit::Lead::Icon(Icon::Exit, color),
            &text,
            "Traffic leaves through the exit server",
        );
    });
    p.fill(&PANELS, "exit", 15, |ui, cx, _| {
        let snap = cx.snapshot().clone();
        let Some(e) = plan(&snap) else { return };
        rproxy_kernel::shell::panel_title_page(ui, cx, "Proxy chain", "exit-server");
        route_line(ui, &snap, &e);
        let (text, tone) = latency_text(&snap);
        kit::tone_text(ui, text, tone);
    });
    p.fill(&servers::CONNECTION, "exit", 40, |ui, cx, _| {
        let Some(e) = plan(cx.snapshot()) else {
            return;
        };
        let pal = kit::colors(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.;
            kit::icon(ui, Icon::Exit, 14., pal.accent);
            let text = format!("Then via {}", e.name());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(theme::SMALL)
                        .color(pal.muted),
                )
                .truncate(),
            );
        });
        ui.add_space(4.);
    });
}
