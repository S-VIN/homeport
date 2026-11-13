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
    name: "Exit server",
    description: "Send all traffic through one stable exit server after the selected server.",
    category: categories::NETWORK,
    requires: &["servers", "format-uri"],
    required: false,
    default_enabled: true,
    setup,
};

/// mihomo name of the exit proxy.
pub const EXIT: &str = "exit";
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

/// Use an exit server, or turn the second hop off with `None`.
pub struct Use(pub Option<Id>);
impl Op for Use {
    type Reply = ();
}
/// Measure latency through the entry and the exit (job "Check exit").
pub struct CheckExit;
impl Op for CheckExit {
    type Reply = u32;
    fn job(&self) -> Option<JobSpec> {
        Some(
            JobSpec::keyed("exit-server/check", "Check exit server")
                .quiet()
                .page("exit-server"),
        )
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
    p.memory(&PROBE, || None);
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
    p.handle(|cx: Cx, Use(id): Use| async move {
        cx.write(move |w| {
            if id.is_some_and(|id| !w.get(&EXITS).iter().any(|e| e.id == id)) {
                bail!("This exit server no longer exists.");
            }
            w.set(&ACTIVE, id);
            w.set(&PROBE, None);
            Ok(())
        })
        .await?;
        if id.is_some() {
            cx.spawn(CheckExit)?;
        }
        Ok(())
    });
    p.handle(|cx: Cx, _: CheckExit| async move {
        let snap = cx.snapshot();
        if active(&snap).is_none() {
            bail!("No exit server is in use.");
        }
        if snap.get(&servers::SELECTED).is_none() {
            bail!("Connect to a server first: it is the entry to the exit server.");
        }
        let result = cx
            .core()
            .delay(EXIT, PROBE_URL, 6000)
            .await
            .map_err(|e| e.message);
        let stored = result.clone();
        cx.write(move |w| {
            w.set(&PROBE, Some((stored, now())));
            Ok(())
        })
        .await?;
        result
            .map_err(|_| Error::msg("The exit server did not answer through the selected server."))
    });
    p.every(
        "check",
        |s| active(s).map(|_| Duration::from_secs(120)),
        || CheckExit,
    );
    // The exit keeps its transport connections (XHTTP reuse) open for a long
    // time. A new entry or a disconnect must take effect now: rebuild the exit
    // so new connections are dialed through the current entry, or not at all.
    p.wrap(|cx: Cx, op: servers::Select, next: Next<servers::Select>| async move {
        let before = *cx.get(&servers::SELECTED);
        next.run(&cx, op).await?;
        if *cx.get(&servers::SELECTED) != before && active(&cx.snapshot()).is_some() {
            cx.core().reapply().await?;
        }
        Ok(())
    });

    p.core(|snap| {
        let Some(px) = active(snap).and_then(|e| proxy(&e.link).ok()) else {
            return Fragment::default();
        };
        let mut options = px.options;
        options.insert("dialer-proxy".into(), json!(servers::MAIN));
        Fragment {
            proxies: vec![mihomo::Proxy {
                name: EXIT.into(),
                protocol: px.protocol.to_string(),
                server: px.server,
                port: px.port,
                options,
            }],
            ..Default::default()
        }
    });
    // Fail closed: an exit in use that cannot be built blocks instead of skipping it.
    p.hook(&servers::ROUTE, |snap, _| {
        let e = active(snap)?;
        Some(if proxy(&e.link).is_ok() {
            Target::Proxy(EXIT.into())
        } else {
            Target::Reject
        })
    });
    ui(p);
    Ok(())
}

fn latency_text(snap: &Snapshot) -> (String, Tone) {
    match &*snap.get(&PROBE) {
        None => ("Not checked yet".into(), Tone::Neutral),
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
fn route_line(ui: &mut egui::Ui, snap: &Snapshot, exit: &Exit) {
    let pal = kit::colors(ui);
    let entry = servers::selected(snap).map(|s| s.name);
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
        match &entry {
            Some(name) => step(ui, name, pal.foreground),
            None => step(ui, "No server selected", pal.danger),
        }
        arrow(ui);
        step(ui, &exit.name, pal.foreground);
        arrow(ui);
        step(ui, "Internet", pal.muted);
    });
}

fn ui(p: &mut Setup) {
    p.page_fn(PageSpec { id: "exit-server", title: "Exit server", icon: Icon::Exit, order: 45 }, |ui, cx| {
        let snap = cx.snapshot().clone();
        let exits = snap.get(&EXITS);
        let current = active(&snap);
        let counter = match &current {
            Some(e) => format!("Using {}", e.name),
            None if exits.is_empty() => String::new(),
            None => "Off".into(),
        };
        kit::page_header(ui, "Exit server", &counter, |ui| {
            if current.is_some() {
                cx.job_button(ui, Icon::Checks, "Check", 80., cx.core_status().running(), || CheckExit);
            }
        });
        kit::helper(ui, "Traffic goes through the selected server, then through the exit server to the internet. Pick a stable server you control as the exit; if it does not answer, traffic stops instead of leaving without it.");
        if let Some(e) = &current {
            kit::card(ui, |ui| {
                route_line(ui, &snap, e);
                let (text, tone) = latency_text(&snap);
                kit::tone_text(ui, text, tone);
            });
            ui.add_space(theme::SECTION_GAP);
        }
        ListEditor::new(&EXITS)
            .unordered()
            .add_form("Add exit server", || Exit { id: Id::random(), name: String::new(), link: String::new() }, |ui, e, width| {
                let r = kit::input(ui, "link", &mut e.link, "Exit server link (vless://…)", width, false);
                kit::submitted(ui, &r) && !e.link.trim().is_empty()
            })
            .leading(16., |ui, cx, e: &Exit| {
                let pal = kit::colors(ui);
                let on = *cx.get(&ACTIVE) == Some(e.id);
                kit::icon(ui, if on { Icon::Connected } else { Icon::Glyph('\u{E18A}') }, 16., if on { pal.success } else { pal.faint });
            })
            .column(Column::edit("Name", "Exit server name", |e: &mut Exit| &mut e.name))
            .column(Column::text("Address", Exit::address).width(150.))
            .column(
                Column::custom("", 84., |ui, cx, e: &Exit| {
                    let on = *cx.get(&ACTIVE) == Some(e.id);
                    let (label, next) = if on { ("Turn off", None) } else { ("Use", Some(e.id)) };
                    if kit::Button::ghost(label).show(ui).clicked() {
                        cx.submit("use", Use(next));
                    }
                })
                .right(),
            )
            .empty(Icon::Exit, "No exit server", "Paste the link of a server you control, for example the one an rproxy VLESS server shows.")
            .show(ui, cx);
    });
    p.fill(&STATUS, "exit", 8, |ui, cx, _| {
        let Some(e) = active(cx.snapshot()) else {
            return;
        };
        let pal = kit::colors(ui);
        let failed = matches!(&*cx.get(&PROBE), Some((Err(_), _)));
        let color = if failed { pal.danger } else { pal.accent };
        if kit::chip(
            ui,
            Some(kit::Lead::Icon(Icon::Exit, color)),
            &format!("Exit · {}", e.name),
            "Traffic leaves through the exit server",
        )
        .clicked()
        {
            cx.toggle_panels();
        }
    });
    p.fill(&PANELS, "exit", 15, |ui, cx, _| {
        let snap = cx.snapshot().clone();
        let Some(e) = active(&snap) else { return };
        let mut open = false;
        rproxy_kernel::shell::panel_title(ui, "Exit server", |ui| {
            if cx.page() != "exit-server" {
                open = kit::Button::ghost("Manage")
                    .tone(kit::Tone::Accent)
                    .show(ui)
                    .clicked();
            }
        });
        if open {
            cx.navigate("exit-server");
        }
        route_line(ui, &snap, &e);
        let (text, tone) = latency_text(&snap);
        kit::tone_text(ui, text, tone);
    });
    p.fill(&servers::CONNECTION, "exit", 40, |ui, cx, _| {
        let Some(e) = active(cx.snapshot()) else {
            return;
        };
        let pal = kit::colors(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.;
            kit::icon(ui, Icon::Exit, 14., pal.accent);
            let text = format!("Then exit server {}", e.name);
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
