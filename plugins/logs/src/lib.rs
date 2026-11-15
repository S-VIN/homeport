//! # logs
//! The core's live log, like the Logs page of other Clash-family apps: what
//! mihomo connected, refused or failed, newest last. Connections are read only
//! while a window or browser tab is open; warnings and errors always.
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

/// One line of the stream; `None` for an info line nobody is reading.
fn line(raw: &[u8], snap: &Snapshot, reading: bool) -> Option<Line> {
    let v: serde_json::Value = serde_json::from_slice(raw).ok()?;
    let level = Level::of(v["type"].as_str().unwrap_or(""));
    if level == Level::Info && !reading {
        return None;
    }
    let text = v["payload"].as_str().unwrap_or("");
    Some(Line {
        at: now(),
        level,
        internal: internal(text),
        text: readable(text, snap),
    })
}

/// Server tests run through rproxy's own listeners and groups
/// (`dial test-3 (match InName/checks-3) …`, `… using test-1[…]`); start-up
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
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || "()[]".contains(c))
        .filter(|w| !w.is_empty())
        .collect();
    let inbound = |w: &&str| {
        w.strip_prefix("InName/")
            .is_some_and(rproxy_kernel::mihomo::config::internal_listener)
    };
    // `InName(checks-3)` splits in two.
    let pair = |p: &[&str]| match p[0] {
        "using" | "dial" => p[1].starts_with("test-"),
        "InName" => rproxy_kernel::mihomo::config::internal_listener(p[1]),
        _ => false,
    };
    words.iter().any(inbound)
        || words.windows(2).any(pair)
        || CHATTER.iter().any(|c| text.contains(c))
}

/// The user's words: `match DomainRegex(^(?:.*\.)?youtube\.com$) using …` →
/// `match youtube.com using …`, and in warnings `(match IPCIDR/10.0.0.0/8)` →
/// `(match 10.0.0.0/8)`, as on the Connections page.
fn readable(text: &str, snap: &Snapshot) -> String {
    match core_rule(text) {
        Some((at, rule)) => {
            let label = plugin_servers::rule_label(snap, &rule);
            with_names(
                &format!("{}{label}{}", &text[..at.start], &text[at.end..]),
                snap,
            )
        }
        None => with_names(text, snap),
    }
}

/// Where a line names the matching rule, and the rule as the connections list
/// writes it (`Kind payload`). Info lines say `match Kind(payload) using …`,
/// warnings `(match Kind/payload)`.
fn core_rule(text: &str) -> Option<(std::ops::Range<usize>, String)> {
    let (start, end, split) = if let Some(i) = text.rfind(" match ")
        && let Some(len) = text[i + 7..].find(" using ")
    {
        (i + 7, i + 7 + len, '(')
    } else {
        let i = text.find("(match ")? + 7;
        (i, i + text[i..].find(") ")?, '/')
    };
    let rule = text[start..end].trim_end();
    let end = start + rule.len();
    let rule = match rule.split_once(split) {
        Some((kind, payload)) => {
            let payload = if split == '(' {
                payload.strip_suffix(')')?
            } else {
                payload
            };
            // The final rule has no payload: `(match Match/)`.
            match payload {
                "" => kind.to_owned(),
                payload => format!("{kind} {payload}"),
            }
        }
        None => rule.to_owned(),
    };
    Some((start..end, rule))
}

/// `main[29fbe608-…]` → `proxy[Japan Tokyo 53]`, `dial main` → `dial proxy`:
/// the names the user knows.
fn with_names(text: &str, snap: &Snapshot) -> String {
    // The core's own words for the main group falling over.
    if text.contains(" failed multiple times, activate health check") {
        return "The proxy failed several times; the core re-checks its servers.".into();
    }
    let mut out = text
        .replace(" main[", " proxy[")
        .replace("dial main ", "dial proxy ");
    let ids: Vec<(String, Id)> = out
        .split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .filter(|w| w.len() == 36)
        .filter_map(|w| Some((w.to_owned(), w.parse::<Id>().ok()?)))
        .collect();
    if ids.is_empty() {
        return out;
    }
    let servers = snap.get(&plugin_servers::SERVERS);
    for (word, id) in ids {
        if let Some(s) = servers.iter().find(|s| s.id == id) {
            out = out.replace(&word, &s.name);
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

/// Follow the core's log, and come back when the core restarts. Without a
/// window only warnings and errors are kept: they explain what went wrong
/// when the page is opened later; every connection is kept only for a reader.
/// (The core answers only once it has a line of the asked level, so the
/// level stays `info` and the filter is here.)
async fn follow(cx: Cx) -> Result<()> {
    loop {
        if let Ok(mut stream) = cx.core().logs("info").await {
            let (mut raw, mut fresh) = (Vec::new(), Vec::new());
            let mut flush = tokio::time::interval(Duration::from_millis(250));
            flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    chunk = stream.chunk() => {
                        let Ok(Some(chunk)) = chunk else { break };
                        // Bytes, not text: a character may span two chunks.
                        raw.extend_from_slice(&chunk);
                        let (snap, reading) = (cx.snapshot(), cx.ui_open());
                        while let Some(end) = raw.iter().position(|b| *b == b'\n') {
                            let bytes: Vec<u8> = raw.drain(..=end).collect();
                            fresh.extend(line(&bytes, &snap, reading));
                        }
                        let over = fresh.len().saturating_sub(KEEP);
                        fresh.drain(..over);
                    }
                    // Nobody to show it to: hold it without waking the UI.
                    _ = flush.tick() => if cx.ui_open() {
                        publish(&cx, &mut fresh);
                    },
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
        .filter(|l| l.level >= min && (all || !l.internal))
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
    // The toggle goes under the levels when they don't fit side by side.
    ui.horizontal_wrapped(|ui| {
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
    // rproxy's own lines the toggle hides at this level.
    let hidden = lines
        .iter()
        .filter(|l| l.level >= min && l.internal && !all)
        .count();
    let mut show_hidden = false;
    kit::list_card(ui, |ui| {
        if shown.is_empty() {
            let title = match min {
                _ if lines.is_empty() => "No log lines yet",
                Level::Info => "No traffic lines",
                Level::Warning => "No warnings",
                Level::Error => "No errors",
            };
            let next = match hidden {
                0 => "New lines appear here as the core works.".to_owned(),
                n => format!(
                    "{} from server tests and start-up hidden.",
                    kit::plural(n, "line", "lines")
                ),
            };
            kit::empty_with(ui, Icon::Logs, title, &next);
            if hidden > 0 {
                ui.vertical_centered(|ui| {
                    show_hidden = kit::Button::secondary("Show them").show(ui).clicked();
                });
                ui.add_space(theme::SPACE_XL);
            }
            return;
        }
        let pal = kit::colors(ui);
        // The widest age, so every message starts at the same edge.
        let font = egui::FontId::monospace(theme::SMALL);
        let age_width = ui.fonts_mut(|f| {
            f.layout_no_wrap("59\u{a0}min ago".into(), font.clone(), pal.muted)
                .size()
                .x
        }) + theme::SPACE_S;
        let phone = kit::phone(ui);
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
                    let age = egui::RichText::new(kit::age(l.at))
                        .font(font.clone())
                        .color(pal.muted);
                    // Words wrap whole; one longer than the line breaks anywhere.
                    let text = |width: f32| {
                        egui::text::LayoutJob::simple(l.text.clone(), font.clone(), ink, width)
                    };
                    if phone {
                        // The age on its own line gives the text the full width.
                        ui.label(age);
                        kit::text_label(ui, text(ui.available_width()), true, false);
                        ui.add_space(theme::SPACE_S);
                        continue;
                    }
                    // Age on the left in the same type, so both share the
                    // first line; wrapped text keeps its own column.
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(age_width, 0.),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_width(age_width);
                                ui.label(age);
                            },
                        );
                        kit::text_label(ui, text(ui.available_width()), true, false);
                    });
                }
            });
    });
    if show_hidden {
        ui.data_mut(|d| d.insert_temp(view_id(), (min, true)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_found_in_both_line_forms() {
        let rule = |text: &str| core_rule(text).map(|(at, rule)| (text[at].to_owned(), rule));
        assert_eq!(
            rule(
                r"[TCP] a --> youtube.com:443 match DomainRegex(^(?:.*\.)?youtube\.com$) using main[x]"
            ),
            Some((
                r"DomainRegex(^(?:.*\.)?youtube\.com$)".into(),
                r"DomainRegex ^(?:.*\.)?youtube\.com$".into()
            ))
        );
        assert_eq!(
            rule("[TCP] a --> b:443 match Match using DIRECT"),
            Some(("Match".into(), "Match".into()))
        );
        assert_eq!(
            rule(
                "[TCP] dial DIRECT (match IPCIDR/10.0.0.0/8) 1.2.3.4:5 --> 10.0.0.1:80 error: i/o timeout"
            ),
            Some(("IPCIDR/10.0.0.0/8".into(), "IPCIDR 10.0.0.0/8".into()))
        );
        assert_eq!(
            rule("[TCP] dial main (match Match/) 1.2.3.4:5 --> a.com:443 error: timeout"),
            Some(("Match/".into(), "Match".into()))
        );
        assert_eq!(rule("Start initial compatible provider main"), None);
    }

    #[test]
    fn test_traffic_and_start_up_are_internal() {
        assert!(internal(
            "[TCP] 127.0.0.1:5 --> a.com:443 match InName/checks-1 using test-1[x]"
        ));
        assert!(internal("Start initial compatible provider main"));
        assert!(internal(
            "[TCP] 127.0.0.1:5 --> a.com:443 match InName(checks-2) using test-2[x]"
        ));
        assert!(internal(
            "[TCP] dial test-3 (match InName/checks-3) 127.0.0.1:5 --> speed.cloudflare.com:443 error: i/o timeout"
        ));
        assert!(!internal(
            "[TCP] 127.0.0.1:5 --> a.com:443 match Match using main[x]"
        ));
        assert!(!internal(
            "[TCP] 127.0.0.1:5 --> routed.checks-1.com:443 match Match using main[x]"
        ));
    }
}
