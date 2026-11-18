//! # chain-exit
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
use homeport_kernel::{
    kit::{self, theme},
    mihomo,
    prelude::*,
    ui::{Column, ListEditor},
};
use kit::Tone;
use plugin_servers as servers;
use std::time::Duration;

pub static PLUGIN: Plugin = Plugin {
    id: "chain-exit",
    name: "Chain exit",
    description: "Send proxied traffic through the current server, then one stable exit server.",
    requires: &[&plugin_servers::PLUGIN],
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

pub mod warp;

/// mihomo names of the exit proxy, the WARP proxy and the exit-then-WARP group.
pub const EXIT: &str = "exit";
pub const WARP: &str = "warp";
pub const EXIT_ROUTE: &str = "exit-route";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "chain_exits")]
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

pub static EXITS: State<Vec<Exit>> = State::new("chain-exit.list");
/// The exit in use; `None` = off (one hop, as without this plugin).
pub static ACTIVE: State<Option<Id>> = State::new("chain-exit.active");
/// Last end-to-end probe through both hops: `(result, unix time)`.
pub static PROBE: State<Option<(std::result::Result<u32, String>, u64)>> =
    State::new("chain-exit.probe");

choices!(pub enum Entry {
    Selected => "Current server",
    Direct => "Directly",
});
choices!(pub enum WarpUse {
    Off => "Off",
    Backup => "If exit fails",
    Only => "Always",
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
pub static SETTINGS: State<Settings> = State::new("chain-exit.settings");
/// The registered WARP device, if set up.
pub static WARP_ACCOUNT: State<Option<warp::Account>> = State::new("chain-exit.warp");

/// Register a WARP device (job "Set up WARP").
pub struct SetUpWarp;
impl Op for SetUpWarp {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Set up WARP").page(&PLUGIN))
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
/// While traffic goes server → exit, servers are tested the same way: each
/// through the exit (`via-<server>`), so a rating says how the chain works.
fn tests_through_exit(snap: &Snapshot) -> bool {
    through_entry(snap)
        && plan(snap).is_some_and(|p| match &p {
            Plan::Exit(e) | Plan::ExitThenWarp(e) => proxy(&e.link).is_ok(),
            Plan::Warp => true,
        })
}
/// The mihomo name of the exit dialed through server `id`.
fn via(id: Id) -> String {
    format!("via-{id}")
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
        Some(JobSpec::new("Test proxy chain").quiet().page(&PLUGIN))
    }
}

fn proxy(link: &str) -> Result<servers::Proxy> {
    let link = link.trim();
    if !servers::is_link(link) || link.lines().count() > 1 {
        bail!("Paste one server link.");
    }
    servers::Proxy::from_link(link)
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
    p.on(redial_on_new_entry).latest();
    p.core(fragment);
    p.hook(&servers::ROUTE, |snap, _| route(snap));
    p.hook(&servers::TEST_PATH, |snap, s| {
        tests_through_exit(snap).then(|| via(s.id))
    });
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
    info!("WARP account registered");
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
    info!("WARP removed");
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
    let exit = id.and_then(|id| cx.get(&EXITS).iter().find(|e| e.id == id).cloned());
    match exit {
        Some(e) => info!("Exit server {} in use", e.name),
        None => info!("Exit server off"),
    }
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
        .delay(plan.target(), mihomo::PROBE_URL, 6000)
        .await
        .map_err(|e| e.message);
    // When it starts or stops answering, not on every check.
    let before = (*cx.live(&PROBE)).as_ref().map(|(r, _)| r.is_ok());
    match &result {
        Err(e) if before != Some(false) => warn!("The exit server does not answer: {e}"),
        Ok(ms) if before == Some(false) => info!("The exit server answers again ({ms} ms)"),
        _ => {}
    }
    cx.set_live(&PROBE, Some((result.clone(), now())));
    result.map_err(|_| Error::msg("The exit did not answer."))
}

/// The exit keeps its transport connections (XHTTP reuse) open for a long
/// time. A new entry must take effect now: rebuild the exit so new
/// connections are dialed through the current entry ([`route`] blocks
/// without one).
async fn redial_on_new_entry(cx: Cx, _: servers::SelectionChanged) -> Result<()> {
    let snap = cx.snapshot();
    if plan(&snap).is_some() && through_entry(&snap) {
        cx.core().reapply().await?;
    }
    Ok(())
}

fn fragment(snap: &Snapshot) -> Fragment {
    let Some(plan) = plan(snap) else {
        return Fragment::default();
    };
    let dial = through_entry(snap).then(|| Target::Group(servers::MAIN.into()));
    let mut f = Fragment {
        labels: vec![
            (EXIT.into(), "Exit server".into()),
            (WARP.into(), "WARP".into()),
        ],
        ..Fragment::default()
    };
    let exit = match &plan {
        Plan::Exit(e) | Plan::ExitThenWarp(e) => match proxy(&e.link) {
            Ok(px) => Some(px),
            Err(_) => return f,
        },
        Plan::Warp => None,
    };
    let account = snap.get(&WARP_ACCOUNT);
    // The exit as a mihomo proxy dialed through `dial`.
    let hop = |name: String, dial: Option<Target>| match &exit {
        Some(px) => Some(mihomo::Proxy {
            name,
            protocol: px.protocol.to_string(),
            server: px.server.clone(),
            port: px.port,
            options: px.options.clone(),
            dialer: dial,
        }),
        None => (*account).as_ref().map(|a| warp::proxy(&name, a, dial)),
    };
    f.proxies
        .extend(hop(EXIT.into(), dial.clone()).filter(|_| exit.is_some()));
    if let (Plan::Warp | Plan::ExitThenWarp(_), Some(account)) = (&plan, &*account) {
        f.proxies.push(warp::proxy(WARP, account, dial));
    }
    // Every server as the entry, for its tests (see [`tests_through_exit`]).
    if tests_through_exit(snap) {
        for s in snap.get(&servers::SERVERS).iter() {
            f.proxies
                .extend(hop(via(s.id), Some(Target::Proxy(s.id.to_string()))));
            f.labels.push((via(s.id), format!("{} → exit", s.name)));
        }
    }
    if matches!(plan, Plan::ExitThenWarp(_)) {
        f.groups.push(mihomo::Group {
            name: EXIT_ROUTE.into(),
            kind: mihomo::GroupKind::Fallback {
                url: mihomo::PROBE_URL.into(),
                interval_secs: 60,
            },
            members: vec![Target::Proxy(EXIT.into()), Target::Proxy(WARP.into())],
            selected: None,
        });
    }
    f
}

/// Fail closed: an exit in use that cannot be built, or that dials through
/// an entry while none is connected, blocks instead of skipping it. The
/// block lands in the same commit as the disconnect, not after a redial.
fn route(snap: &Snapshot) -> Option<Target> {
    let plan = plan(snap)?;
    let broken = matches!(&plan, Plan::Exit(e) | Plan::ExitThenWarp(e) if proxy(&e.link).is_err());
    let no_entry = through_entry(snap) && snap.get(&servers::SELECTED).is_none();
    Some(if broken || no_entry {
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
            title: "Chain exit",
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
            cx.page_header(ui, &counter, |ui, cx| {
                if current.is_some() {
                    cx.job_button(ui, Icon::Checks, "Test", cx.core_status().running(), || {
                        CheckExit
                    });
                }
            });

            kit::page_scroll(ui, "chain-exit", |ui| body(ui, cx, &snap, current.as_ref()));
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
    }
    // Like every page: the add bar and list first, then the settings.
    exits(ui, cx);
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
                );
                kit::submitted(ui, &r) && !e.link.trim().is_empty()
            },
        )
        .leading("", 16., |ui, cx, e: &Exit| {
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
        homeport_kernel::shell::panel_title_page(ui, cx, "Chain exit", &PLUGIN);
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
