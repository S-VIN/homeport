//! # health-checks
//! Measures servers without touching ordinary traffic: latency and HTTP test URLs
//! with expected status through mihomo's delay test (any number in parallel,
//! works for every protocol), and a short download through one of the kernel's
//! isolated check lanes. The rating is an overridable op ([`Rate`]); other
//! plugins can wrap it.
//!
//! Adds to the Servers page: a Check-all button, Rating/Latency columns, details
//! and sort orders; and its own Tests page.
use plugin_servers::{self as servers, Exclusion, Server};
// The capability this plugin implements; its ops and event are the contract's.
pub use rproxy_contracts::health::{
    CAPABILITY, CheckAll, Checked, FULL_SPEED, PROBE_URL, QuickCheck, Report,
};
use rproxy_kernel::mihomo::Delay;
use rproxy_kernel::{
    kit,
    prelude::*,
    ui::{Column, ListEditor},
};
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
    provides: &[CAPABILITY],
    setup,
    ..Plugin::DEFAULT
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
pub fn health(snap: &Snapshot, server: Id) -> Option<Health> {
    snap.get(&HEALTH).iter().find(|h| h.id == server).cloned()
}

// ---- ops ------------------------------------------------------------------

/// Full check of one server; stores and returns its health.
pub struct CheckServer {
    pub id: Id,
}
impl Op for CheckServer {
    type Reply = Option<Health>;
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
const SPEED_URL: &str = "https://speed.cloudflare.com/__down?bytes=250000";
const SPEED_LIMIT: u64 = 250_000;
/// Timeout of one delay test; a failed test is retried once.
pub const PROBE_TIMEOUT_MS: u32 = 3000;
/// Latency at which the latency part of the rating reaches zero.
const SLOW_MS: f64 = 1000.;
/// Servers measured at the same time by Check all (downloads share the check lanes).
const PARALLEL: usize = 24;

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
        vec![Test {
            id: Id::NIL,
            url: PROBE_URL.into(),
            expected: Expected::NoContent,
        }]
    } else {
        tests.to_vec()
    }
}
/// The URL mihomo's own failover tests use: the first test URL, or the default probe.
pub fn probe_url(snap: &Snapshot) -> String {
    snap.get(&TESTS)
        .first()
        .map_or_else(|| PROBE_URL.into(), |t| t.url.clone())
}
async fn delay_test(cx: &Cx, proxy: &str, t: &Test, attempts: u8) -> Delay {
    let mut last = Delay::Failed;
    for _ in 0..attempts {
        match cx
            .core()
            .probe(proxy, &t.url, PROBE_TIMEOUT_MS, t.expected.mihomo())
            .await
        {
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
    if let (Some(first), Some(_)) = (tests.first(), outcomes.first().and_then(|d| d.ms()))
        && let Some(ms) = delay_test(cx, &proxy, first, 1).await.ms()
    {
        best = best.map(|b| b.min(ms));
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
        let mut r = client
            .get(SPEED_URL)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?;
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
    snap.get(&servers::SERVERS)
        .iter()
        .find(|s| s.id == id)
        .cloned()
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
        .map(|s| {
            (
                s.id,
                if Some(s.id) == selected {
                    i64::MAX
                } else {
                    health(&snap, s.id)
                        .and_then(|h| h.rating)
                        .map_or(-1, i64::from)
                },
            )
        })
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
        bail!(
            "{} could not complete.",
            kit::plural(failures, "server check", "server checks")
        );
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
    p.live(&CHECKING, || false);
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

    p.handle(
        |_: Cx, r: Rate| async move { Ok(default_rating(r.ping_ms, r.bytes_per_sec, &r.results)) },
    );

    p.handle(check_all_servers);
    p.handle(check_server);
    p.handle(quick_check);
    p.every(
        "check",
        |s| Some(Duration::from_secs(s.get(&CONFIG).check_secs.into())),
        || CheckAll,
    );

    p.hook(&servers::EXCLUDE, |snap, s| {
        let h = health(snap, s.id)?;
        unreachable(&h).then(|| Exclusion {
            reason: "Unreachable on last check".into(),
            disconnect: false,
        })
    });
    p.hook(&servers::SORT_KEYS, |_, _| {
        vec![
            ("Rating", |snap: &Snapshot, s: &Server| {
                -(health(snap, s.id)
                    .and_then(|h| h.rating)
                    .map_or(-1, i64::from))
            }),
            ("Latency", |snap: &Snapshot, s: &Server| {
                health(snap, s.id)
                    .and_then(|h| h.ping_ms)
                    .map_or(i64::MAX, i64::from)
            }),
            ("Download speed", |snap: &Snapshot, s: &Server| {
                -(health(snap, s.id)
                    .and_then(|h| h.bytes_per_sec)
                    .map_or(-1, |b| b as i64))
            }),
        ]
    });
    p.hook(&servers::FREEZE, |snap, _| *snap.live(&CHECKING));
    p.hook(&rproxy_contracts::health::REPORT, |snap, id| {
        report(snap, *id)
    });
    p.hook(&rproxy_contracts::health::CHECK_URL, |snap, _| {
        Some(probe_url(snap))
    });
    // The tray turns red when the connected server failed its last check.
    p.hook(&rproxy_kernel::shell::TRAY, |snap, _| {
        let id = (*snap.get(&servers::SELECTED))?;
        (health(snap, id)?.rating == Some(0)).then_some(rproxy_kernel::shell::TrayLink::Failed)
    });
    ui(p);
    Ok(())
}

async fn check_all_servers(cx: Cx, _: CheckAll) -> Result<()> {
    /// Clears the flag however the pass ends: done, failed, cancelled or crashed.
    struct Pass(Cx);
    impl Drop for Pass {
        fn drop(&mut self) {
            self.0.set_live(&CHECKING, false);
        }
    }
    cx.set_live(&CHECKING, true);
    let _pass = Pass(cx.clone());
    check_all(&cx).await
}

async fn check_server(cx: Cx, op: CheckServer) -> Result<Option<Health>> {
    let snap = cx.snapshot();
    let Some(s) = server(&snap, op.id).filter(|s| !skipped(&snap, s)) else {
        return Ok(None);
    };
    let _measuring = cx.mark_live(&IN_FLIGHT, s.id);
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
        Some(_) => {
            cx.call(Rate {
                server: s.id,
                ping_ms: ping,
                bytes_per_sec: speed,
                results: results.clone(),
            })
            .await?
        }
    };
    let h = Health {
        id: s.id,
        ping_ms: ping,
        bytes_per_sec: speed,
        rating: Some(rating),
        checked: Some(now()),
    };
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
}

/// Fast check without download (liveness of the connected server, candidates
/// before switching). Uses no check lane, so it never waits for a full check. Not stored.
async fn quick_check(cx: Cx, op: QuickCheck) -> Result<Option<Report>> {
    let snap = cx.snapshot();
    let Some(s) = server(&snap, op.id).filter(|s| !skipped(&snap, s)) else {
        return Ok(None);
    };
    let (ping, results) = cx.or_cancel(measure(&cx, s.id, &probes(&snap))).await?;
    let bytes_per_sec = health(&snap, s.id).and_then(|p| p.bytes_per_sec);
    let mut rating = Some(0);
    if ping.is_some() {
        rating = Some(
            cx.call(Rate {
                server: s.id,
                ping_ms: ping,
                bytes_per_sec,
                results: results.clone(),
            })
            .await?,
        );
    }
    let passed = results.iter().filter(|r| r.passed).count();
    Ok(Some(Report {
        ping_ms: ping,
        bytes_per_sec,
        rating,
        checked: Some(now()),
        passed,
        tests: results.len(),
    }))
}

/// The stored check of a server, as the health contract reports it.
fn report(snap: &Snapshot, server: Id) -> Option<Report> {
    let h = health(snap, server)?;
    let results = snap.get(&RESULTS);
    let mine = results.iter().filter(|r| r.server == server);
    let (passed, tests) = mine.fold((0, 0), |(p, t), r| (p + usize::from(r.passed), t + 1));
    Some(Report {
        ping_ms: h.ping_ms,
        bytes_per_sec: h.bytes_per_sec,
        rating: h.rating,
        checked: h.checked,
        passed,
        tests,
    })
}

/// No test answered through the server on its last check.
pub fn unreachable(h: &Health) -> bool {
    h.checked.is_some() && h.ping_ms.is_none() && h.rating == Some(0)
}

pub fn summary(snap: &Snapshot, s: &Server) -> String {
    let tests = snap.get(&TESTS);
    let results: Vec<Measurement> = snap
        .get(&RESULTS)
        .iter()
        .filter(|r| r.server == s.id)
        .cloned()
        .collect();
    match health(snap, s.id) {
        Some(h) if unreachable(&h) => "Unreachable on last check".into(),
        _ if results.is_empty() => "Not checked yet".into(),
        _ => format!(
            "{} of {} passed",
            results.iter().filter(|r| r.passed).count(),
            tests.len()
        ),
    }
}

/// "Passed · 120 ms", "Failed · Timeout", "Failed · HTTP 404" (older checks) …
/// "Passed · 120 ms", "Failed · Timeout", "Failed · status not 2xx".
fn result_text(r: &Measurement, expected: Expected) -> String {
    let detail = match (r.delay_ms, r.failure, r.status) {
        (Some(ms), ..) => format!("{ms} ms"),
        (None, Some(Failure::WrongStatus), _) => format!("status not {expected}"),
        (None, Some(Failure::Failed), _) | (None, None, None) => {
            return if r.passed { "Passed" } else { "Failed" }.into();
        }
        (None, Some(f), _) => f.to_string(),
        (None, None, Some(code)) => format!("HTTP {code}"),
    };
    format!("{} · {detail}", if r.passed { "Passed" } else { "Failed" })
}

fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| url.to_owned())
}

fn ui(p: &mut Setup) {
    p.fill(&servers::TOOLBAR, "check-all", 20, |ui, cx, _| {
        let ok = cx.core_status().running();
        cx.job_button(ui, Icon::Checks, "Check all", ok, || CheckAll);
    });
    let rating = servers::Column {
        id: "rating",
        order: 10,
        title: "Rating",
        help: "0–100 from latency, tests and speed",
        secondary: false,
    };
    servers::column(p, rating, |ui, cx, s| {
        if cx.live(&IN_FLIGHT).contains(&s.id) {
            kit::spinner(ui).on_hover_text("Checking…");
            return;
        }
        let rating = health(cx.snapshot(), s.id).and_then(|h| h.rating);
        let text = rating.map_or_else(|| "—".into(), |n| n.to_string());
        ui.colored_label(kit::rating_color(ui, rating), text)
            .on_hover_text(summary(cx.snapshot(), s));
    });
    let latency = servers::Column {
        id: "latency",
        order: 20,
        title: "Latency",
        help: "Best response time of a test URL through the server",
        secondary: true,
    };
    servers::column(p, latency, |ui, cx, s| {
        let ping = health(cx.snapshot(), s.id).and_then(|h| h.ping_ms);
        let pal = kit::colors(ui);
        let ink = if ping.is_some() {
            pal.foreground
        } else {
            pal.faint
        };
        ui.colored_label(ink, ping.map_or_else(|| "—".into(), |n| format!("{n} ms")));
    });
    p.fill(&servers::DETAILS, "health", 10, details);
    p.fill(&servers::CONNECTION, "health", 10, connection_stats);
    p.page_fn(
        PageSpec {
            id: "tests",
            title: "Checks",
            icon: Icon::Checks,
            order: 40,
        },
        page,
    );
    p.fill(&SETTINGS_SECTIONS, "interval", 0, |ui, cx, _| {
        cx.setting(
            ui,
            "Check servers",
            &CONFIG,
            |s| &mut s.check_secs,
            |ui, v| kit::interval(ui, "check", v),
        );
    });
}

/// Health of one server in its expanded row, with "Check now".
fn details(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server) {
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
    kit::property(
        ui,
        "Latency",
        &ping.map_or_else(|| "—".into(), |ms| format!("{ms} ms")),
    );
    kit::property(
        ui,
        "Download",
        &h.as_ref()
            .and_then(|h| h.bytes_per_sec)
            .map_or_else(|| "—".into(), kit::rate),
    );
    let checked = h
        .as_ref()
        .and_then(|h| h.checked)
        .map_or_else(|| "Never".into(), kit::age);
    kit::property_with(ui, "Last check", |ui| {
        ui.label(egui::RichText::new(checked).color(pal.foreground));
        // A check of just this server, with its outcome right here.
        let key = ("check", s.id);
        match cx.request::<Option<Health>>(key) {
            Request::Pending => {
                kit::spinner(ui);
            }
            Request::Done(Ok(None)) => {
                kit::hint(ui, "Skipped: excluded");
            }
            Request::Done(Err(e)) => {
                kit::tone_text(ui, e, kit::Tone::Danger);
            }
            Request::Idle | Request::Done(Ok(Some(_))) => {
                if kit::Button::ghost("Check now")
                    .icon(Icon::Checks)
                    .show(ui)
                    .clicked()
                {
                    cx.submit(key, CheckServer { id: s.id });
                }
            }
        }
    });
    for t in snap.get(&TESTS).iter() {
        let r = snap
            .get(&RESULTS)
            .iter()
            .find(|r| r.server == s.id && r.test == t.id)
            .cloned();
        kit::property_with(ui, &host(&t.url), |ui| {
            ui.spacing_mut().item_spacing.x = 4.;
            let (icon, text, color) = match r {
                None => (Icon::Info, "Not checked".to_owned(), pal.muted),
                Some(r) => (
                    if r.passed {
                        Icon::Connected
                    } else {
                        Icon::Error
                    },
                    result_text(&r, t.expected),
                    if r.passed { pal.success } else { pal.danger },
                ),
            };
            kit::icon(ui, icon, 14., color);
            ui.colored_label(color, text).on_hover_text(&t.url);
        });
    }
}

/// Latency, rating and tests of the connected server.
fn connection_stats(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server) {
    let snap = cx.snapshot().clone();
    let h = health(&snap, s.id);
    let rating = h.as_ref().and_then(|h| h.rating);
    let tests = snap.get(&TESTS).len();
    let passed = snap
        .get(&RESULTS)
        .iter()
        .filter(|r| r.server == s.id && r.passed)
        .count();
    let mut items = vec![
        (
            "Latency",
            h.as_ref()
                .and_then(|h| h.ping_ms)
                .map_or_else(|| "—".into(), |ms| format!("{ms} ms")),
            None,
        ),
        (
            "Rating",
            rating.map_or_else(|| "—".into(), |r| r.to_string()),
            Some(kit::rating_color(ui, rating)),
        ),
    ];
    if tests > 0 {
        items.push(("Tests", format!("{passed} of {tests}"), None));
    }
    kit::stats(ui, &items);
    ui.add_space(4.);
}

/// The test URLs.
fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let n = cx.get(&TESTS).len();
    let counter = kit::count(n, "test URL", "test URLs");
    kit::page_header(ui, "Checks", &counter, |_| {});

    ListEditor::new(&TESTS)
        .unordered()
        .add_form(
            "Add test URL",
            || Test {
                id: Id::random(),
                url: String::new(),
                expected: Expected::NoContent,
            },
            |ui, t, width| {
                kit::Fields::new(width)
                    .input("url", &mut t.url, "https://example.org/")
                    .help(
                        "HEAD requests; redirects are not followed. Only a matching status passes.",
                    )
                    .choice("expected", &mut t.expected, Expected::ALL)
                    .show(ui)
                    && !t.url.trim().is_empty()
            },
        )
        .column(Column::edit(
            "URL",
            "https://example.org/",
            |t: &mut Test| &mut t.url,
        ))
        .column(Column::choice(
            "Expected status",
            120.,
            |t: &mut Test| &mut t.expected,
            Expected::ALL,
        ))
        .empty(Icon::Checks, "No test URLs")
        .show(ui, cx);
}
