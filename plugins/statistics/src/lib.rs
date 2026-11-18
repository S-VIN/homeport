//! # statistics
//! How much data went through the core: today, this month and all time, the
//! last 30 days and each server, saved across restarts; plus the speed of the
//! last five minutes.
//!
//! A task reads the core's connections every second (every 5 s with no window
//! open) and counts the bytes each one moved since the last reading; checks,
//! speed tests and the app's own downloads are not counted. The sums are saved
//! every 30 s and when the day changes.
use homeport_kernel::{
    kit::{self, theme},
    prelude::*,
};
use plugin_servers::{self as servers, Server};
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

pub static PLUGIN: Plugin = Plugin {
    id: "statistics",
    name: "Statistics",
    description: "How much data went through: today, this month, per day and per server.",
    requires: &[&plugin_servers::PLUGIN],
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

/// Bytes on one local day.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "statistics_days")]
pub struct Day {
    pub id: Id,
    /// `2026-11-18`, local time.
    pub day: String,
    pub up: u64,
    pub down: u64,
}
/// Bytes through one server.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "statistics_servers")]
pub struct ServerUse {
    pub id: Id,
    pub server: Id,
    /// The month of `month_up`/`month_down`: `2026-11`.
    pub month: String,
    pub month_up: u64,
    pub month_down: u64,
    pub total_up: u64,
    pub total_down: u64,
}
pub static DAYS: State<Vec<Day>> = State::new("statistics.days");
pub static SERVERS: State<Vec<ServerUse>> = State::new("statistics.servers");

/// Seconds of speed history.
const HISTORY: usize = 300;
/// How often the counted bytes are saved.
const SAVE_EVERY: Duration = Duration::from_secs(30);

/// Readings not saved yet, and the speed history.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// Bytes per second, oldest first: (down, up).
    pub speed: VecDeque<(u64, u64)>,
    /// The day the unsaved bytes belong to.
    day: String,
    /// Unsaved bytes per server (`None`: direct and blocked), (up, down).
    unsaved: HashMap<Option<Id>, (u64, u64)>,
}
pub static LIVE: State<Live> = State::new("statistics.live");

impl Live {
    fn unsaved_total(&self) -> (u64, u64) {
        self.unsaved
            .values()
            .fold((0, 0), |(u, d), (a, b)| (u + a, d + b))
    }
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}
fn month_of(day: &str) -> &str {
    day.get(..7).unwrap_or(day)
}

/// Bytes of each open connection at the last reading, to count only what is new.
#[derive(Default)]
struct Counter {
    last: HashMap<String, (u64, u64)>,
    /// The first reading only sets the starting point.
    primed: bool,
}
impl Counter {
    /// Bytes moved since the last reading, per server: (up, down).
    fn read(&mut self, v: &mihomo::Connections) -> HashMap<Option<Id>, (u64, u64)> {
        let mut moved: HashMap<Option<Id>, (u64, u64)> = HashMap::new();
        let mut seen = HashMap::new();
        for c in &v.connections {
            if mihomo::config::internal_listener(&c.metadata.inbound_name) {
                continue;
            }
            let (up, down) = self.last.get(&c.id).copied().unwrap_or_default();
            let server = c.chains.iter().find_map(|n| n.parse::<Id>().ok());
            let e = moved.entry(server).or_default();
            e.0 += c.upload.saturating_sub(up);
            e.1 += c.download.saturating_sub(down);
            seen.insert(c.id.clone(), (c.upload, c.download));
        }
        self.last = seen;
        if !std::mem::replace(&mut self.primed, true) {
            moved.clear();
        }
        moved.retain(|_, (u, d)| *u + *d > 0);
        moved
    }
}

fn setup(p: &mut Setup) -> Result<()> {
    p.collection(&DAYS);
    p.collection(&SERVERS);
    p.cascade(&servers::SERVERS, &SERVERS, |s| s.server);
    p.live(&LIVE, Live::default);
    p.task("count", count);
    p.page_fn(
        PageSpec {
            title: "Statistics",
            icon: Icon::Statistics,
            order: 36,
        },
        page,
    );
    p.fill(&servers::DETAILS, "statistics", 31, details);
    Ok(())
}

/// Read the core's connections, add up the bytes, save them now and then.
async fn count(cx: Cx) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (mut counter, mut skipped, mut saved) = (Counter::default(), 0, Instant::now());
    // The core counts bytes in bursts: the speed is the average of the last
    // five readings over the time they really took.
    let mut recent: VecDeque<(u64, u64, f64)> = VecDeque::new();
    let mut sampled = Instant::now();
    loop {
        tick.tick().await;
        // Nobody watching: the speed graph can wait, the totals stay right.
        skipped += 1;
        if !cx.ui_open() && skipped < 5 {
            continue;
        }
        let secs = std::mem::take(&mut skipped) as u64;
        let moved = match cx.core().connections().await {
            Ok(v) => counter.read(&v),
            Err(_) => {
                counter = Counter::default();
                HashMap::new()
            }
        };
        let day = today();
        let new_day = cx.live(&LIVE).day != day;
        if new_day || saved.elapsed() >= SAVE_EVERY {
            // A failed save keeps the bytes for the next one.
            if let Err(e) = save(&cx).await {
                debug!("Statistics not saved: {e}");
            }
            saved = Instant::now();
        }
        let (up, down) = moved.values().fold((0, 0), |(u, d), (a, b)| (u + a, d + b));
        recent.push_back((down, up, sampled.elapsed().as_secs_f64().max(0.1)));
        sampled = Instant::now();
        if recent.len() > 5 {
            recent.pop_front();
        }
        let (d, u, t) = recent
            .iter()
            .fold((0, 0, 0.), |(d, u, t), x| (d + x.0, u + x.1, t + x.2));
        let rate = ((d as f64 / t) as u64, (u as f64 / t) as u64);
        cx.update_live(&LIVE, |l| {
            l.day = day;
            for (server, (u, d)) in moved {
                let e = l.unsaved.entry(server).or_default();
                e.0 += u;
                e.1 += d;
            }
            for _ in 0..secs {
                l.speed.push_back(rate);
            }
            while l.speed.len() > HISTORY {
                l.speed.pop_front();
            }
        });
    }
}

/// Add the unsaved bytes to the day and the servers.
async fn save(cx: &Cx) -> Result<()> {
    let live = cx.live(&LIVE);
    if live.unsaved.is_empty() {
        return Ok(());
    }
    let (day, unsaved) = (live.day.clone(), live.unsaved.clone());
    let (up, down) = live.unsaved_total();
    cx.write(move |w| {
        w.update(&DAYS, |days| match days.iter_mut().find(|d| d.day == day) {
            Some(d) => (d.up, d.down) = (d.up + up, d.down + down),
            None => days.push(Day {
                id: Id::random(),
                day: day.clone(),
                up,
                down,
            }),
        });
        let known: Vec<Id> = w.get(&servers::SERVERS).iter().map(|s| s.id).collect();
        let month = month_of(&day).to_owned();
        w.update(&SERVERS, |rows| {
            for (server, (u, d)) in &unsaved {
                let Some(server) = server.filter(|s| known.contains(s)) else {
                    continue;
                };
                let i = match rows.iter().position(|r| r.server == server) {
                    Some(i) => i,
                    None => {
                        rows.push(ServerUse {
                            id: Id::random(),
                            server,
                            month: month.clone(),
                            month_up: 0,
                            month_down: 0,
                            total_up: 0,
                            total_down: 0,
                        });
                        rows.len() - 1
                    }
                };
                let r = &mut rows[i];
                if r.month != month {
                    (r.month, r.month_up, r.month_down) = (month.clone(), 0, 0);
                }
                r.month_up += u;
                r.month_down += d;
                r.total_up += u;
                r.total_down += d;
            }
        });
        Ok(())
    })
    .await?;
    // Bytes read meanwhile stay for the next save.
    cx.update_live(&LIVE, |l| {
        for (server, (u, d)) in &live.unsaved {
            if let Some(e) = l.unsaved.get_mut(server) {
                e.0 -= u;
                e.1 -= d;
            }
        }
        l.unsaved.retain(|_, (u, d)| *u + *d > 0);
    });
    Ok(())
}

/// Saved and unsaved bytes of one server: (this month, all time), each (up, down).
fn server_use(snap: &Snapshot, live: &Live, server: Id) -> ((u64, u64), (u64, u64)) {
    let month = month_of(&today()).to_owned();
    let (mut m, mut t) = ((0, 0), (0, 0));
    if let Some(r) = snap.get(&SERVERS).iter().find(|r| r.server == server) {
        if r.month == month {
            m = (r.month_up, r.month_down);
        }
        t = (r.total_up, r.total_down);
    }
    if let Some((u, d)) = live.unsaved.get(&Some(server)) {
        m = (m.0 + u, m.1 + d);
        t = (t.0 + u, t.1 + d);
    }
    (m, t)
}

/// (up, down).
type Bytes = (u64, u64);

fn total((up, down): Bytes) -> String {
    kit::bytes(up + down)
}
fn split((up, down): (u64, u64)) -> String {
    format!("↓ {} · ↑ {}", kit::bytes(down), kit::bytes(up))
}

fn details(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server) {
    let (month, all) = server_use(cx.snapshot(), &cx.live(&LIVE), s.id);
    if all.0 + all.1 > 0 {
        kit::property(ui, "This month", &split(month));
        kit::property(ui, "All time", &split(all));
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let snap = cx.snapshot().clone();
    let live = cx.live(&LIVE);
    let today = today();
    let month = month_of(&today);
    // Saved days plus what is not saved yet (it belongs to today).
    let mut days: Vec<Day> = snap.get(&DAYS).to_vec();
    let (up, down) = live.unsaved_total();
    match days.iter_mut().find(|d| d.day == today) {
        Some(d) => (d.up, d.down) = (d.up + up, d.down + down),
        None => days.push(Day {
            id: Id::random(),
            day: today.clone(),
            up,
            down,
        }),
    }
    let sum = |keep: &dyn Fn(&Day) -> bool| {
        (days.iter().filter(|d| keep(d))).fold((0, 0), |(u, d), x| (u + x.up, d + x.down))
    };
    cx.page_header(ui, "", |_, _| {});
    kit::page_scroll(ui, "statistics", |ui| {
        kit::card(ui, |ui| {
            kit::stats(
                ui,
                &[
                    ("Today", total(sum(&|d| d.day == today)), None),
                    (
                        "This month",
                        total(sum(&|d| month_of(&d.day) == month)),
                        None,
                    ),
                    ("All time", total(sum(&|_| true)), None),
                ],
            );
        });
        let (down, up) = live.speed.back().copied().unwrap_or_default();
        let now = format!("↓ {} · ↑ {}", kit::rate(down), kit::rate(up));
        kit::section_note(ui, "Speed", &now, Some("The last 5 minutes"));
        kit::card(ui, |ui| speed_chart(ui, &live.speed));
        kit::section(ui, "Last 30 days", None);
        kit::card(ui, |ui| days_chart(ui, &days));
        servers_list(ui, &snap, &live);
    });
}

/// Download and upload speed, newest on the right.
fn speed_chart(ui: &mut egui::Ui, speed: &VecDeque<(u64, u64)>) {
    use egui_plot::{Corner, Legend, Line, Plot, PlotPoints};
    let pal = kit::colors(ui);
    let n = speed.len() as f64;
    // x: seconds before now (negative), y: bytes per second.
    let points = |pick: fn(&(u64, u64)) -> u64| -> PlotPoints<'static> {
        (speed.iter().enumerate())
            .map(|(i, s)| [i as f64 - n + 1., pick(s) as f64])
            .collect()
    };
    // Room at the top for the legend.
    let peak = speed.iter().map(|s| s.0.max(s.1)).max().unwrap_or(0);
    let (step, top) = scale(peak + peak / 3);
    let left = -(HISTORY as f64 - 1.);
    still(Plot::new("speed"), step, top)
        .legend(Legend::default().position(Corner::LeftTop))
        .x_grid_spacer(|_| marks(60., 0..=4).map(|m| mark(-m, 60.)).collect())
        .x_axis_formatter(|m, _| match (-m.value).round() as u64 {
            0 => "now".into(),
            s => format!("{} min", s / 60),
        })
        .y_axis_formatter(|m, _| kit::rate(m.value.max(0.) as u64))
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
    use egui_plot::{Bar, BarChart, Plot};
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
        .x_axis_formatter(move |m, _| label(m.value.round().max(0.) as usize))
        .y_axis_formatter(|m, _| kit::bytes(m.value.max(0.) as u64))
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

/// Grid step and top of an axis up to `max` bytes: round sizes (1, 2, 5 …
/// in KB, MB, GB), at most four steps.
fn scale(max: u64) -> (f64, f64) {
    let max = max.max(1024) as f64;
    let mut unit = 1.;
    while max / unit >= 1024. {
        unit *= 1024.;
    }
    let step = [1., 2., 5., 10., 20., 50., 100., 200., 500., 1024.]
        .into_iter()
        .map(|k| k * unit)
        .find(|step| (max / step).ceil() <= 4.)
        .unwrap_or(1024. * unit);
    (step, (max / step).ceil() * step)
}
fn marks(step: f64, n: std::ops::RangeInclusive<u32>) -> impl Iterator<Item = f64> {
    n.map(move |i| f64::from(i) * step)
}
fn mark(value: f64, step_size: f64) -> egui_plot::GridMark {
    egui_plot::GridMark { value, step_size }
}

/// A plot that only shows: no zoom, drag or scroll (the page scrolls); the
/// value axis has round steps up to `top`.
fn still(plot: egui_plot::Plot<'_>, step: f64, top: f64) -> egui_plot::Plot<'_> {
    let count = (top / step).round() as u32;
    plot.height(140.)
        .allow_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .show_crosshair(false)
        .show_grid([false, true])
        .y_axis_min_width(56.)
        .y_grid_spacer(move |_| marks(step, 0..=count).map(|v| mark(v, step)).collect())
}

/// Servers that carried data, the most this month first.
fn servers_list(ui: &mut egui::Ui, snap: &Snapshot, live: &Live) {
    let mut rows: Vec<(String, Bytes, Bytes)> = snap
        .get(&servers::SERVERS)
        .iter()
        .map(|s| {
            let (m, t) = server_use(snap, live, s.id);
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
                Icon::Servers,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn read(v: serde_json::Value) -> mihomo::Connections {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn counts_only_new_bytes_and_skips_internal_traffic() {
        let server = Id::random();
        let conn = |id: &str, inbound: &str, down: u64| {
            serde_json::json!({"id": id, "upload": 1, "download": down,
                "chains": [server.to_string(), "main"], "rule": "Match", "rulePayload": "",
                "metadata": {"inboundName": inbound}})
        };
        let mut c = Counter::default();
        // The first reading is the starting point.
        assert!(
            c.read(&read(
                serde_json::json!({"connections": [conn("a", "local-proxy", 100)]})
            ))
            .is_empty()
        );
        let moved = c.read(&read(serde_json::json!({"connections": [
            conn("a", "local-proxy", 150), conn("b", "local-proxy", 20), conn("t", "checks-0", 9000)]})));
        assert_eq!(moved.get(&Some(server)), Some(&(1, 70)));
        assert_eq!(moved.len(), 1);
    }
}
