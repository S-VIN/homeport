//! The Connections tab: what goes where, by which rule, through which server.
use crate::{Close, traffic::Conn};
use homeport_kernel::{
    core::Labels,
    kit::{self, theme},
    prelude::*,
};

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

/// The filter row with "Close all", then the list.
pub fn show(ui: &mut egui::Ui, cx: &mut UiCx, conns: &[Conn], online: bool, filter: &mut String) {
    let labels = cx.core().labels();
    let mut close_all = false;
    ui.horizontal(|ui| {
        let button = 104.;
        let width = (ui.available_width() - button - theme::HORIZONTAL_GAP).max(80.);
        kit::input(
            ui,
            "filter",
            filter,
            "Filter by host, rule or server",
            width,
        );
        close_all = kit::Button::secondary("Close all")
            .icon(Icon::Close)
            .tone(kit::Tone::Danger)
            .enabled(online && !conns.is_empty())
            .show(ui)
            .on_hover_text("Apps may reconnect automatically.")
            .clicked();
    });
    ui.add_space(theme::SPACE_M);
    if close_all {
        cx.submit("close-all", Close(None));
    }
    if let Some(Ok(())) = cx.outcome("close-all") {
        cx.notify("All connections closed. Apps may reconnect.");
    }
    let needle = filter.trim().to_lowercase();
    let matches = |c: &Conn, route: &str| {
        needle.is_empty()
            || [c.target.as_str(), c.rule.as_str(), route]
                .iter()
                .any(|s| s.to_lowercase().contains(&needle))
    };
    let mut rows: Vec<(Conn, String)> = conns
        .iter()
        .map(|c| {
            // Rules in the user's words ("*.youtube.com"), not the core's.
            let rule = labels.name(&c.rule).to_owned();
            (Conn { rule, ..c.clone() }, route(&labels, &c.chain))
        })
        .filter(|(c, r)| matches(c, r))
        .collect();
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
                    filter.clear();
                }
            });
        }
        return;
    }
    kit::page_scroll(ui, "connections", |ui| {
        kit::list_card(ui, |ui| {
            for (c, via) in &rows {
                row(ui, cx, c, via, online);
            }
        });
    });
}

/// Target and how it goes on the left, bytes and a close button on the right.
fn row(ui: &mut egui::Ui, cx: &mut UiCx, c: &Conn, via: &str, online: bool) {
    kit::row(ui, ("conn", &c.id), false, |ui, _| {
        kit::row_line(ui, 54., |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close = kit::IconButton::new(Icon::Close, "Close this connection")
                    .enabled(online)
                    .emphasis(kit::Emphasis::Quiet)
                    .danger()
                    .show(ui);
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
                            kit::duration(c.started.elapsed().as_secs()),
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
