//! # connections
//! Live connections and traffic from the core: what goes where, by which rule,
//! through which server, and how fast. Totals per server for this session show
//! in the server's details; the current speed is a chip in the status bar.
//!
//! The core is polled once a second by a service into a live state (not
//! saved, no transaction); the UI repaints when it changes.
use plugin_servers::{self as servers, Server};
use rproxy_kernel::{
    core::Labels,
    kit::{self, theme},
    prelude::*,
    shell::panel_title,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub static PLUGIN: Plugin = Plugin {
    id: "connections",
    name: "Connections",
    description: "Live connections and traffic: what goes where, by which rule, through which server.",
    requires: &["servers"],
    setup,
    ..Plugin::DEFAULT
};

/// One open connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub id: String,
    /// Host name or address with port.
    pub target: String,
    pub network: String,
    /// The app that opened it, when the core looks apps up (app rules exist).
    pub process: String,
    pub rule: String,
    /// The outbound names from mihomo (server ids, DIRECT, exit …), outermost first.
    pub chain: Vec<String>,
    pub up: u64,
    pub down: u64,
    pub started: Instant,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Traffic {
    pub conns: Vec<Conn>,
    pub up_total: u64,
    pub down_total: u64,
    pub up_rate: u64,
    pub down_rate: u64,
    /// Bytes per server this session: closed connections plus open ones.
    closed: HashMap<Id, (u64, u64)>,
    last: HashMap<String, (Option<Id>, u64, u64)>,
    /// Anything open or moving now or in the last few seconds, so bursts
    /// don't make the chip and the panel blink.
    active: bool,
    pub online: bool,
}
/// When the last sample was taken and the last traffic seen. Kept by the
/// poller, outside [`Traffic`]: a new time alone must not repaint the windows.
#[derive(Clone, Copy, Debug, Default)]
struct Clock {
    sampled: Option<Instant>,
    busy: Option<Instant>,
}
pub static TRAFFIC: State<Traffic> = State::new("connections.traffic");

impl Traffic {
    /// Traffic now or in the last few seconds.
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn by_server(&self, id: Id) -> (u64, u64) {
        let (mut up, mut down) = self.closed.get(&id).copied().unwrap_or_default();
        for (server, u, d) in self.last.values() {
            if *server == Some(id) {
                up += u;
                down += d;
            }
        }
        (up, down)
    }
    fn update(&mut self, v: &mihomo::Connections, clock: &mut Clock) {
        let now = Instant::now();
        let (up_total, down_total) = (v.upload_total, v.download_total);
        if let Some(at) = clock.sampled {
            let secs = now.duration_since(at).as_secs_f64().max(0.1);
            self.up_rate = (up_total.saturating_sub(self.up_total) as f64 / secs) as u64;
            self.down_rate = (down_total.saturating_sub(self.down_total) as f64 / secs) as u64;
        }
        (self.up_total, self.down_total, clock.sampled) = (up_total, down_total, Some(now));
        let old: HashMap<String, Instant> = self
            .conns
            .iter()
            .map(|c| (c.id.clone(), c.started))
            .collect();
        let mut seen = HashMap::new();
        self.conns = v
            .connections
            .iter()
            .filter(|c| !mihomo::config::internal_listener(&c.metadata.inbound_name))
            .map(|c| {
                let m = &c.metadata;
                let host = [&m.host, &m.destination_ip]
                    .into_iter()
                    .find(|h| !h.is_empty())
                    .map_or("?", |h| h.as_str());
                let conn = Conn {
                    target: match m.destination_port {
                        Some(p) => format!("{host}:{p}"),
                        None => host.to_owned(),
                    },
                    network: m.network.to_uppercase(),
                    process: m.process.clone(),
                    rule: match (c.rule.as_str(), c.rule_payload.as_str()) {
                        (r, "") => r.to_owned(),
                        (r, p) => format!("{r} {p}"),
                    },
                    up: c.upload,
                    down: c.download,
                    started: old.get(&c.id).copied().unwrap_or(now),
                    chain: c.chains.clone(),
                    id: c.id.clone(),
                };
                let server = conn.chain.iter().find_map(|n| n.parse::<Id>().ok());
                seen.insert(conn.id.clone(), (server, conn.up, conn.down));
                conn
            })
            .collect();
        // Connections that closed since the last sample keep counting for their server.
        for (id, (server, up, down)) in self.last.drain() {
            if let (false, Some(s)) = (seen.contains_key(&id), server) {
                let t = self.closed.entry(s).or_default();
                t.0 += up;
                t.1 += down;
            }
        }
        self.last = seen;
        self.conns.sort_by_key(|c| std::cmp::Reverse(c.started));
        if !self.conns.is_empty() || self.up_rate + self.down_rate > 0 {
            clock.busy = Some(now);
        }
        self.active = clock
            .busy
            .is_some_and(|t| now.duration_since(t) < Duration::from_secs(10));
    }
}

/// Close one connection, or all of them with `None`.
pub struct Close(pub Option<String>);
impl Op for Close {
    type Reply = ();
}

fn setup(p: &mut Setup) -> Result<()> {
    p.live(&TRAFFIC, Traffic::default);
    p.service(
        "poll",
        |_| Some(()),
        |cx: Cx, ()| async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let (mut idle, mut clock) = (0, Clock::default());
            loop {
                tick.tick().await;
                // Nobody watching (a server without an open browser tab): sample
                // every 10 s, enough for the session totals.
                idle = if cx.ui_open() { 0 } else { (idle + 1) % 10 };
                if idle != 0 {
                    continue;
                }
                match cx.core().connections().await {
                    Ok(v) => cx.update_live(&TRAFFIC, |t| {
                        t.update(&v, &mut clock);
                        t.online = true;
                    }),
                    Err(_) => {
                        clock.sampled = None;
                        cx.update_live(&TRAFFIC, |t| {
                            t.online = false;
                            t.up_rate = 0;
                            t.down_rate = 0;
                        })
                    }
                }
            }
        },
    );
    p.handle(|cx: Cx, Close(id): Close| async move {
        match id {
            Some(id) => cx.core().close_connection(&id).await,
            None => cx.core().close_all().await,
        }
    });
    ui(p);
    Ok(())
}

/// What a connection went through, in words: its outbounds, not the groups
/// that picked them.
fn route(labels: &Labels, chain: &[String]) -> String {
    let parts: Vec<&str> = chain
        .iter()
        .filter(|n| !labels.is_group(n))
        .filter_map(|n| labels.get(n))
        .collect();
    if parts.is_empty() {
        chain.first().cloned().unwrap_or_default()
    } else {
        parts.join(" → ")
    }
}

fn age(started: Instant) -> String {
    let s = started.elapsed().as_secs();
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}

fn ui(p: &mut Setup) {
    p.page_fn(
        PageSpec {
            id: "connections",
            title: "Connections",
            icon: Icon::Activity,
            order: 35,
        },
        page,
    );
    p.fill(&STATUS, "traffic", 40, traffic_chip);
    p.fill(&PANELS, "traffic", 30, traffic_panel);
    p.fill(&servers::DETAILS, "traffic", 30, |ui, cx, s: &Server| {
        let (up, down) = cx.live(&TRAFFIC).by_server(s.id);
        if up + down > 0 {
            let text = format!("↓ {}  ↑ {} this session", kit::bytes(down), kit::bytes(up));
            kit::property(ui, "Traffic", &text);
        }
    });
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let labels = cx.core().labels();
    let t = cx.live(&TRAFFIC);
    let online = t.online;
    let counter = if online {
        format!("{} open", t.conns.len())
    } else {
        "Core offline".into()
    };
    let mut close_all = false;
    kit::page_header(ui, "Connections", &counter, |ui| {
        close_all = kit::Button::ghost("Close all")
            .tone(kit::Tone::Danger)
            .enabled(online && !t.conns.is_empty())
            .show(ui)
            .on_hover_text("Apps may reconnect automatically.")
            .clicked();
    });
    if close_all {
        cx.submit("close-all", Close(None));
    }
    if let Some(Ok(())) = cx.outcome("close-all") {
        cx.notify("All connections closed. Apps may reconnect.");
    }
    kit::card(ui, |ui| {
        kit::stats(
            ui,
            &[
                ("Download", kit::rate(t.down_rate), None),
                ("Upload", kit::rate(t.up_rate), None),
                ("Downloaded", kit::bytes(t.down_total), None),
                ("Uploaded", kit::bytes(t.up_total), None),
            ],
        );
    });
    ui.add_space(theme::SECTION_GAP);
    let filter_id = egui::Id::unique("connections-filter");
    let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
    kit::input(
        ui,
        "filter",
        &mut filter,
        "Filter by host, rule or server",
        ui.available_width(),
        false,
    );
    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
    ui.add_space(theme::SPACE_M);
    let needle = filter.trim().to_lowercase();
    let matches = |c: &Conn, route: &str| {
        needle.is_empty()
            || [c.target.as_str(), c.rule.as_str(), route]
                .iter()
                .any(|s| s.to_lowercase().contains(&needle))
    };
    let mut rows: Vec<(Conn, String)> = t
        .conns
        .iter()
        .map(|c| {
            // Rules in the user's words ("*.youtube.com"), not the core's.
            let rule = labels.name(&c.rule).to_owned();
            (Conn { rule, ..c.clone() }, route(&labels, &c.chain))
        })
        .filter(|(c, r)| matches(c, r))
        .collect();
    drop(t);
    let matched = rows.len();
    rows.truncate(300);
    if !online && !rows.is_empty() {
        kit::banner(
            ui,
            kit::Tone::Warning,
            "Core offline · Last known connections",
            None,
        );
    }
    if matched > rows.len() {
        kit::hint(ui, format!("Showing {} of {matched} matches", rows.len()));
    }
    if rows.is_empty() {
        if needle.is_empty() {
            kit::list_card(ui, |ui| {
                if online {
                    kit::empty_with(
                        ui,
                        Icon::Activity,
                        "No open connections",
                        "Apps that use the proxy show up here.",
                    );
                } else {
                    kit::empty(ui, Icon::Warning, "Core offline");
                }
            });
        } else {
            kit::empty(ui, Icon::Filter, "Nothing matches");
            ui.vertical_centered(|ui| {
                if kit::Button::secondary("Clear filter").show(ui).clicked() {
                    ui.data_mut(|d| d.insert_temp(filter_id, String::new()));
                }
            });
        }
        return;
    }
    kit::page_scroll(ui, "connections", |ui| {
        kit::list_card(ui, |ui| {
            for (c, via) in &rows {
                connection_row(ui, cx, c, via, online);
            }
        });
    });
}

/// Target and how it goes on the left, bytes and a close button on the right.
fn connection_row(ui: &mut egui::Ui, cx: &mut UiCx, c: &Conn, via: &str, online: bool) {
    kit::row(ui, ("conn", &c.id), false, |ui, _| {
        kit::row_line(ui, 54., |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close = kit::icon_button_ex(
                    ui,
                    Icon::Close,
                    "Close this connection",
                    online,
                    kit::Emphasis::Quiet,
                    true,
                );
                if close.clicked() {
                    cx.submit(("close", &c.id), Close(Some(c.id.clone())));
                }
                ui.allocate_ui_with_layout(
                    egui::vec2(88., 36.),
                    egui::Layout::top_down(egui::Align::Max),
                    |ui| {
                        kit::hint(ui, format!("↓ {}", kit::bytes(c.down)));
                        kit::caption(ui, format!("↑ {}", kit::bytes(c.up)));
                    },
                );
                let width = ui.available_width().max(40.);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 38.),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        let pal = kit::colors(ui);
                        kit::truncated(
                            ui,
                            egui::RichText::new(&c.target).color(pal.foreground),
                            Some(&c.target),
                        );
                        // Least telling last: a narrow row cuts the protocol first.
                        // Not twice when an app rule matched it.
                        let app = match format!("app:{}", c.process) {
                            _ if c.process.is_empty() => String::new(),
                            app if app == c.rule => String::new(),
                            app => format!("{app} · "),
                        };
                        let detail = format!(
                            "{app}{via} · {} · {} ago · {}",
                            c.rule,
                            age(c.started),
                            c.network
                        );
                        kit::truncated(
                            ui,
                            egui::RichText::new(&detail)
                                .size(theme::SMALL)
                                .color(pal.muted),
                            Some(&detail),
                        );
                    },
                );
            });
        });
    });
}

fn traffic_chip(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let t = cx.live(&TRAFFIC);
    let (down, up) = (t.down_rate, t.up_rate);
    if !t.active() {
        return;
    }
    let pal = kit::colors(ui);
    let tip = format!("Download {} · upload {}", kit::rate(down), kit::rate(up));
    cx.status_chip(
        ui,
        kit::Lead::Icon(Icon::Activity, pal.muted),
        &format!("↓ {}", kit::rate(down)),
        &tip,
    );
}

fn traffic_panel(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let t = cx.live(&TRAFFIC);
    // The Connections page shows the same numbers in its own tiles.
    if !t.online || !t.active() || cx.page() == "connections" {
        return;
    }
    panel_title(ui, "Traffic", |ui| {
        kit::badge(ui, &format!("{} open", t.conns.len()), kit::Tone::Neutral);
    });
    kit::property(ui, "Download", &kit::rate(t.down_rate));
    kit::property(ui, "Upload", &kit::rate(t.up_rate));
    kit::property(
        ui,
        "This session",
        &format!(
            "↓ {} · ↑ {}",
            kit::bytes(t.down_total),
            kit::bytes(t.up_total)
        ),
    );
    ui.add_space(6.);
    if kit::Button::secondary("Open Connections")
        .icon(Icon::Activity)
        .show(ui)
        .clicked()
    {
        cx.navigate("connections");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_connections_keep_counting_for_their_server() {
        let server = Id::random();
        let conn = |id: &str, down: u64| {
            serde_json::json!({"id": id, "upload": 1, "download": down, "chains": [server.to_string(), "main"], "rule": "Match",
            "metadata": {"host": "example.org", "destinationPort": "443", "network": "tcp"}})
        };
        let (mut t, mut clock) = (Traffic::default(), Clock::default());
        let read = |v| serde_json::from_value::<mihomo::Connections>(v).unwrap();
        t.update(&read(serde_json::json!({"uploadTotal": 2, "downloadTotal": 300, "connections": [conn("a", 100), conn("b", 200)]})), &mut clock);
        assert_eq!(t.by_server(server), (2, 300));
        t.update(&read(serde_json::json!({"uploadTotal": 3, "downloadTotal": 450, "connections": [conn("b", 350)]})), &mut clock);
        assert!(t.active());
        assert_eq!(
            t.by_server(server),
            (2, 450),
            "a closed with 100, b is at 350"
        );
        assert_eq!(t.conns[0].target, "example.org:443");
    }
}
