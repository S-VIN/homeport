//! # health-checks
//! Measures servers without touching ordinary traffic: latency and HTTP test URLs
//! with expected status through mihomo's delay test (any number in parallel,
//! works for every protocol), and a short download through one of the kernel's
//! isolated check lanes. The rating is an overridable op ([`Rate`]); other
//! plugins can wrap it.
//!
//! Adds to the Servers page: a Check-all button, Rating/Latency columns, details
//! and sort orders; and its own Tests page.
use plugin_servers::{self as servers, Cell, Exclusion, Server};
use rproxy_kernel::{
    kit::{self, theme},
    prelude::*,
    ui::{Column, ListEditor},
};
use rproxy_kernel::mihomo::Delay;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::Semaphore, task::JoinSet, time::timeout};

pub static PLUGIN: Plugin = Plugin {
    id: "health-checks",
    name: "Checks",
    description: "Measure latency, HTTP tests and download speed of every server and rate it.",
    category: categories::SERVERS,
    requires: &["servers"],
    required: false,
    default_enabled: true,
    platforms: Platform::ALL,
    setup,
};

choices!(pub enum Expected {
    NoContent => "204",
    Success => "2xx",
    Below400 => "< 400",
    Below500 => "< 500",
    Below503 => "< 503",
    Any => "Any status",
});
impl Expected {
    pub fn accepts(self, code: u16) -> bool {
        match self {
            Self::NoContent => code == 204,
            Self::Success => (200..300).contains(&code),
            Self::Below400 => code < 400,
            Self::Below500 => code < 500,
            Self::Below503 => code < 503,
            Self::Any => true,
        }
    }
    /// The same range in mihomo's syntax (`None` accepts every status).
    pub fn mihomo(self) -> Option<&'static str> {
        match self {
            Self::NoContent => Some("204"),
            Self::Success => Some("200-299"),
            Self::Below400 => Some("100-399"),
            Self::Below500 => Some("100-499"),
            Self::Below503 => Some("100-502"),
            Self::Any => None,
        }
    }
}

choices!(pub enum Failure {
    Timeout => "Timeout",
    WrongStatus => "Wrong status",
    Failed => "Failed",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "http_tests")]
pub struct Test {
    pub id: Id,
    pub url: String,
    pub expected: Expected,
}
/// Last measurement of one server; `id` is the server id.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "server_health")]
pub struct Health {
    pub id: Id,
    pub ping_ms: Option<u32>,
    pub bytes_per_sec: Option<u64>,
    pub rating: Option<u8>,
    pub checked: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "test_results")]
pub struct Measurement {
    pub id: Id,
    pub server: Id,
    pub test: Id,
    /// HTTP status of older checks; the delay test does not report it.
    pub status: Option<u16>,
    pub passed: bool,
    /// Response time through the server when passed.
    #[serde(default)]
    pub delay_ms: Option<u32>,
    /// Why the test failed, when known.
    #[serde(default)]
    pub failure: Option<Failure>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub check_secs: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self { check_secs: 3600 }
    }
}

pub static TESTS: State<Vec<Test>> = State::new("health-checks.tests");
pub static HEALTH: State<Vec<Health>> = State::new("health-checks.health");
pub static RESULTS: State<Vec<Measurement>> = State::new("health-checks.results");
pub static CONFIG: State<Settings> = State::new("health-checks.settings");
/// True while a full check pass runs (rows keep their order meanwhile).
pub static CHECKING: State<bool> = State::new("health-checks.checking");

/// Servers being measured right now (rows show a spinner).
pub static IN_FLIGHT: State<Vec<Id>> = State::new("health-checks.in-flight");
/// Marks a server as being measured until dropped.
struct Flight(Cx, Id);
impl Flight {
    fn start(cx: &Cx, id: Id) -> Self {
        cx.update_live(&IN_FLIGHT, |l| l.push(id));
        Self(cx.clone(), id)
    }
}
impl Drop for Flight {
    fn drop(&mut self) {
        self.0.update_live(&IN_FLIGHT, |l| l.retain(|x| *x != self.1));
    }
}

pub fn health(snap: &Snapshot, server: Id) -> Option<Health> {
    snap.get(&HEALTH).iter().find(|h| h.id == server).cloned()
}

// ---- ops ------------------------------------------------------------------

/// Check every eligible server (job "Check servers").
pub struct CheckAll;
impl Op for CheckAll {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Check servers").page("servers"))
    }
}
/// Full check of one server; stores and returns its health.
pub struct CheckServer {
    pub id: Id,
}
impl Op for CheckServer {
    type Reply = Option<Health>;
}
/// Fast check without download (liveness of the connected server, candidates
/// before switching). Uses no check lane, so it never waits for a full check. Not stored.
pub struct QuickCheck {
    pub id: Id,
}
#[derive(Clone, Debug)]
pub struct Quick {
    pub health: Health,
    pub results: Vec<Measurement>,
}
impl Quick {
    /// Reachable and at least half of the tests passed.
    pub fn passed(&self) -> bool {
        self.health.ping_ms.is_some() && self.results.iter().filter(|r| r.passed).count() * 2 >= self.results.len()
    }
}
impl Op for QuickCheck {
    type Reply = Option<Quick>;
}
/// Combine measurements into a 0–100 rating. Wrap it to add your own criteria.
#[derive(Clone)]
pub struct Rate {
    pub server: Id,
    pub ping_ms: Option<u32>,
    pub bytes_per_sec: Option<u64>,
    pub results: Vec<Measurement>,
}
impl Op for Rate {
    type Reply = u8;
}
/// Emitted after a full check pass.
#[derive(Clone, Debug)]
pub struct Checked {
    pub servers: Vec<Id>,
}

const SPEED_URL: &str = "https://speed.cloudflare.com/__down?bytes=250000";
const SPEED_LIMIT: u64 = 250_000;
/// Probe used without test URLs, and for latency when no test passes.
pub const PROBE_URL: &str = "https://www.gstatic.com/generate_204";
/// Timeout of one delay test; a failed test is retried once.
pub const PROBE_TIMEOUT_MS: u32 = 3000;
/// Latency at which the latency part of the rating reaches zero.
const SLOW_MS: f64 = 1000.;
/// Servers measured at the same time by Check all (downloads share the check lanes).
const PARALLEL: usize = 24;
pub const FULL_SPEED: u64 = 125_000;

pub fn default_rating(ping: Option<u32>, speed: Option<u64>, tests: &[Measurement]) -> u8 {
    let mut weight = 30.;
    let mut score = 30. * (speed.unwrap_or(0) as f64 / FULL_SPEED as f64).min(1.);
    if let Some(ms) = ping {
        weight += 30.;
        score += 30. * (1. - ms as f64 / SLOW_MS).max(0.);
    }
    if !tests.is_empty() {
        weight += 40.;
        score += 40. * tests.iter().filter(|t| t.passed).count() as f64 / tests.len() as f64;
    }
    (100. * score / weight).round() as u8
}

/// The tests a check runs: the configured ones, or the default probe.
pub fn probes(snap: &Snapshot) -> Vec<Test> {
    let tests = snap.get(&TESTS);
    if tests.is_empty() {
        vec![Test { id: Id::NIL, url: PROBE_URL.into(), expected: Expected::NoContent }]
    } else {
        tests.to_vec()
    }
}
/// The URL mihomo's own failover tests use: the first test URL, or the default probe.
pub fn probe_url(snap: &Snapshot) -> String {
    snap.get(&TESTS).first().map_or_else(|| PROBE_URL.into(), |t| t.url.clone())
}
async fn delay_test(cx: &Cx, proxy: &str, t: &Test, attempts: u8) -> Delay {
    let mut last = Delay::Failed;
    for _ in 0..attempts {
        match cx.core().probe(proxy, &t.url, PROBE_TIMEOUT_MS, t.expected.mihomo()).await {
            // A wrong status is an answer: retrying does not change it.
            Ok(d @ (Delay::Passed(_) | Delay::WrongStatus)) => return d,
            Ok(d) => last = d,
            Err(_) => return Delay::Failed,
        }
    }
    last
}
/// Run every test through mihomo's delay test in parallel. Latency is the best
/// response time of a passed test, with a second sample of the first one.
async fn measure(cx: &Cx, server: Id, tests: &[Test]) -> (Option<u32>, Vec<Measurement>) {
    let proxy = server.to_string();
    let mut set = JoinSet::new();
    for (i, t) in tests.iter().cloned().enumerate() {
        let (cx, proxy) = (cx.clone(), proxy.clone());
        set.spawn(async move { (i, delay_test(&cx, &proxy, &t, 2).await) });
    }
    let mut outcomes = vec![Delay::Failed; tests.len()];
    while let Some(r) = set.join_next().await {
        if let Ok((i, d)) = r {
            outcomes[i] = d;
        }
    }
    let mut best = outcomes.iter().filter_map(|d| d.ms()).min();
    if let (Some(first), Some(_)) = (tests.first(), outcomes.first().and_then(|d| d.ms())) {
        if let Some(ms) = delay_test(cx, &proxy, first, 1).await.ms() {
            best = best.map(|b| b.min(ms));
        }
    }
    let rows = tests
        .iter()
        .zip(outcomes)
        .map(|(t, d)| Measurement {
            id: server.child(t.id.0.as_bytes()),
            server,
            test: t.id,
            status: None,
            passed: d.ms().is_some(),
            delay_ms: d.ms(),
            failure: match d {
                Delay::Passed(_) => None,
                Delay::Timeout => Some(Failure::Timeout),
                Delay::WrongStatus => Some(Failure::WrongStatus),
                Delay::Failed => Some(Failure::Failed),
            },
        })
        .collect();
    (best, rows)
}
async fn download_speed(client: &reqwest::Client) -> Option<u64> {
    let start = Instant::now();
    let mut bytes = 0;
    let read = async {
        let mut r = client.get(SPEED_URL).send().await.ok()?.error_for_status().ok()?;
        while let Some(chunk) = r.chunk().await.ok()? {
            bytes = (bytes + chunk.len() as u64).min(SPEED_LIMIT);
            if bytes == SPEED_LIMIT {
                break;
            }
        }
        Some(())
    };
    let _ = timeout(Duration::from_secs(2), read).await;
    (bytes > 0).then(|| (bytes as f64 / start.elapsed().as_secs_f64().max(0.001)) as u64)
}
fn server(snap: &Snapshot, id: Id) -> Option<Server> {
    snap.get(&servers::SERVERS).iter().find(|s| s.id == id).cloned()
}
fn skipped(snap: &Snapshot, s: &Server) -> bool {
    // Name filters exclude for good; servers that only failed a check are re-checked.
    servers::exclusion(snap, s).is_some_and(|e| e.disconnect)
}

async fn check_all(cx: &Cx) -> Result<()> {
    let snap = cx.snapshot();
    // The connected server first, then the best ones: their results matter most.
    let selected = *snap.get(&servers::SELECTED);
    let mut ids: Vec<(Id, i64)> = snap
        .get(&servers::SERVERS)
        .iter()
        .filter(|s| !skipped(&snap, s))
        .map(|s| (s.id, if Some(s.id) == selected { i64::MAX } else { health(&snap, s.id).and_then(|h| h.rating).map_or(-1, i64::from) }))
        .collect();
    ids.sort_by_key(|(_, key)| -key);
    let total = Some(ids.len() as u64);
    cx.progress(0, total);
    // Delay tests run in parallel; downloads wait for a free check lane in this order.
    let slots = Arc::new(Semaphore::new(PARALLEL));
    let mut set = JoinSet::new();
    for (id, _) in ids {
        let (cx, slots) = (cx.clone(), slots.clone());
        set.spawn(async move {
            let _slot = slots.acquire_owned().await;
            (id, cx.call(CheckServer { id }).await)
        });
    }
    let (mut checked, mut failures, mut done) = (Vec::new(), 0, 0);
    while let Some(next) = cx.or_cancel(set.join_next()).await? {
        match next {
            Ok((id, Ok(Some(_)))) => checked.push(id),
            Ok((_, Ok(None))) => {}
            Ok((_, Err(e))) if e.cancelled() => return Err(e),
            _ => failures += 1,
        }
        done += 1;
        cx.progress(done, total);
    }
    cx.emit(Checked { servers: checked });
    if failures > 0 {
        bail!("{} could not complete.", kit::plural(failures, "server check", "server checks"));
    }
    Ok(())
}

fn setup(p: &mut Setup) -> Result<()> {
    p.live(&IN_FLIGHT, Vec::new);
    p.list(&TESTS).validate(|_, t, _, others| {
        let url = url::Url::parse(t.url.trim())
            .ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some())
            .ok_or_else(|| Error::msg("Enter an HTTP(S) test URL."))?;
        t.url = url.to_string();
        if others.iter().any(|o| o.url == t.url) {
            bail!("Test URL already exists.");
        }
        Ok(())
    });
    p.collection(&HEALTH);
    p.collection(&RESULTS);
    p.editable_setting(&CONFIG);
    p.memory(&CHECKING, || false);
    p.cascade(&servers::SERVERS, &HEALTH, |h| h.id);
    p.cascade(&servers::SERVERS, &RESULTS, |m| m.server);
    p.cascade(&TESTS, &RESULTS, |m| m.test);
    // Editing a test invalidates its old results.
    p.on(|cx: Cx, e: Saved<Test>| async move {
        cx.write(|w| {
            w.update(&RESULTS, |r| r.retain(|m| m.test != e.item.id));
            Ok(())
        })
        .await
    });

    p.handle(|_: Cx, r: Rate| async move { Ok(default_rating(r.ping_ms, r.bytes_per_sec, &r.results)) });

    p.handle(|cx: Cx, _: CheckAll| async move {
        let set = |on: bool| {
            let cx = cx.clone();
            async move {
                cx.write(move |w| {
                    w.set(&CHECKING, on);
                    Ok(())
                })
                .await
            }
        };
        set(true).await?;
        // A panic becomes an error, so the flag is always reset.
        use futures_util::FutureExt;
        let result = std::panic::AssertUnwindSafe(check_all(&cx)).catch_unwind().await.unwrap_or_else(|_| Err(Error::msg("Server check crashed.")));
        set(false).await?;
        result
    });
    p.handle(|cx: Cx, op: CheckServer| async move {
        let snap = cx.snapshot();
        let Some(s) = server(&snap, op.id).filter(|s| !skipped(&snap, s)) else { return Ok(None) };
        let _flight = Flight::start(&cx, s.id);
        let (ping, results) = cx.or_cancel(measure(&cx, s.id, &probes(&snap))).await?;
        // Only configured tests are stored; the default probe only measures latency.
        let results: Vec<Measurement> = results.into_iter().filter(|m| m.test != Id::NIL).collect();
        let mut speed = None;
        if ping.is_some() {
            let lease = cx.core().lease(Target::Proxy(s.id.to_string())).await?;
            let measured = cx.or_cancel(download_speed(lease.http())).await;
            lease.release().await;
            speed = measured?;
        }
        let rating = match ping {
            None => 0,
            Some(_) => cx.call(Rate { server: s.id, ping_ms: ping, bytes_per_sec: speed, results: results.clone() }).await?,
        };
        let h = Health { id: s.id, ping_ms: ping, bytes_per_sec: speed, rating: Some(rating), checked: Some(now()) };
        let stored = h.clone();
        cx.write(move |w| {
            if !w.get(&servers::SERVERS).iter().any(|x| x.id == stored.id) {
                return Ok(());
            }
            let tests: Vec<Id> = w.get(&TESTS).iter().map(|t| t.id).collect();
            w.update(&HEALTH, |l| {
                l.retain(|x| x.id != stored.id);
                l.push(stored.clone());
            });
            w.update(&RESULTS, |l| {
                l.retain(|m| m.server != stored.id);
                l.extend(results.into_iter().filter(|m| tests.contains(&m.test)));
            });
            Ok(())
        })
        .await?;
        Ok(Some(h))
    });
    p.handle(|cx: Cx, op: QuickCheck| async move {
        let snap = cx.snapshot();
        let Some(s) = server(&snap, op.id).filter(|s| !skipped(&snap, s)) else { return Ok(None) };
        let (ping, results) = cx.or_cancel(measure(&cx, s.id, &probes(&snap))).await?;
        let prior = health(&snap, s.id);
        let mut h = Health { id: s.id, ping_ms: ping, bytes_per_sec: prior.and_then(|p| p.bytes_per_sec), rating: Some(0), checked: Some(now()) };
        if ping.is_some() {
            h.rating = Some(cx.call(Rate { server: s.id, ping_ms: ping, bytes_per_sec: h.bytes_per_sec, results: results.clone() }).await?);
        }
        Ok(Some(Quick { health: h, results }))
    });
    p.every("check", |s| Some(Duration::from_secs(s.get(&CONFIG).check_secs.into())), || CheckAll);

    p.hook(&servers::EXCLUDE, |snap, s| {
        let h = health(snap, s.id)?;
        unreachable(&h).then(|| Exclusion { reason: "Unreachable on last check".into(), disconnect: false })
    });
    p.hook(&servers::SORT_KEYS, |_, _| {
        vec![
            ("Rating", |snap: &Snapshot, s: &Server| -(health(snap, s.id).and_then(|h| h.rating).map_or(-1, i64::from))),
            ("Latency", |snap: &Snapshot, s: &Server| health(snap, s.id).and_then(|h| h.ping_ms).map_or(i64::MAX, i64::from)),
            ("Download speed", |snap: &Snapshot, s: &Server| -(health(snap, s.id).and_then(|h| h.bytes_per_sec).map_or(-1, |b| b as i64))),
        ]
    });
    p.hook(&servers::FREEZE, |snap, _| *snap.get(&CHECKING));
    // The tray turns red when the connected server failed its last check.
    p.hook(&rproxy_kernel::shell::TRAY, |snap, _| {
        let id = (*snap.get(&servers::SELECTED))?;
        (health(snap, id)?.rating == Some(0)).then_some(rproxy_kernel::shell::TrayLink::Failed)
    });
    ui(p);
    Ok(())
}

/// No test answered through the server on its last check.
pub fn unreachable(h: &Health) -> bool {
    h.checked.is_some() && h.ping_ms.is_none() && h.rating == Some(0)
}

pub fn summary(snap: &Snapshot, s: &Server) -> String {
    let tests = snap.get(&TESTS);
    let results: Vec<Measurement> = snap.get(&RESULTS).iter().filter(|r| r.server == s.id).cloned().collect();
    match health(snap, s.id) {
        Some(h) if unreachable(&h) => "Unreachable on last check".into(),
        _ if results.is_empty() => "Not checked yet".into(),
        _ => format!("{} of {} passed", results.iter().filter(|r| r.passed).count(), tests.len()),
    }
}

/// "Passed · 120 ms", "Failed · Timeout", "Failed · HTTP 404" (older checks) …
fn result_text(r: &Measurement) -> String {
    let detail = match (r.delay_ms, r.failure, r.status) {
        (Some(ms), ..) => format!("{ms} ms"),
        (None, Some(Failure::Failed), _) | (None, None, None) => return if r.passed { "Passed" } else { "Failed" }.into(),
        (None, Some(f), _) => f.to_string(),
        (None, None, Some(code)) => format!("HTTP {code}"),
    };
    format!("{} · {detail}", if r.passed { "Passed" } else { "Failed" })
}

fn speed(bytes: u64) -> String {
    let kib = bytes as f64 / 1024.;
    if kib >= 1024. {
        format!("{:.1} MiB/s", kib / 1024.)
    } else {
        format!("{kib:.0} KiB/s")
    }
}

fn host(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| url.to_owned())
}

fn ui(p: &mut Setup) {
    p.fill(&servers::TOOLBAR, "check-all", 20, |ui, cx, _| {
        let ok = cx.core_status().running();
        cx.job_button(ui, Icon::Checks, "Check all", 100., ok, || CheckAll);
    });
    p.fill(&servers::COLUMNS, "rating", 10, |ui, cx, c: &Cell| {
        kit::cell_right(ui, 48., |ui| match &c.server {
            None => {
                kit::caption(ui, "Rating").on_hover_text("0–100 from latency, tests and speed");
            }
            Some(s) if cx.live(&IN_FLIGHT).contains(&s.id) => {
                kit::spinner(ui).on_hover_text("Checking…");
            }
            Some(s) => {
                let rating = health(cx.snapshot(), s.id).and_then(|h| h.rating);
                let text = rating.map_or_else(|| "—".into(), |n| n.to_string());
                ui.colored_label(kit::rating_color(ui, rating), text).on_hover_text(summary(cx.snapshot(), s));
            }
        });
    });
    p.fill(&servers::COLUMNS, "latency", 20, |ui, cx, c: &Cell| {
        if !c.wide {
            return;
        }
        kit::cell_right(ui, 60., |ui| match &c.server {
            None => {
                kit::caption(ui, "Latency").on_hover_text("Best response time of a test URL through the server");
            }
            Some(s) => {
                let ping = health(cx.snapshot(), s.id).and_then(|h| h.ping_ms);
                let pal = kit::colors(ui);
                ui.colored_label(if ping.is_some() { pal.foreground } else { pal.faint }, ping.map_or_else(|| "—".into(), |n| format!("{n} ms")));
            }
        });
    });
    p.fill(&servers::DETAILS, "health", 10, |ui, cx, s: &Server| {
        let snap = cx.snapshot().clone();
        let pal = kit::colors(ui);
        let h = health(&snap, s.id);
        if cx.live(&IN_FLIGHT).contains(&s.id) {
            kit::property_with(ui, "Check", |ui| {
                kit::spinner(ui);
                ui.label(egui::RichText::new("Checking…").color(pal.muted));
            });
        }
        let ping = h.as_ref().and_then(|h| h.ping_ms);
        kit::property(ui, "Latency", &ping.map_or_else(|| "—".into(), |ms| format!("{ms} ms")));
        kit::property(ui, "Download", &h.as_ref().and_then(|h| h.bytes_per_sec).map_or_else(|| "—".into(), speed));
        kit::property(ui, "Last check", &h.as_ref().and_then(|h| h.checked).map_or_else(|| "Never".into(), kit::age));
        for t in snap.get(&TESTS).iter() {
            let r = snap.get(&RESULTS).iter().find(|r| r.server == s.id && r.test == t.id).cloned();
            kit::property_with(ui, &host(&t.url), |ui| {
                ui.spacing_mut().item_spacing.x = 4.;
                let (icon, text, color) = match r {
                    None => (Icon::Info, "Not checked".to_owned(), pal.muted),
                    Some(r) => (
                        if r.passed { Icon::Connected } else { Icon::Error },
                        result_text(&r),
                        if r.passed { pal.success } else { pal.danger },
                    ),
                };
                kit::icon(ui, icon, 14., color);
                ui.colored_label(color, text).on_hover_text(&t.url);
            });
        }
    });
    p.fill(&servers::CONNECTION, "health", 10, |ui, cx, s: &Server| {
        let snap = cx.snapshot().clone();
        let h = health(&snap, s.id);
        let rating = h.as_ref().and_then(|h| h.rating);
        let tests = snap.get(&TESTS).len();
        let passed = snap.get(&RESULTS).iter().filter(|r| r.server == s.id && r.passed).count();
        let mut items = vec![
            ("Latency", h.as_ref().and_then(|h| h.ping_ms).map_or_else(|| "—".into(), |ms| format!("{ms} ms")), None),
            ("Rating", rating.map_or_else(|| "—".into(), |r| r.to_string()), Some(kit::rating_color(ui, rating))),
        ];
        if tests > 0 {
            items.push(("Tests", format!("{passed} of {tests}"), None));
        }
        kit::stats(ui, &items);
        ui.add_space(4.);
    });
    p.page_fn(PageSpec { id: "tests", title: "Checks", icon: Icon::Checks, order: 40 }, |ui, cx| {
        let n = cx.get(&TESTS).len();
        let counter = if n == 0 { String::new() } else { kit::plural(n, "test URL", "test URLs") };
        kit::page_header(ui, "Checks", &counter, |_| {});
        kit::helper(ui, "Each check requests these URLs through the server (HEAD, redirects not followed). A test passes when the response status matches. The first URL also tells whether the connected server is alive.");
        ListEditor::new(&TESTS)
            .unordered()
            .add_form("Add test URL", || Test { id: Id::random(), url: String::new(), expected: Expected::NoContent }, |ui, t, width| {
                let w = (width - 120. - theme::HORIZONTAL_GAP).max(60.);
                let r = kit::input(ui, "url", &mut t.url, "https://example.org/", w, false);
                kit::choice(ui, "expected", &mut t.expected, Expected::ALL, 120., "Expected HTTP status");
                kit::submitted(ui, &r) && !t.url.trim().is_empty()
            })
            .column(Column::edit("URL", "https://example.org/", |t: &mut Test| &mut t.url))
            .column(Column::choice("Expected status", 120., |t: &mut Test| &mut t.expected, Expected::ALL))
            .empty(Icon::Checks, "No test URLs", "Without tests a check measures only latency and speed. Add URLs your traffic depends on.")
            .show(ui, cx);
    });
    p.fill(&SETTINGS_SECTIONS, "interval", 0, |ui, cx, _| {
        let mut secs = cx.get(&CONFIG).check_secs;
        kit::settings_row(ui, "Check servers", "Measure every server on this schedule", |ui| {
            if kit::interval(ui, "check", &mut secs) {
                cx.update("check-interval", &CONFIG, move |s| s.check_secs = secs);
            }
        });
    });
}
