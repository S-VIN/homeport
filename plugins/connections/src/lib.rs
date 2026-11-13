//! # connections
//! Live connections and traffic from the core: what goes where, by which rule,
//! through which server, and how fast. Totals per server for this session show
//! in the server's details; the current speed is a chip in the status bar.
//!
//! The core is polled once a second by a service into a live state (not
//! saved, no transaction); the UI repaints when it changes.
use plugin_servers::{self as servers, Server};
use rproxy_kernel::{
    kit::{self, theme},
    prelude::*,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub static PLUGIN: Plugin = Plugin {
    id: "connections",
    name: "Connections",
    description:
        "Live connections and traffic: what goes where, by which rule, through which server.",
    category: categories::NETWORK,
    requires: &["servers"],
    required: false,
    default_enabled: true,
    platforms: Platform::ALL,
    setup,
};

/// One open connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub id: String,
    /// Host name or address with port.
    pub target: String,
    pub network: String,
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
    sampled: Option<Instant>,
    pub online: bool,
}
pub static TRAFFIC: State<Traffic> = State::new("connections.traffic");

impl Traffic {
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
    fn update(&mut self, v: &Value) {
        let now = Instant::now();
        let (up_total, down_total) = (
            v["uploadTotal"].as_u64().unwrap_or(0),
            v["downloadTotal"].as_u64().unwrap_or(0),
        );
        if let Some(at) = self.sampled {
            let secs = now.duration_since(at).as_secs_f64().max(0.1);
            self.up_rate = (up_total.saturating_sub(self.up_total) as f64 / secs) as u64;
            self.down_rate = (down_total.saturating_sub(self.down_total) as f64 / secs) as u64;
        }
        (self.up_total, self.down_total, self.sampled) = (up_total, down_total, Some(now));
        let old: HashMap<String, Instant> = self
            .conns
            .iter()
            .map(|c| (c.id.clone(), c.started))
            .collect();
        let mut seen = HashMap::new();
        self.conns = v["connections"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|c| {
                let m = &c["metadata"];
                let host = m["host"]
                    .as_str()
                    .filter(|h| !h.is_empty())
                    .or_else(|| m["destinationIP"].as_str())
                    .unwrap_or("?");
                let id = c["id"].as_str().unwrap_or_default().to_owned();
                let chain: Vec<String> = c["chains"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let conn = Conn {
                    target: match &m["destinationPort"] {
                        Value::String(p) => format!("{host}:{p}"),
                        Value::Number(p) => format!("{host}:{p}"),
                        _ => host.to_owned(),
                    },
                    network: m["network"].as_str().unwrap_or("").to_uppercase(),
                    rule: match (
                        c["rule"].as_str().unwrap_or(""),
                        c["rulePayload"].as_str().unwrap_or(""),
                    ) {
                        (r, "") => r.to_owned(),
                        (r, p) => format!("{r} {p}"),
                    },
                    up: c["upload"].as_u64().unwrap_or(0),
                    down: c["download"].as_u64().unwrap_or(0),
                    started: old.get(&id).copied().unwrap_or(now),
                    chain,
                    id,
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
    }
}

/// Close one connection, or all of them with `None`.
pub struct Close(pub Option<String>);
impl Op for Close {
    type Reply = ();
}

fn setup(p: &mut Setup) -> Result<()> {
    p.live(&TRAFFIC, Traffic::default);
    p.service("poll", |_| Some(()), |cx: Cx, ()| async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            match cx.core().connections().await {
                Ok(v) => cx.update_live(&TRAFFIC, |t| {
                    t.update(&v);
                    t.online = true;
                }),
                Err(_) => cx.update_live(&TRAFFIC, |t| t.online = false),
            }
        }
    });
    p.handle(|cx: Cx, Close(id): Close| async move {
        match id {
            Some(id) => cx.core().close_connection(&id).await,
            None => cx.core().close_all().await,
        }
    });
    ui(p);
    Ok(())
}

/// What a connection went through, in words.
fn route(snap: &Snapshot, chain: &[String]) -> String {
    let servers = snap.get(&servers::SERVERS);
    let name = |n: &str| -> Option<String> {
        if let Ok(id) = n.parse::<Id>() {
            return servers.iter().find(|s| s.id == id).map(|s| s.name.clone());
        }
        match n {
            "DIRECT" => Some("Direct".into()),
            "REJECT" | "REJECT-DROP" => Some("Blocked".into()),
            "exit" => Some("Exit server".into()),
            "warp" => Some("WARP".into()),
            _ => None,
        }
    };
    let parts: Vec<String> = chain.iter().filter_map(|n| name(n)).collect();
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
    p.page_fn(PageSpec { id: "connections", title: "Connections", icon: Icon::Activity, order: 35 }, |ui, cx| {
        let snap = cx.snapshot().clone();
        let t = cx.live(&TRAFFIC);
        let counter = if t.online { format!("{} open", t.conns.len()) } else { "Core offline".into() };
        let has = !t.conns.is_empty();
        let mut close_all = false;
        kit::page_header(ui, "Connections", &counter, |ui| {
            close_all = kit::Button::ghost("Close all").tone(kit::Tone::Danger).enabled(has).show(ui).on_hover_text("Close every open connection; apps reconnect on their own").clicked();
        });
        kit::helper(ui, "What goes where right now: the rule that matched and the route it took. Newest first.");
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
        kit::input(ui, "filter", &mut filter, "Filter by host, rule or server", ui.available_width(), false);
        ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
        ui.add_space(theme::SPACE_M);
        let needle = filter.trim().to_lowercase();
        let rows: Vec<(Conn, String)> = t
            .conns
            .iter()
            .map(|c| (c.clone(), route(&snap, &c.chain)))
            .filter(|(c, r)| needle.is_empty() || [c.target.as_str(), c.rule.as_str(), r.as_str()].iter().any(|s| s.to_lowercase().contains(&needle)))
            .take(300)
            .collect();
        drop(t);
        if close_all {
            cx.submit("close-all", Close(None));
        }
        if rows.is_empty() {
            kit::empty(ui, Icon::Activity, if needle.is_empty() { "No open connections" } else { "Nothing matches" }, "Connections appear here while apps use the listeners.");
            return;
        }
        kit::page_scroll(ui, "connections", |ui| {
            kit::list_card(ui, |ui| {
                for (c, via) in rows {
                    kit::row(ui, ("conn", &c.id), false, |ui, state| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                kit::truncated(ui, egui::RichText::new(&c.target).color(kit::colors(ui).foreground), Some(&c.target));
                            });
                            kit::caption(ui, format!("{} · {} · {} · {}", c.network, c.rule, via, age(c.started)));
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if state.hovered && kit::icon_button(ui, Icon::Close, "Close this connection", true).clicked() {
                                cx.submit(("close", &c.id), Close(Some(c.id.clone())));
                            }
                            kit::caption(ui, format!("↓ {}  ↑ {}", kit::bytes(c.down), kit::bytes(c.up)));
                        });
                    });
                }
            });
        });
    });
    p.fill(&STATUS, "traffic", 40, |ui, cx, _| {
        let t = cx.live(&TRAFFIC);
        let (down, up) = (t.down_rate, t.up_rate);
        if down + up == 0 {
            return;
        }
        let pal = kit::colors(ui);
        if kit::chip(
            ui,
            Some(kit::Lead::Icon(Icon::Activity, pal.muted)),
            &format!("↓ {}", kit::rate(down)),
            &format!("Download {} · upload {}", kit::rate(down), kit::rate(up)),
        )
        .clicked()
        {
            cx.navigate("connections");
        }
    });
    p.fill(&servers::DETAILS, "traffic", 30, |ui, cx, s: &Server| {
        let (up, down) = cx.live(&TRAFFIC).by_server(s.id);
        if up + down > 0 {
            kit::property(
                ui,
                "Traffic",
                &format!("↓ {}  ↑ {} this session", kit::bytes(down), kit::bytes(up)),
            );
        }
    });
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
        let mut t = Traffic::default();
        t.update(&serde_json::json!({"uploadTotal": 2, "downloadTotal": 300, "connections": [conn("a", 100), conn("b", 200)]}));
        assert_eq!(t.by_server(server), (2, 300));
        t.update(&serde_json::json!({"uploadTotal": 3, "downloadTotal": 450, "connections": [conn("b", 350)]}));
        assert_eq!(
            t.by_server(server),
            (2, 450),
            "a closed with 100, b is at 350"
        );
        assert_eq!(t.conns[0].target, "example.org:443");
    }
}
