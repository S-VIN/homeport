//! What the core moves now: open connections, speed and the bytes not saved yet.
use homeport_kernel::prelude::*;
use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

/// Seconds of speed history.
pub const HISTORY: usize = 300;

/// One open connection.
#[derive(Clone, Debug, PartialEq)]
pub struct Conn {
    pub id: String,
    /// Host name or address with port.
    pub target: String,
    pub network: String,
    /// The app that opened it, when the core looks apps up (app rules exist).
    pub process: String,
    pub rule: String,
    /// The outbound names from mihomo (server ids, DIRECT, exit …), outermost first.
    pub chain: Vec<String>,
    pub up: u64,
    pub down: u64,
    pub started: Instant,
}

/// Live traffic; checks, speed tests and the app's own downloads are not counted.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Traffic {
    pub conns: Vec<Conn>,
    pub online: bool,
    /// Bytes per second, oldest first: (down, up).
    pub speed: VecDeque<(u64, u64)>,
    /// Bytes through the core since it started, as the core counts them.
    pub up_total: u64,
    pub down_total: u64,
    /// The day the unsaved bytes belong to.
    pub(crate) day: String,
    /// Unsaved bytes per server (`None`: direct and blocked), (up, down).
    pub(crate) unsaved: HashMap<Option<Id>, (u64, u64)>,
    /// Bytes moving now or in the last few seconds, so bursts don't make
    /// the chip and the panel blink.
    active: bool,
}

impl Traffic {
    /// Traffic now or in the last few seconds.
    pub fn active(&self) -> bool {
        self.active
    }
    /// The latest speed: (down, up).
    pub fn rate(&self) -> (u64, u64) {
        self.speed.back().copied().unwrap_or_default()
    }
    pub(crate) fn unsaved_total(&self) -> (u64, u64) {
        self.unsaved
            .values()
            .fold((0, 0), |(u, d), (a, b)| (u + a, d + b))
    }
}

/// What the poller remembers between readings. Kept outside [`Traffic`]: a new
/// time alone must not repaint the windows.
#[derive(Default)]
pub(crate) struct Reader {
    /// Bytes of each open connection at the last reading, to count only what is new.
    last: HashMap<String, (u64, u64)>,
    /// The first reading only sets the starting point.
    primed: bool,
    /// The core counts bytes in bursts: the speed is the average of the last
    /// five readings over the time they really took.
    recent: VecDeque<(u64, u64, f64)>,
    sampled: Option<Instant>,
    busy: Option<Instant>,
}

impl Reader {
    /// Take one reading of the core; `secs` is how many seconds it stands for.
    pub(crate) fn read(
        &mut self,
        t: &mut Traffic,
        v: &mihomo::Connections,
        secs: usize,
        day: String,
    ) {
        let now = Instant::now();
        let old: HashMap<&str, Instant> =
            t.conns.iter().map(|c| (c.id.as_str(), c.started)).collect();
        let mut moved: HashMap<Option<Id>, (u64, u64)> = HashMap::new();
        let mut seen = HashMap::new();
        let conns: Vec<Conn> = v
            .connections
            .iter()
            .filter(|c| !mihomo::config::internal_listener(&c.metadata.inbound_name))
            .map(|c| {
                let (up, down) = self.last.get(&c.id).copied().unwrap_or_default();
                let e = moved
                    .entry(plugin_servers::server_in(&c.chains))
                    .or_default();
                e.0 += c.upload.saturating_sub(up);
                e.1 += c.download.saturating_sub(down);
                seen.insert(c.id.clone(), (c.upload, c.download));
                let m = &c.metadata;
                let host = [&m.host, &m.destination_ip]
                    .into_iter()
                    .find(|h| !h.is_empty())
                    .map_or("?", |h| h.as_str());
                Conn {
                    target: match m.destination_port {
                        Some(p) => format!("{host}:{p}"),
                        None => host.to_owned(),
                    },
                    network: m.network.to_uppercase(),
                    process: m.process.clone(),
                    rule: match (c.rule.as_str(), c.rule_payload.as_str()) {
                        (r, "") => r.to_owned(),
                        (r, p) => format!("{r} {p}"),
                    },
                    up: c.upload,
                    down: c.download,
                    started: old.get(c.id.as_str()).copied().unwrap_or(now),
                    chain: c.chains.clone(),
                    id: c.id.clone(),
                }
            })
            .collect();
        self.last = seen;
        if !std::mem::replace(&mut self.primed, true) {
            moved.clear();
        }
        moved.retain(|_, (u, d)| *u + *d > 0);
        let (up, down) = moved.values().fold((0, 0), |(u, d), (a, b)| (u + a, d + b));
        let took = self
            .sampled
            .map_or(1., |at| now.duration_since(at).as_secs_f64());
        self.recent.push_back((down, up, took.max(0.1)));
        self.sampled = Some(now);
        if self.recent.len() > 5 {
            self.recent.pop_front();
        }
        let (d, u, s) =
            (self.recent.iter()).fold((0, 0, 0.), |(d, u, s), x| (d + x.0, u + x.1, s + x.2));
        let rate = ((d as f64 / s) as u64, (u as f64 / s) as u64);

        t.conns = conns;
        t.conns.sort_by_key(|c| std::cmp::Reverse(c.started));
        t.online = true;
        t.day = day;
        (t.up_total, t.down_total) = (v.upload_total, v.download_total);
        for (server, (u, d)) in moved {
            let e = t.unsaved.entry(server).or_default();
            e.0 += u;
            e.1 += d;
        }
        t.speed.extend(std::iter::repeat_n(rate, secs.max(1)));
        while t.speed.len() > HISTORY {
            t.speed.pop_front();
        }
        // Idle open connections are not traffic: the chip would show 0 B/s.
        if rate.0 + rate.1 > 0 {
            self.busy = Some(now);
        }
        t.active = self
            .busy
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(10));
    }

    /// The core is not answering. A core that comes back restarted counts
    /// from zero: every connection after this is new, none is skipped.
    pub(crate) fn offline(&mut self, t: &mut Traffic, secs: usize) {
        (self.recent, self.sampled) = (VecDeque::new(), None);
        t.online = false;
        t.active = false;
        t.speed.extend(std::iter::repeat_n((0, 0), secs.max(1)));
        while t.speed.len() > HISTORY {
            t.speed.pop_front();
        }
    }
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
                "metadata": {"inboundName": inbound, "host": "example.org", "destinationPort": "443", "network": "tcp"}})
        };
        let (mut r, mut t) = (Reader::default(), Traffic::default());
        // The first reading is the starting point.
        r.read(
            &mut t,
            &read(serde_json::json!({"connections": [conn("a", "local-proxy", 100)]})),
            1,
            "d".into(),
        );
        assert!(t.unsaved.is_empty());
        r.read(&mut t, &read(serde_json::json!({"connections": [
            conn("a", "local-proxy", 150), conn("b", "local-proxy", 20), conn("t", "checks-0", 9000)]})), 1, "d".into());
        assert_eq!(t.unsaved.get(&Some(server)), Some(&(1, 70)));
        assert_eq!(t.unsaved.len(), 1);
        assert_eq!(t.conns.len(), 2, "internal connections are not listed");
        assert_eq!(t.conns[0].target, "example.org:443");
        assert!(t.active());
    }
}
