//! Tests on screen: Test all, the Rating, Latency and Speed columns, a
//! server's last test in its details and the connection card, and the
//! interval in Settings.
use super::*;
use crate::{COLUMNS, CONNECTION, DETAILS, TOOLBAR};
use homeport_kernel::columns::Column;

/// The reading of check `id` of a server, if tested.
fn reading(snap: &Snapshot, server: Id, id: &str) -> Option<Reading> {
    last_test(snap, server)?.scores.0.get(id).cloned()
}
/// "3 of 4 passed · Speed 1.2 MB/s"-style line of every check, for tooltips.
fn summary(snap: &Snapshot, s: &Server) -> String {
    match last_test(snap, s.id) {
        None => "Not tested yet".into(),
        Some(h) if h.unreachable() => h.failed_text(),
        Some(h) => {
            let latency = h.ping_ms.map(|ms| format!("Latency: {ms} ms"));
            let lines: Vec<String> = latency
                .into_iter()
                .chain((checks(snap).iter()).filter_map(|c| {
                    Some(format!(
                        "{}: {}",
                        c.spec.title,
                        h.scores.0.get(c.spec.id)?.text
                    ))
                }))
                .collect();
            lines.join(" · ")
        }
    }
}

pub(super) fn register(p: &mut Setup) {
    p.fill(&TOOLBAR, "test-all", 20, |ui, cx, _| {
        let ok = cx.core_status().running();
        cx.job_button(ui, Icon::Gauge, "Test all", ok, || TestAll);
    });
    let rating = Column::<Server> {
        id: "rating",
        order: 10,
        title: "Rating",
        help: "0–100 from latency and the tests",
        secondary: false,
        sort: Some(|snap, s| -(rating_of(snap, s.id).map_or(-1, i64::from))),
    };
    p.column(&COLUMNS, rating, |ui, cx, s| {
        if cx.live(&IN_FLIGHT).contains(&s.id) {
            kit::spinner(ui).on_hover_text("Testing…");
            return;
        }
        // An unreachable server has no rating worth a red 0: it says it failed.
        let h = last_test(cx.snapshot(), s.id);
        let r = if h.as_ref().is_some_and(TestResult::unreachable) {
            kit::tone_text(ui, "Failed", kit::Tone::Warning)
        } else {
            let rating = rating_of(cx.snapshot(), s.id);
            let text = rating.map_or_else(|| "—".into(), |n| n.to_string());
            ui.colored_label(kit::score_color(ui, rating), text)
        };
        r.on_hover_text(summary(cx.snapshot(), s));
    });
    let latency = Column::<Server> {
        id: "latency",
        order: 20,
        title: "Latency",
        help: "Response time through the server",
        secondary: true,
        sort: Some(|snap, s| {
            (last_test(snap, s.id).and_then(|h| h.ping_ms)).map_or(i64::MAX, i64::from)
        }),
    };
    p.column(&COLUMNS, latency, |ui, cx, s| {
        let ping = last_test(cx.snapshot(), s.id).and_then(|h| h.ping_ms);
        muted_unless(
            ui,
            ping.is_some(),
            ping.map_or_else(|| "—".into(), |n| format!("{n} ms")),
        );
    });
    let speed = Column::<Server> {
        id: "speed",
        order: 30,
        title: "Speed",
        help: "Download speed in the last test",
        secondary: true,
        sort: Some(|snap, s| reading(snap, s.id, "speed").map_or(1, |r| -(r.value * 1e6) as i64)),
    };
    p.column(&COLUMNS, speed, |ui, cx, s| {
        let speed = reading(cx.snapshot(), s.id, "speed").filter(|r| r.value > 0.);
        muted_unless(
            ui,
            speed.is_some(),
            speed.map_or_else(|| "—".into(), |r| r.text),
        );
    });
    p.fill(&DETAILS, "tests", 10, details);
    p.fill(&CONNECTION, "tests", 10, connection_stats);
    p.fill(&SETTINGS_SECTIONS, "interval", 0, |ui, cx, _| {
        cx.setting(
            ui,
            "Auto test",
            &TEST_SETTINGS,
            |s| &mut s.check_secs,
            |ui, v| kit::interval(ui, "check", v),
        );
    });
}

fn muted_unless(ui: &mut egui::Ui, known: bool, text: String) {
    let pal = kit::colors(ui);
    ui.colored_label(if known { pal.foreground } else { pal.faint }, text);
}

/// The last test of one server in its expanded row: when, with "Test now",
/// then latency and each check.
fn details(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server) {
    let snap = cx.snapshot().clone();
    let pal = kit::colors(ui);
    let h = last_test(&snap, s.id);
    let checked = (h.as_ref())
        .and_then(|h| h.checked)
        .map_or_else(|| "Never".into(), kit::age);
    kit::property_with(ui, "Last test", |ui| {
        ui.label(egui::RichText::new(checked).color(pal.foreground));
        // A test of just this server, with its outcome right here.
        // Rows stay in place while it runs: the button turns into a spinner.
        let key = ("check", s.id);
        let testing = cx.live(&IN_FLIGHT).contains(&s.id);
        match cx.request::<TestServer>(key) {
            _ if testing => {
                kit::spinner(ui).on_hover_text("Testing…");
            }
            Request::Pending => {
                kit::spinner(ui);
            }
            Request::Done(Ok(None)) => {
                kit::hint(ui, "Skipped: excluded");
            }
            Request::Done(Err(e)) => {
                kit::tone_text(ui, e, kit::Tone::Warning);
            }
            Request::Idle | Request::Done(Ok(Some(_))) => {
                if kit::Button::ghost("Test now")
                    .icon(Icon::Gauge)
                    .show(ui)
                    .clicked()
                {
                    cx.submit(key, TestServer { id: s.id });
                }
            }
        }
    });
    // Then what it measured; a check's own lines (say, each test URL) follow it.
    let ping = h.as_ref().and_then(|h| h.ping_ms);
    kit::property(
        ui,
        "Latency",
        &ping.map_or_else(|| "—".into(), |ms| format!("{ms} ms")),
    );
    for c in checks(&snap) {
        let text = (h.as_ref())
            .and_then(|h| h.scores.0.get(c.spec.id))
            .map(|r| r.text.clone());
        kit::property(ui, c.spec.title, &text.unwrap_or_else(|| "—".into()));
    }
}

/// Latency, rating and the quick checks of the connected server.
fn connection_stats(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server) {
    let snap = cx.snapshot().clone();
    let h = last_test(&snap, s.id);
    let rating = rating_of(&snap, s.id);
    let mut items = vec![
        (
            "Latency",
            (h.as_ref().and_then(|h| h.ping_ms))
                .map_or_else(|| "—".into(), |ms| format!("{ms} ms")),
            None,
        ),
        // The same word as the Rating column.
        if let Some(h) = h.as_ref().filter(|h| h.unreachable()) {
            let text = match &h.failure {
                Some(why) => format!("Failed · {}", why.to_lowercase()),
                None => "Failed".into(),
            };
            ("Rating", text, Some(kit::colors(ui).warning))
        } else {
            (
                "Rating",
                rating.map_or_else(|| "—".into(), |r| r.to_string()),
                Some(kit::score_color(ui, rating)),
            )
        },
    ];
    for c in checks(&snap).iter().filter(|c| c.spec.quick) {
        if let Some(r) = h.as_ref().and_then(|h| h.scores.0.get(c.spec.id)) {
            items.push((c.spec.title, r.text.clone(), None));
        }
    }
    kit::stats(ui, &items);
    ui.add_space(kit::theme::SPACE_S);
}
