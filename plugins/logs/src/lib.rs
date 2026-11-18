//! # logs
//! The live log, newest last: what homeport and its plugins did (the kernel's
//! [`logging`] lines) and, like the Logs page of other Clash-family apps, what
//! the core connected, refused or failed. Core connections are read only while
//! a window or browser tab is open; warnings and errors always.
use homeport_kernel::{core::Labels, kit, kit::theme, logging, prelude::*};
use std::time::Duration;

pub static PLUGIN: Plugin = Plugin {
    id: "logs",
    name: "Logs",
    description: "The live log of the app and the core: what changed, connections, errors.",
    category: categories::NETWORK,
    setup,
    ..Plugin::DEFAULT
};

/// Lines kept in memory; older ones drop off.
const KEEP: usize = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    #[default]
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
    /// homeport's own work (server tests, start-up), not the user's traffic.
    pub internal: bool,
}
/// The last [`KEEP`] lines of the core's log.
pub static LINES: State<Vec<Line>> = State::new("logs.lines");
/// The app's own lines since the last [`Clear`].
pub static APP: State<Vec<logging::Entry>> = State::new("logs.app");
/// The last app line [`Clear`] dropped.
static CLEARED: State<u64> = State::new("logs.cleared");

/// Forget the lines shown so far.
pub struct Clear;
impl Op for Clear {
    type Reply = ();
}

/// One line of the stream; `None` for an info line nobody is reading.
fn line(raw: &[u8], labels: &Labels, reading: bool) -> Option<Line> {
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
        text: readable(text, labels),
    })
}

/// Server tests run through homeport's own listeners and groups
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
            .is_some_and(homeport_kernel::mihomo::config::internal_listener)
    };
    // `InName(checks-3)` splits in two.
    let pair = |p: &[&str]| match p[0] {
        "using" | "dial" => p[1].starts_with("test-"),
        "InName" => homeport_kernel::mihomo::config::internal_listener(p[1]),
        _ => false,
    };
    words.iter().any(inbound)
        || words.windows(2).any(pair)
        || CHATTER.iter().any(|c| text.contains(c))
}

/// The user's words: `match DomainRegex(^(?:.*\.)?youtube\.com$) using main[…]` →
/// `match youtube.com using proxy[Japan Tokyo 53]`, and in warnings
/// `(match IPCIDR/10.0.0.0/8)` → `(match 10.0.0.0/8)`, as on the Connections page.
fn readable(text: &str, labels: &Labels) -> String {
    // The core's own words for the main group falling over.
    if text.contains(" failed multiple times, activate health check") {
        return "The proxy failed several times; the core re-checks its servers.".into();
    }
    match core_rule(text) {
        Some((at, rule)) => format!(
            "{}{}{}",
            labels.text(&text[..at.start]),
            labels.name(&rule),
            labels.text(&text[at.end..])
        ),
        None => labels.text(text),
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
    let mut changes = cx.core().changes();
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
                        let (labels, reading) = (cx.core().labels(), cx.ui_open());
                        while let Some(end) = raw.iter().position(|b| *b == b'\n') {
                            let bytes: Vec<u8> = raw.drain(..=end).collect();
                            fresh.extend(line(&bytes, &labels, reading));
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
        // Try again once the core (or anything) changes.
        if changes.changed().await.is_err() {
            return Ok(());
        }
    }
}

/// Mirror the app's log, a few times a second at most.
async fn follow_app(cx: Cx) -> Result<()> {
    let mut changes = logging::changes();
    loop {
        let from = *cx.live(&CLEARED);
        let fresh = logging::recent()
            .into_iter()
            .filter(|e| e.seq > from)
            .collect();
        cx.set_live(&APP, fresh);
        if changes.changed().await.is_err() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn setup(p: &mut Setup) -> Result<()> {
    p.live(&LINES, Vec::new);
    p.live(&APP, Vec::new);
    p.live(&CLEARED, || 0);
    p.handle(|cx: Cx, _: Clear| async move {
        cx.set_live(&LINES, Vec::new());
        cx.set_live(&CLEARED, logging::recent().last().map_or(0, |e| e.seq));
        cx.set_live(&APP, Vec::new());
        Ok(())
    });
    p.task("follow", follow);
    p.task("follow-app", follow_app);
    p.page::<LogsPage>(PageSpec {
        title: "Logs",
        icon: Icon::Logs,
        order: 37,
    });
    Ok(())
}

/// Whose lines the page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Source {
    #[default]
    All,
    /// homeport and its plugins.
    App,
    /// mihomo.
    Core,
}

/// What the page shows, kept per window: the lowest level, whose lines, and
/// the core's lines from homeport's own work.
#[derive(Default)]
struct LogsPage {
    min: Level,
    source: Source,
    all: bool,
}
impl Page for LogsPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        page(ui, cx, self);
    }
}

/// One line of either log, as the page draws it.
struct Row<'a> {
    at: u64,
    level: Level,
    /// The writing plugin of an app line; `None` for the core's.
    source: Option<&'static str>,
    text: &'a str,
    count: u32,
}
fn app_level(level: logging::Level) -> Level {
    match level {
        logging::Level::Error => Level::Error,
        logging::Level::Warn => Level::Warning,
        _ => Level::Info,
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx, view: &mut LogsPage) {
    let (lines, app) = (cx.live(&LINES), cx.live(&APP));
    let LogsPage { min, source, all } = *view;
    let core = lines
        .iter()
        .filter(|l| source != Source::App && l.level >= min && (all || !l.internal))
        .map(|l| Row {
            at: l.at,
            level: l.level,
            source: None,
            text: &l.text,
            count: 1,
        });
    let own = app
        .iter()
        .filter(|e| source != Source::Core && app_level(e.level) >= min)
        .map(|e| Row {
            at: e.at,
            level: app_level(e.level),
            source: Some(e.source),
            text: &e.text,
            count: e.count,
        });
    let mut shown: Vec<Row> = core.chain(own).collect();
    // Both logs are in time order: a stable sort interleaves them.
    shown.sort_by_key(|r| r.at);
    let counter = kit::count(shown.len(), "line", "lines");
    let mut clear = false;
    cx.page_header(ui, &counter, |ui, _| {
        clear = kit::Button::secondary("Clear")
            .icon(Icon::Delete)
            .enabled(!lines.is_empty() || !app.is_empty())
            .show(ui)
            .clicked();
    });
    let options = [
        (Level::Info, "All"),
        (Level::Warning, "Warnings"),
        (Level::Error, "Errors"),
    ];
    let sources = [
        (Source::All, "All"),
        (Source::App, "App"),
        (Source::Core, "Core"),
    ];
    // What does not fit side by side goes to the next line.
    ui.horizontal_wrapped(|ui| {
        kit::segmented(ui, &mut view.min, &options);
        ui.add_space(theme::SPACE_L);
        kit::segmented(ui, &mut view.source, &sources);
        if source != Source::App {
            ui.add_space(theme::SPACE_L);
            kit::toggle(ui, &mut view.all, "Server tests and reloads")
                .on_hover_text("The core's lines from server tests and configuration reloads");
        }
    });
    ui.add_space(theme::SPACE_M);
    if clear {
        cx.run(Clear);
    }
    // The core's lines from homeport's work the toggle hides at this level.
    let hidden = lines
        .iter()
        .filter(|l| source != Source::App && l.level >= min && l.internal && !all)
        .count();
    let mut show_hidden = false;
    kit::list_card(ui, |ui| {
        if shown.is_empty() {
            let title = match (min, source) {
                _ if lines.is_empty() && app.is_empty() => "No log lines yet",
                (Level::Info, Source::Core) => "No traffic lines",
                (Level::Info, _) => "No log lines",
                (Level::Warning, _) => "No warnings",
                (Level::Error, _) => "No errors",
            };
            let next = match hidden {
                0 if min > Level::Info => "Problems appear here when they happen.".to_owned(),
                0 => "New lines appear here as homeport works.".to_owned(),
                n => format!(
                    "{} from server tests and reloads hidden.",
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
        let font = egui::FontId::proportional(theme::SMALL);
        let age_width = ui.fonts_mut(|f| {
            f.layout_no_wrap("59\u{a0}min ago".into(), font.clone(), pal.muted)
                .size()
                .x
        }) + theme::SPACE_S;
        // Plugin names get a column of their own, so every message starts at one edge.
        const SOURCE_MAX: f32 = 140.;
        let source_width = ui.fonts_mut(|f| {
            (shown.iter().filter_map(|r| r.source))
                .map(|s| f.layout_no_wrap(s.into(), font.clone(), pal.muted).size().x)
                .fold(0., f32::max)
        });
        let source_width = match source_width {
            0. => 0.,
            w => w.min(SOURCE_MAX) + theme::SPACE_M,
        };
        let phone = kit::phone(ui);
        // The scroll edge keeps an inset from the card's border.
        let inset = theme::INSET - theme::SPACE_S;
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(0, theme::SPACE_S as i8))
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("log-lines")
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin::symmetric(inset as i8, 0))
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = theme::SPACE_XS;
                                // An age shows once for its run of lines, and again on
                                // the first visible line when its run starts above.
                                let mut last_age = String::new();
                                let mut age_visible = false;
                                for l in shown {
                                    let ink = match l.level {
                                        Level::Info => pal.foreground,
                                        Level::Warning => pal.warning,
                                        Level::Error => pal.danger,
                                    };
                                    let age_text = kit::age(l.at);
                                    // Mostly in view: a line cut at the top keeps its age.
                                    let visible = ui.cursor().top() + theme::SMALL / 2.
                                        >= ui.clip_rect().top();
                                    let repeated =
                                        age_text == last_age && (age_visible || !visible);
                                    age_visible |= visible;
                                    last_age = age_text.clone();
                                    let age = egui::RichText::new(if repeated {
                                        String::new()
                                    } else {
                                        age_text
                                    })
                                    .font(font.clone())
                                    .color(pal.muted);
                                    // Words wrap whole; one longer than the line breaks anywhere.
                                    // An app line starts with its plugin and ends with its repeats.
                                    let text = |width: f32, prefix: bool| {
                                        let mut job = egui::text::LayoutJob::default();
                                        job.wrap.max_width = width;
                                        let format =
                                            |color| egui::TextFormat::simple(font.clone(), color);
                                        if let Some(source) = l.source.filter(|_| prefix) {
                                            job.append(
                                                &format!("{source}  "),
                                                0.,
                                                format(pal.muted),
                                            );
                                        }
                                        job.append(l.text, 0., format(ink));
                                        if l.count > 1 {
                                            job.append(
                                                &format!("  ×{}", l.count),
                                                0.,
                                                format(pal.muted),
                                            );
                                        }
                                        job
                                    };
                                    if phone {
                                        // The age on its own line gives the text the full width.
                                        if !repeated {
                                            ui.label(age);
                                        }
                                        kit::text_label(
                                            ui,
                                            text(ui.available_width(), true),
                                            true,
                                            false,
                                        );
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
                                        if source_width > 0. {
                                            ui.allocate_ui_with_layout(
                                                egui::vec2(source_width, 0.),
                                                egui::Layout::top_down(egui::Align::Min),
                                                |ui| {
                                                    ui.set_width(source_width);
                                                    let name = egui::RichText::new(
                                                        l.source.unwrap_or_default(),
                                                    )
                                                    .font(font.clone())
                                                    .color(pal.muted);
                                                    ui.add(egui::Label::new(name).truncate());
                                                },
                                            );
                                        }
                                        kit::text_label(
                                            ui,
                                            text(ui.available_width(), false),
                                            true,
                                            false,
                                        );
                                    });
                                }
                            });
                    });
            });
    });
    view.all |= show_hidden;
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
