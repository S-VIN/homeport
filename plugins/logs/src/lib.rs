//! # logs
//! The live log, newest last: what homeport and its plugins did (the kernel's
//! [`logging`] lines) and, like the Logs page of other Clash-family apps, what
//! the core connected, refused or failed. Core connections are read only while
//! a window or browser tab is open; warnings and errors always.
mod ui;

use homeport_kernel::{core::Labels, logging, mihomo, prelude::*};
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
impl From<mihomo::LogLevel> for Level {
    fn from(level: mihomo::LogLevel) -> Self {
        match level {
            mihomo::LogLevel::Debug | mihomo::LogLevel::Info => Level::Info,
            mihomo::LogLevel::Warning => Level::Warning,
            mihomo::LogLevel::Error => Level::Error,
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
fn line(raw: mihomo::LogLine, labels: &Labels, reading: bool) -> Option<Line> {
    let level = Level::from(raw.level);
    if level == Level::Info && !reading {
        return None;
    }
    Some(Line {
        at: now(),
        level,
        internal: internal(&raw.payload),
        text: readable(&raw.payload, labels),
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
        "using" | "dial" => homeport_kernel::mihomo::config::internal_group(p[1]),
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
        if let Ok(mut stream) = cx.core().logs(mihomo::LogLevel::Info).await {
            let mut fresh = Vec::new();
            let mut flush = tokio::time::interval(Duration::from_millis(250));
            flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    raw = stream.next() => {
                        let Some(raw) = raw else { break };
                        fresh.extend(line(raw, &cx.core().labels(), cx.ui_open()));
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
    p.page::<ui::LogsPage>(PageSpec {
        title: "Logs",
        icon: Icon::Logs,
        order: 37,
    });
    Ok(())
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
