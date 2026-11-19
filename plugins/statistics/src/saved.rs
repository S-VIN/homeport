//! Bytes per day and per server, saved across restarts.
use crate::{TRAFFIC, traffic::Traffic};
use homeport_kernel::prelude::*;
use plugin_servers as servers;

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
pub static USAGE: State<Vec<ServerUse>> = State::new("statistics.usage");

/// (up, down).
pub type Bytes = (u64, u64);

pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}
pub fn month_of(day: &str) -> &str {
    day.get(..7).unwrap_or(day)
}

/// Add the unsaved bytes to the day and the servers.
pub async fn save(cx: &Cx) -> Result<()> {
    let live = cx.live(&TRAFFIC);
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
        w.update(&USAGE, |rows| {
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
    cx.update_live(&TRAFFIC, |l| {
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

/// Saved days plus what is not saved yet (it belongs to today).
pub fn days(snap: &Snapshot, live: &Traffic) -> Vec<Day> {
    let today = today();
    let mut days: Vec<Day> = snap.get(&DAYS).to_vec();
    let (up, down) = live.unsaved_total();
    match days.iter_mut().find(|d| d.day == today) {
        Some(d) => (d.up, d.down) = (d.up + up, d.down + down),
        None => days.push(Day {
            id: Id::random(),
            day: today,
            up,
            down,
        }),
    }
    days
}

/// Saved and unsaved bytes of one server: (this month, all time).
pub fn server_use(snap: &Snapshot, live: &Traffic, server: Id) -> (Bytes, Bytes) {
    let month = month_of(&today()).to_owned();
    let (mut m, mut t) = ((0, 0), (0, 0));
    if let Some(r) = snap.get(&USAGE).iter().find(|r| r.server == server) {
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
