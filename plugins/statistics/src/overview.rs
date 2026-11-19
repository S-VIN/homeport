//! The Overview tab: totals, the speed of the last five minutes, the last 30
//! days and each server.
use crate::{
    saved::{self, Bytes, Day},
    traffic::{HISTORY, Traffic},
};
use egui_plot::{AxisHints, GridMark, Plot, PlotPoints};
use homeport_kernel::{
    kit::{self, theme},
    prelude::*,
};
use plugin_servers as servers;
use std::collections::VecDeque;

pub fn total((up, down): Bytes) -> String {
    kit::bytes(up + down)
}
pub fn split((up, down): Bytes) -> String {
    format!("↓ {} · ↑ {}", kit::bytes(down), kit::bytes(up))
}

pub fn show(ui: &mut egui::Ui, snap: &Snapshot, live: &Traffic) {
    let today = saved::today();
    let month = saved::month_of(&today);
    let days = saved::days(snap, live);
    let sum = |keep: &dyn Fn(&Day) -> bool| {
        (days.iter().filter(|d| keep(d))).fold((0, 0), |(u, d), x| (u + x.up, d + x.down))
    };
    let all = sum(&|_| true);
    kit::card(ui, |ui| {
        kit::stats(
            ui,
            &[
                ("Today", total(sum(&|d| d.day == today)), None),
                (
                    "This month",
                    total(sum(&|d| saved::month_of(&d.day) == month)),
                    None,
                ),
                ("All time", total(all), None),
            ],
        );
    });
    let moving = live.speed.iter().any(|s| s.0 + s.1 > 0);
    // Nothing went through yet: one empty state instead of three.
    if !moving && all.0 + all.1 == 0 {
        kit::card(ui, |ui| {
            kit::empty_with(
                ui,
                Icon::Activity,
                "No traffic yet",
                "Speed, days and servers show up once apps use the proxy.",
            )
        });
        return;
    }
    let (down, up) = live.rate();
    let now = format!("↓ {} · ↑ {}", kit::rate(down), kit::rate(up));
    kit::section_note(
        ui,
        "Speed",
        &now,
        Some("The last 5 minutes: download is the filled line, upload the gray one."),
    );
    kit::card(ui, |ui| match moving {
        true => speed_chart(ui, &live.speed),
        false => kit::empty_with(
            ui,
            Icon::Activity,
            "No traffic right now",
            "The speed of apps that use the proxy shows up here.",
        ),
    });
    kit::section(ui, "Last 30 days", None);
    kit::card(ui, |ui| match all.0 + all.1 > 0 {
        true => days_chart(ui, &days),
        false => kit::empty(ui, Icon::ChartBar, "No data yet"),
    });
    servers_list(ui, snap, live);
}

/// Axis labels in the kit's caption style.
fn axis<'a>(ui: &egui::Ui, hints: AxisHints<'a>) -> AxisHints<'a> {
    hints
        .tick_label_font(egui::FontId::proportional(theme::CAPTION))
        .tick_label_color(kit::colors(ui).muted)
}

/// Download and upload speed, newest on the right.
fn speed_chart(ui: &mut egui::Ui, speed: &VecDeque<(u64, u64)>) {
    use egui_plot::Line;
    let pal = kit::colors(ui);
    let n = speed.len() as f64;
    // x: seconds before now (negative), y: bytes per second.
    let points = |pick: fn(&(u64, u64)) -> u64| -> PlotPoints<'static> {
        (speed.iter().enumerate())
            .map(|(i, s)| [i as f64 - n + 1., pick(s) as f64])
            .collect()
    };
    let peak = speed.iter().map(|s| s.0.max(s.1)).max().unwrap_or(0);
    let (step, top) = scale(peak);
    let left = -(HISTORY as f64 - 1.);
    still(Plot::new("speed"), step, top)
        .x_grid_spacer(|_| marks(60., 0..=4).map(|m| mark(-m, 60.)).collect())
        .custom_x_axes(vec![axis(
            ui,
            AxisHints::new_x().formatter(|m, _| match (-m.value).round() as u64 {
                0 => "now".into(),
                s => format!("{} min", s / 60),
            }),
        )])
        .custom_y_axes(vec![axis(
            ui,
            AxisHints::new_y()
                .formatter(|m, _| kit::rate(m.value.max(0.) as u64))
                .min_thickness(56.),
        )])
        .label_formatter(|h| match h {
            egui_plot::HoverPosition::NearDataPoint {
                plot_name,
                position,
                ..
            } => {
                let ago = (-position.x).round().max(0.) as u64;
                let rate = kit::rate(position.y.max(0.) as u64);
                Some(format!("{plot_name} {rate}\n{ago} s ago"))
            }
            egui_plot::HoverPosition::Elsewhere { .. } => None,
        })
        .show(ui, |plot| {
            // The last label ("now") needs room before the edge.
            plot.set_plot_bounds_x(left..=12.);
            plot.set_plot_bounds_y(0.0..=top);
            plot.line(
                Line::new("Download", points(|s| s.0))
                    .color(pal.accent)
                    .fill(0.),
            );
            plot.line(Line::new("Upload", points(|s| s.1)).color(pal.muted));
        });
}

/// Data per day for the last 30 days (days without traffic too).
fn days_chart(ui: &mut egui::Ui, days: &[Day]) {
    use egui_plot::{Bar, BarChart};
    const N: usize = 30;
    let pal = kit::colors(ui);
    let now = chrono::Local::now().date_naive();
    let dates: Vec<chrono::NaiveDate> = (0..N as u64)
        .rev()
        .map(|i| now - chrono::Days::new(i))
        .collect();
    let label = |i: usize| {
        dates
            .get(i)
            .map(|d| d.format("%b %-d").to_string())
            .unwrap_or_default()
    };
    let bytes: Vec<u64> = (dates.iter())
        .map(|date| {
            let key = date.format("%Y-%m-%d").to_string();
            days.iter()
                .find(|d| d.day == key)
                .map_or(0, |d| d.up + d.down)
        })
        .collect();
    let bars: Vec<Bar> = (bytes.iter().enumerate())
        .map(|(i, b)| Bar::new(i as f64, *b as f64).width(0.7))
        .collect();
    let (step, top) = scale(bytes.iter().copied().max().unwrap_or(0));
    let shown = still(Plot::new("days"), step, top)
        // A label a week apart, ending today.
        .x_grid_spacer(|_| {
            marks(7., 0..=4)
                .map(|m| mark((N - 1) as f64 - m, 7.))
                .collect()
        })
        .custom_x_axes(vec![axis(
            ui,
            AxisHints::new_x().formatter(move |m, _| label(m.value.round().max(0.) as usize)),
        )])
        .custom_y_axes(vec![axis(
            ui,
            AxisHints::new_y()
                .formatter(|m, _| kit::bytes(m.value.max(0.) as u64))
                .min_thickness(56.),
        )])
        .label_formatter(|_| None)
        .show(ui, |plot| {
            plot.set_plot_bounds_x(-0.6..=N as f64 + 0.4);
            plot.set_plot_bounds_y(0.0..=top);
            plot.bar_chart(
                BarChart::new("Data", bars)
                    .color(pal.accent)
                    .allow_hover(false),
            );
            plot.pointer_coordinate()
                .map(|p| p.x.round())
                .filter(|x| (0.0..N as f64).contains(x))
                .map(|x| x as usize)
        });
    if let Some(i) = shown.inner {
        let text = format!("{}: {}", dates[i].format("%b %-d"), kit::bytes(bytes[i]));
        shown.response.on_hover_text_at_pointer(text);
    }
}

/// Grid step and top of an axis up to `max` bytes: round sizes (¼, ½, 1, 2,
/// 5 … in KB, MB, GB), at most four steps; a top near 1000 KB is 1 MB.
fn scale(max: u64) -> (f64, f64) {
    let max = max.max(1024) as f64;
    let mut unit = 1.;
    while max / unit >= 512. {
        unit *= 1024.;
    }
    let step = [
        0.25, 0.5, 1., 2., 5., 10., 20., 50., 100., 200., 500., 1024.,
    ]
    .into_iter()
    .map(|k| k * unit)
    .find(|step| (max / step).ceil() <= 4.)
    .unwrap_or(1024. * unit);
    (step, (max / step).ceil() * step)
}
fn marks(step: f64, n: std::ops::RangeInclusive<u32>) -> impl Iterator<Item = f64> {
    n.map(move |i| f64::from(i) * step)
}
fn mark(value: f64, step_size: f64) -> GridMark {
    GridMark { value, step_size }
}

/// A plot that only shows: no zoom, drag or scroll (the page scrolls); the
/// value axis has round steps up to `top`.
fn still(plot: Plot<'_>, step: f64, top: f64) -> Plot<'_> {
    let count = (top / step).round() as u32;
    plot.height(140.)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .show_crosshair(false)
        .show_grid([false, true])
        .y_grid_spacer(move |_| marks(step, 0..=count).map(|v| mark(v, step)).collect())
}

/// Servers that carried data, the most this month first.
fn servers_list(ui: &mut egui::Ui, snap: &Snapshot, live: &Traffic) {
    let mut rows: Vec<(String, Bytes, Bytes)> = snap
        .get(&servers::SERVERS)
        .iter()
        .map(|s| {
            let (m, t) = saved::server_use(snap, live, s.id);
            (s.name.clone(), m, t)
        })
        .filter(|(_, _, t)| t.0 + t.1 > 0)
        .collect();
    rows.sort_by_key(|(_, m, t)| std::cmp::Reverse((m.0 + m.1, t.0 + t.1)));
    kit::section(ui, "Servers", None);
    kit::list_card(ui, |ui| {
        if rows.is_empty() {
            kit::empty_with(
                ui,
                Icon::HardDrives,
                "No data yet",
                "Servers show up here once traffic goes through them.",
            );
            return;
        }
        const NUMBER: f32 = 90.;
        kit::list_header(ui, |ui| {
            ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
            kit::cell(
                ui,
                ui.available_width() - 2. * (NUMBER + theme::HORIZONTAL_GAP),
                |ui| {
                    kit::caption(ui, "Server");
                },
            );
            kit::cell_right(ui, NUMBER, |ui| kit::caption(ui, "This month"));
            kit::cell_right(ui, NUMBER, |ui| kit::caption(ui, "All time"));
        });
        for (name, month, all) in &rows {
            kit::row(ui, ("server", name), false, |ui, _| {
                kit::row_line(ui, theme::ROW, |ui| {
                    let width = ui.available_width() - 2. * (NUMBER + theme::HORIZONTAL_GAP);
                    kit::cell(ui, width, |ui| {
                        kit::truncated(ui, egui::RichText::new(name), Some(name));
                    });
                    kit::cell_right(ui, NUMBER, |ui| {
                        ui.label(total(*month)).on_hover_text(split(*month));
                    });
                    kit::cell_right(ui, NUMBER, |ui| {
                        ui.label(total(*all)).on_hover_text(split(*all));
                    });
                });
            });
        }
    });
}
