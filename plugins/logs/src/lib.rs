//! # logs
//! The core's live log, like the Logs page of other Clash-family apps: what
//! mihomo connected, refused or failed, newest last. It is read only while a
//! window or browser tab is open.
use rproxy_kernel::{kit, kit::theme, prelude::*};
use std::time::Duration;

pub static PLUGIN: Plugin = Plugin {
    id: "logs",
    name: "Logs",
    description: "The core's live log: connections, rule matches, errors.",
    category: categories::NETWORK,
    requires: &["servers"],
    setup,
    ..Plugin::DEFAULT
};

/// Lines kept in memory; older ones drop off.
const KEEP: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warning,
    Error,
}
impl Level {
    fn of(kind: &str) -> Level {
        match kind {
            "warning" => Level::Warning,
            "error" | "fatal" => Level::Error,
            _ => Level::Info,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Unix seconds when it arrived.
    pub at: u64,
    pub level: Level,
    /// Server ids already replaced by server names.
    pub text: String,
    /// rproxy's own work (server tests, start-up), not the user's traffic.
    pub internal: bool,
}
/// The last [`KEEP`] lines of the core's log.
pub static LINES: State<Vec<Line>> = State::new("logs.lines");

/// Forget the lines shown so far.
pub struct Clear;
impl Op for Clear {
    type Reply = ();
}

fn line(raw: &[u8], snap: &Snapshot) -> Option<Line> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let text = v["payload"].as_str().unwrap_or("");
    Some(Line {
        at: now(),
        level: Level::of(v["type"].as_str().unwrap_or("")),
        internal: internal(text),
        text: with_names(text, snap),
    })
}

/// Server tests run through rproxy's own listeners and groups; start-up
/// messages repeat on every configuration change.
fn internal(text: &str) -> bool {
    const CHATTER: [&str; 6] = [
        "Start initial",
        "Initial configuration",
        "Geodata Loader",
        "Geosite Matcher",
        "Sniffer is closed",
        "configuration in progress",
    ];
    text.contains("checks-")
        || text.contains(" test-")
        || text.contains("routed")
        || CHATTER.iter().any(|c| text.contains(c))
}

/// `main[29fbe608-…]` → `proxy[Japan Tokyo 53]`: the names the user knows.
fn with_names(text: &str, snap: &Snapshot) -> String {
    let servers = snap.get(&plugin_servers::SERVERS);
    let mut out = text.replace(" main[", " proxy[");
    let ids: Vec<&str> = out
        .split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .filter(|w| w.len() == 36 && w.parse::<Id>().is_ok())
        .collect();
    let ids: Vec<String> = ids.into_iter().map(str::to_owned).collect();
    for id in ids {
        if let Some(s) = servers.iter().find(|s| s.id.to_string() == id) {
            out = out.replace(&id, &s.name);
        }
    }
    out
}

/// Add new lines, a few times a second at most: every change repaints the UI.
fn publish(cx: &Cx, fresh: &mut Vec<Line>) {
    if fresh.is_empty() {
        return;
    }
    let fresh = std::mem::take(fresh);
    cx.update_live(&LINES, |all| {
        all.extend(fresh);
        let over = all.len().saturating_sub(KEEP);
        all.drain(..over);
    });
}

/// Follow the core's log while a window or browser tab is open (nobody reads
/// it otherwise), and come back when the core restarts.
async fn follow(cx: Cx) -> Result<()> {
    loop {
        let stream = match cx.ui_open() {
            true => cx.core().logs("info").await.ok(),
            false => None,
        };
        if let Some(mut stream) = stream {
            let (mut raw, mut fresh) = (Vec::new(), Vec::new());
            let mut flush = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    chunk = stream.chunk() => {
                        let Ok(Some(chunk)) = chunk else { break };
                        // Bytes, not text: a character may span two chunks.
                        raw.extend_from_slice(&chunk);
                        let snap = cx.snapshot();
                        while let Some(end) = raw.iter().position(|b| *b == b'\n') {
                            let bytes: Vec<u8> = raw.drain(..=end).collect();
                            fresh.extend(line(&bytes, &snap));
                        }
                    }
                    _ = flush.tick() => {
                        publish(&cx, &mut fresh);
                        if !cx.ui_open() {
                            break;
                        }
                    }
                }
            }
            publish(&cx, &mut fresh);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn setup(p: &mut Setup) -> Result<()> {
    p.live(&LINES, Vec::new);
    p.handle(|cx: Cx, _: Clear| async move {
        cx.set_live(&LINES, Vec::new());
        Ok(())
    });
    p.service("follow", |_| Some(()), |cx: Cx, ()| follow(cx));
    p.page_fn(
        PageSpec {
            id: "logs",
            title: "Logs",
            icon: Icon::Logs,
            order: 37,
        },
        page,
    );
    Ok(())
}

/// What the page shows, kept per window: the lowest level and rproxy's own lines.
fn view_id() -> egui::Id {
    egui::Id::unique("logs-view")
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let lines = cx.live(&LINES);
    let (mut min, mut all): (Level, bool) = ui
        .data(|d| d.get_temp(view_id()))
        .unwrap_or((Level::Info, false));
    let shown: Vec<&Line> = lines
        .iter()
        .filter(|l| l.level >= min && (all || !l.internal || l.level > Level::Info))
        .collect();
    let counter = kit::count(shown.len(), "line", "lines");
    let mut clear = false;
    kit::page_header(ui, "Logs", &counter, |ui| {
        clear = kit::Button::ghost("Clear")
            .icon(Icon::Delete)
            .enabled(!lines.is_empty())
            .show(ui)
            .clicked();
    });
    let options = [
        (Level::Info, "All"),
        (Level::Warning, "Warnings"),
        (Level::Error, "Errors"),
    ];
    ui.horizontal(|ui| {
        let level = kit::segmented(ui, &mut min, &options);
        ui.add_space(theme::SPACE_L);
        let internal = kit::toggle(ui, &mut all, "Server tests and start-up").changed();
        if level || internal {
            ui.data_mut(|d| d.insert_temp(view_id(), (min, all)));
        }
    });
    ui.add_space(theme::SPACE_M);
    if clear {
        cx.run(Clear);
    }
    kit::list_card(ui, |ui| {
        if shown.is_empty() {
            kit::empty_with(
                ui,
                Icon::Logs,
                "No log lines yet",
                "New lines appear here as the core works.",
            );
            return;
        }
        let pal = kit::colors(ui);
        egui::ScrollArea::vertical()
            .id_salt("log-lines")
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for l in shown {
                    let ink = match l.level {
                        Level::Info => pal.foreground,
                        Level::Warning => pal.warning,
                        Level::Error => pal.danger,
                    };
                    // Age on the left; wrapped text keeps its own column.
                    ui.horizontal_top(|ui| {
                        kit::cell(ui, 64., |ui| {
                            kit::hint(ui, kit::age(l.at));
                        });
                        let text = egui::RichText::new(&l.text)
                            .monospace()
                            .size(theme::SMALL)
                            .color(ink);
                        ui.add(egui::Label::new(text).wrap());
                    });
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_traffic_and_start_up_are_internal() {
        assert!(internal(
            "[TCP] 127.0.0.1:5 --> a.com:443 match InName/checks-1 using test-1[x]"
        ));
        assert!(internal("Start initial compatible provider main"));
        assert!(!internal(
            "[TCP] 127.0.0.1:5 --> a.com:443 match Match using main[x]"
        ));
    }
}
