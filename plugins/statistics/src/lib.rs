//! # statistics
//! Traffic through the core: open connections and the speed now, and how much
//! data went through today, this month and all time, per day and per server,
//! saved across restarts. One page with two tabs: Overview and Connections.
//!
//! A task reads the core's connections every second (every 5 s with no window
//! open) into a live state and counts the bytes each one moved since the last
//! reading; checks, speed tests and the app's own downloads are not counted.
//! The sums are saved every 5 s and when the day changes.
use homeport_kernel::{kit, prelude::*, shell::panel_title_page};
use plugin_servers::{self as servers, Server};
use std::time::{Duration, Instant};

mod connections;
mod overview;
mod saved;
mod traffic;

pub use saved::{DAYS, Day, ServerUse, USAGE};
pub use traffic::{Conn, Traffic};

pub static PLUGIN: Plugin = Plugin {
    id: "statistics",
    name: "Statistics",
    description: "Open connections, the speed now and how much data went through: per day and per server.",
    requires: &[&plugin_servers::PLUGIN],
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

pub static TRAFFIC: State<Traffic> = State::new("statistics.traffic");

/// How often the counted bytes are saved.
const SAVE_EVERY: Duration = Duration::from_secs(5);

/// Close one connection, or all of them with `None`.
pub struct Close(pub Option<String>);
impl Op for Close {
    type Reply = ();
}

fn setup(p: &mut Setup) -> Result<()> {
    p.collection(&DAYS);
    p.collection(&USAGE);
    p.cascade(&servers::SERVERS, &USAGE, |s| s.server);
    p.live(&TRAFFIC, Traffic::default);
    p.task("poll", poll);
    p.handle(close);
    p.page::<StatisticsPage>(PageSpec {
        title: "Statistics",
        icon: Icon::ChartBar,
        order: 35,
    });
    p.fill(&STATUS, "traffic", 40, chip);
    p.fill(&PANELS, "traffic", 30, panel);
    p.fill(&servers::DETAILS, "statistics", 30, details);
    Ok(())
}

/// Read the core's connections, add up the bytes, save them now and then.
async fn poll(cx: Cx) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (mut reader, mut skipped, mut saved) = (traffic::Reader::default(), 0, Instant::now());
    loop {
        tick.tick().await;
        // Nobody watching (a server without an open browser tab): the speed
        // graph can wait, the totals stay right.
        skipped += 1;
        if !cx.ui_open() && skipped < 5 {
            continue;
        }
        let secs = std::mem::take(&mut skipped);
        let day = saved::today();
        if cx.live(&TRAFFIC).day != day || saved.elapsed() >= SAVE_EVERY {
            // A failed save keeps the bytes for the next one.
            if let Err(e) = saved::save(&cx).await {
                debug!("Statistics not saved: {e}");
            }
            saved = Instant::now();
        }
        match cx.core().connections().await {
            Ok(v) => cx.update_live(&TRAFFIC, |t| reader.read(t, &v, secs, day)),
            Err(_) => cx.update_live(&TRAFFIC, |t| reader.offline(t, secs)),
        }
    }
}

async fn close(cx: Cx, Close(id): Close) -> Result<()> {
    match id {
        Some(id) => cx.core().close_connection(&id).await,
        None => {
            cx.core().close_all().await?;
            info!("Closed all connections");
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Tab {
    #[default]
    Overview,
    Connections,
}

/// The tab and the connection filter, kept per window.
#[derive(Default)]
struct StatisticsPage {
    tab: Tab,
    filter: String,
}
impl Page for StatisticsPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let snap = cx.snapshot().clone();
        let t = cx.live(&TRAFFIC).clone();
        let counter = match (self.tab, t.online) {
            (Tab::Overview, _) => String::new(),
            (Tab::Connections, true) => format!("{} open", t.conns.len()),
            (Tab::Connections, false) => "Core offline".into(),
        };
        let tab = &mut self.tab;
        cx.page_header(ui, &counter, |ui, _| {
            kit::segmented(
                ui,
                tab,
                &[
                    (Tab::Overview, "Overview"),
                    (Tab::Connections, "Connections"),
                ],
            );
        });
        match self.tab {
            Tab::Overview => kit::page_scroll(ui, "statistics", |ui| {
                overview::show(ui, &snap, &t);
            }),
            Tab::Connections => connections::show(ui, cx, &t.conns, t.online, &mut self.filter),
        }
    }
}

fn chip(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let t = cx.live(&TRAFFIC);
    if !t.active() {
        return;
    }
    let (down, up) = t.rate();
    let pal = kit::colors(ui);
    let tip = format!("Download {} · upload {}", kit::rate(down), kit::rate(up));
    cx.status_chip(
        ui,
        kit::Lead::Icon(Icon::Activity, pal.muted),
        &format!("↓ {}", kit::rate(down)),
        &tip,
    );
}

fn panel(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let t = cx.live(&TRAFFIC);
    // The Statistics page shows the same numbers.
    if !t.online || !t.active() || cx.on_page(&PLUGIN) {
        return;
    }
    let t = t.clone();
    panel_title_page(ui, cx, "Statistics", &PLUGIN);
    let open = t.conns.len();
    let (down, up) = t.rate();
    kit::property(ui, "Connections", &format!("{open} open"));
    kit::property(ui, "Download", &kit::rate(down));
    kit::property(ui, "Upload", &kit::rate(up));
    kit::property(
        ui,
        "This session",
        &format!(
            "↓ {} · ↑ {}",
            kit::bytes(t.down_total),
            kit::bytes(t.up_total)
        ),
    );
}

fn details(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server) {
    let (month, all) = saved::server_use(cx.snapshot(), &cx.live(&TRAFFIC), s.id);
    if all.0 + all.1 > 0 {
        kit::property(ui, "This month", &overview::split(month));
        kit::property(ui, "All time", &overview::split(all));
    }
}
