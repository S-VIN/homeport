//! Server tests and the rating. Servers measures latency (reachability) and,
//! like every other test, runs the checks of [`CHECKS`]: its own download
//! speed and whatever other plugins add (say, test URLs). The rating is the
//! weighted mean of latency and the checks' values.
//!
//! A plugin adds a check with [`add_check`]: an async function that gets a
//! [`Probe`]: how to reach the server for tests, which a plugin may
//! change with [`TEST_PATH`] (say, through a proxy chain).
use crate::{Exclusion, SELECTED, SERVERS, Server, server};
use homeport_kernel::{
    core::Lease,
    mihomo::{Delay, PROBE_URL},
    prelude::*,
    sql_text,
};
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::Semaphore, task::JoinSet, time::timeout};

mod view;

/// A download speed that counts as fast (bytes per second, 1 Mbit/s).
pub const FULL_SPEED: u64 = 125_000;
/// Timeout of one delay test; a failed test is retried once.
pub const PROBE_TIMEOUT_MS: u32 = 3000;
const SPEED_URL: &str = "https://speed.cloudflare.com/__down?bytes=250000";
const SPEED_LIMIT: u64 = 250_000;
/// Latency at which the latency part of the rating reaches zero.
const SLOW_MS: f64 = 1000.;
/// Weight of latency in the rating (checks state their own).
const LATENCY_WEIGHT: f64 = 30.;
/// Servers measured at the same time by Test all (downloads share the check lanes).
const PARALLEL: usize = 24;

// ---- the extension point ---------------------------------------------------

/// What a check found: `value` from 0 (failed) to 1 (perfect), and the same
/// in words for the server's details ("3 of 4 passed", "1.2 MB/s").
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Reading {
    pub value: f64,
    pub text: String,
    /// A response time through the server, when the check measured one
    /// (quick checks only): the latency is the best of all.
    #[serde(default)]
    pub delay_ms: Option<u32>,
}
/// How a check counts.
#[derive(Clone, Copy, Debug)]
pub struct CheckSpec {
    /// Unique among checks; a later plugin with the same id replaces the
    /// check (servers' own: `speed`).
    pub id: &'static str,
    /// Its line in the server's details ("Speed", "Test URLs").
    pub title: &'static str,
    /// Its share of the rating; latency weighs 30.
    pub weight: f64,
    /// Runs with the latency test, also in quick checks (Auto select, before
    /// switching); a quick check below 0.5 means the server does not work.
    /// Otherwise it runs only once the server answered, and quick checks
    /// reuse its last reading.
    pub quick: bool,
}
/// How to reach the server under test.
#[derive(Clone, Debug)]
pub struct Probe {
    pub server: Id,
    /// [`probe_url`] when the test began.
    pub url: String,
    /// The mihomo proxy tests go through: the server itself, or what
    /// [`TEST_PATH`] says.
    pub proxy: String,
    /// A quick check: nothing is stored.
    pub quick: bool,
}
impl Probe {
    fn of(snap: &Snapshot, s: &Server, quick: bool) -> Self {
        Self {
            server: s.id,
            url: probe_url(snap),
            proxy: snap
                .first(&TEST_PATH, s)
                .unwrap_or_else(|| s.id.to_string()),
            quick,
        }
    }
    /// mihomo's delay test of `url` (`expected`: status range in mihomo's
    /// syntax), up to `attempts` times; a wrong status is final.
    pub async fn delay(&self, cx: &Cx, url: &str, expected: Option<&str>, attempts: u8) -> Delay {
        let mut last = Delay::Failed;
        for _ in 0..attempts {
            match cx
                .core()
                .probe(&self.proxy, url, PROBE_TIMEOUT_MS, expected)
                .await
            {
                Ok(d @ (Delay::Passed(_) | Delay::WrongStatus)) => return d,
                Ok(d) => last = d,
                Err(_) => return Delay::Failed,
            }
        }
        last
    }
    /// An isolated route through the server, for real requests (downloads).
    pub async fn lease(&self, cx: &Cx) -> Result<Lease> {
        cx.core().lease(Target::Proxy(self.proxy.clone())).await
    }
}
/// The op behind a check: one type per check function `F`, handled by the
/// plugin that added it (so the check writes that plugin's state).
struct RunCheck<F>(Probe, PhantomData<fn() -> F>);
impl<F: 'static> Op for RunCheck<F> {
    type Reply = Option<Reading>;
}
type Run = Arc<
    dyn Fn(&Cx, Probe) -> Pin<Box<dyn Future<Output = Result<Option<Reading>>> + Send>>
        + Send
        + Sync,
>;
/// A check as [`CHECKS`] lists it.
#[derive(Clone)]
pub struct Check {
    pub spec: CheckSpec,
    run: Run,
}
/// Every check of a test, from each plugin that has one now.
pub static CHECKS: Hook<(), Option<Check>> = Hook::new("servers.checks");
/// The URL that shows a server works: the latency test and mihomo's failover
/// open it (default: [`PROBE_URL`]); say, the first of the user's test URLs.
pub static PROBE: Hook<(), Option<String>> = Hook::new("servers.probe");
pub fn probe_url(snap: &Snapshot) -> String {
    snap.first(&PROBE, &()).unwrap_or_else(|| PROBE_URL.into())
}
/// The proxy tests of a server go through (default: the server). One plugin
/// decides, e.g. a chain exit tests every server as its entry.
pub static TEST_PATH: Hook<Server, Option<String>> = Hook::exclusive("servers.test-path");

/// Rate servers with `check` too, while `applies` (say, while there is
/// something to test). `check` gets how to reach the server and answers its
/// reading (`None`: nothing to say this time, it does not count).
pub fn add_check<F, Fut>(p: &mut Setup, spec: CheckSpec, applies: fn(&Snapshot) -> bool, check: F)
where
    F: Fn(Cx, Probe) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Option<Reading>>> + Send + 'static,
{
    p.handle(move |cx, RunCheck::<F>(probe, _)| check(cx, probe));
    let run: Run = Arc::new(|cx, probe| Box::pin(cx.call(RunCheck::<F>(probe, PhantomData))));
    p.hook(&CHECKS, move |snap, _| {
        applies(snap).then(|| Check {
            spec,
            run: run.clone(),
        })
    });
}
/// The checks of a test now, one per id (a later plugin replaces an earlier one).
pub fn checks(snap: &Snapshot) -> Vec<Check> {
    let mut all: Vec<Check> = Vec::new();
    for c in snap.hooks(&CHECKS, &()).into_iter().flatten() {
        all.retain(|o| o.spec.id != c.spec.id);
        all.push(c);
    }
    all
}

// ---- results -----------------------------------------------------------------

/// The last reading of each check, by check id.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Scores(pub BTreeMap<String, Reading>);
impl fmt::Display for Scores {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&serde_json::to_string(&self.0).map_err(|_| fmt::Error)?)
    }
}
impl FromStr for Scores {
    type Err = serde_json::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}
sql_text!(Scores);

/// Last test of one server; `id` is the server id.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "servers_tests")]
pub struct TestResult {
    pub id: Id,
    pub ping_ms: Option<u32>,
    pub checked: Option<u64>,
    pub scores: Scores,
    /// Why nothing answered ("Timed out"), when it did not.
    pub failure: Option<String>,
}
impl TestResult {
    /// Nothing answered through the server on its last test.
    pub fn unreachable(&self) -> bool {
        self.checked.is_some() && self.ping_ms.is_none()
    }
    /// "Failed the last test: timed out" for an unreachable server.
    pub fn failed_text(&self) -> String {
        match &self.failure {
            Some(why) => format!("Failed the last test: {}", why.to_lowercase()),
            None => "Failed the last test".into(),
        }
    }
}
/// What a test of a server found, for policies.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub ping_ms: Option<u32>,
    /// 0–100; `None` while untested.
    pub rating: Option<u8>,
    pub checked: Option<u64>,
    /// `(check, value, quick)` of every check of the test.
    pub values: Vec<(&'static str, f64, bool)>,
    /// Why nothing answered, when it did not.
    pub failure: Option<String>,
}
impl Report {
    /// The stored test `h`, read by `checks`.
    pub(crate) fn of(checks: &[Check], h: &TestResult) -> Self {
        Self {
            ping_ms: h.ping_ms,
            rating: h.checked.map(|_| rating(checks, h.ping_ms, &h.scores)),
            checked: h.checked,
            values: values(checks, &h.scores),
            failure: h.failure.clone(),
        }
    }
    /// Reachable and no quick check failed.
    pub fn works(&self) -> bool {
        self.ping_ms.is_some() && self.values.iter().all(|(_, v, quick)| !quick || *v >= 0.5)
    }
    /// Every check perfect (all tests passed, full speed).
    pub fn ideal(&self) -> bool {
        self.values.iter().all(|(_, v, _)| *v >= 1.)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TestSettings {
    pub check_secs: u32,
}
impl Default for TestSettings {
    fn default() -> Self {
        Self { check_secs: 3600 }
    }
}

pub static RESULTS: State<Vec<TestResult>> = State::new("servers.results");
pub static TEST_SETTINGS: State<TestSettings> = State::new("servers.test-settings");
/// True while Test all runs (rows keep their order meanwhile).
pub static TESTING: State<bool> = State::new("servers.testing");
/// Servers being tested right now (rows show a spinner).
pub static IN_FLIGHT: State<Vec<Id>> = State::new("servers.in-flight");

pub fn last_test(snap: &Snapshot, server: Id) -> Option<TestResult> {
    snap.get(&RESULTS).iter().find(|h| h.id == server).cloned()
}
/// The stored test of a server, read by the checks of now: a check turned
/// off since no longer counts in the rating.
pub fn report(snap: &Snapshot, server: Id) -> Option<Report> {
    let h = last_test(snap, server)?;
    Some(Report::of(&checks(snap), &h))
}
/// The rating of a server by its last test; `None` while untested.
pub fn rating_of(snap: &Snapshot, server: Id) -> Option<u8> {
    report(snap, server)?.rating
}
fn values(checks: &[Check], scores: &Scores) -> Vec<(&'static str, f64, bool)> {
    (checks.iter())
        .filter_map(|c| Some((c.spec.id, scores.0.get(c.spec.id)?.value, c.spec.quick)))
        .collect()
}

// ---- ops ---------------------------------------------------------------------

/// Test every eligible server (job "Test servers").
pub struct TestAll;
impl Op for TestAll {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Test servers").page(&crate::PLUGIN))
    }
}
/// Test the servers without a result (job "Test new servers"), so new ones
/// show a rating right away. One waiting run covers any number of loads.
struct TestNew;
impl Op for TestNew {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(
            JobSpec::new("Test new servers")
                .queued()
                .page(&crate::PLUGIN),
        )
    }
}
/// Full test of one server; stores its health and returns the report.
pub struct TestServer {
    pub id: Id,
}
impl Op for TestServer {
    type Reply = Option<Report>;
}
/// Fast test without the slow checks, not stored (say, a candidate right
/// before switching to it).
pub struct QuickTest {
    pub id: Id,
}
impl Op for QuickTest {
    type Reply = Option<Report>;
}
/// Emitted after Test all.
#[derive(Clone, Debug)]
pub struct Tested {
    pub servers: Vec<Id>,
}

// ---- measuring -----------------------------------------------------------------

fn skipped(snap: &Snapshot, s: &Server) -> bool {
    crate::exclusion(snap, s).is_some_and(|e| e.permanent)
}
/// Latency through the probe: the best of two samples of the default probe
/// URL (any answer counts: this is reachability); `None` when nothing answers.
/// The best of two delay tests, or why the first one failed.
async fn latency(cx: &Cx, probe: &Probe) -> std::result::Result<u32, &'static str> {
    let first = match probe.delay(cx, &probe.url, None, 2).await {
        Delay::Passed(ms) => ms,
        Delay::Timeout => return Err("Timed out"),
        Delay::WrongStatus => return Err("Unexpected answer"),
        Delay::Failed => return Err("Connection failed"),
    };
    let second = probe.delay(cx, &probe.url, None, 1).await.ms();
    Ok(second.map_or(first, |s| s.min(first)))
}
/// Run `checks` through `probe` at the same time; a failed one does not count.
async fn run(cx: &Cx, checks: &[Check], probe: &Probe) -> Result<Scores> {
    let mut set = JoinSet::new();
    for c in checks {
        let (id, reading) = (c.spec.id, (c.run)(cx, probe.clone()));
        set.spawn(async move { (id, reading.await) });
    }
    let mut scores = BTreeMap::new();
    while let Some(done) = cx.or_cancel(set.join_next()).await? {
        match done {
            Ok((id, Ok(Some(r)))) => {
                scores.insert(id.to_owned(), r);
            }
            Ok((_, Err(e))) if e.cancelled() => return Err(e),
            Ok((id, Err(e))) => debug!("Check {id} failed: {e}"),
            _ => {}
        }
    }
    Ok(Scores(scores))
}
/// What the latency test and the quick checks, run at the same time, found.
struct Answer {
    /// The best response time of all; `None`: nothing answered.
    ping: Option<u32>,
    /// Why, when nothing answered.
    failure: Option<String>,
    scores: Scores,
}
async fn answer(cx: &Cx, checks: &[Check], probe: &Probe) -> Result<Answer> {
    let quick: Vec<Check> = checks.iter().filter(|c| c.spec.quick).cloned().collect();
    let (own, scores) = tokio::join!(cx.or_cancel(latency(cx, probe)), run(cx, &quick, probe));
    let (own, scores) = (own?, scores?);
    let ping = (own.ok().into_iter())
        .chain(scores.0.values().filter_map(|r| r.delay_ms))
        .min();
    let failure = own.err().filter(|_| ping.is_none()).map(str::to_owned);
    Ok(Answer {
        ping,
        failure,
        scores,
    })
}

/// 0–100: the weighted mean of latency and the checks' values; 0 when
/// nothing answered.
fn rating(checks: &[Check], ping: Option<u32>, scores: &Scores) -> u8 {
    let Some(ms) = ping else { return 0 };
    let mut weight = LATENCY_WEIGHT;
    let mut score = LATENCY_WEIGHT * (1. - ms as f64 / SLOW_MS).max(0.);
    for c in checks {
        if let Some(r) = scores.0.get(c.spec.id) {
            weight += c.spec.weight;
            score += c.spec.weight * r.value.clamp(0., 1.);
        }
    }
    (100. * score / weight).round() as u8
}

async fn check_server(cx: Cx, op: TestServer) -> Result<Option<Report>> {
    let snap = cx.snapshot();
    let Some(s) = server(&snap, op.id).filter(|s| !skipped(&snap, s)) else {
        return Ok(None);
    };
    let _testing = cx.mark_live(&IN_FLIGHT, s.id);
    let (probe, checks) = (Probe::of(&snap, &s, false), checks(&snap));
    let Answer {
        ping,
        failure,
        mut scores,
    } = answer(&cx, &checks, &probe).await?;
    if ping.is_some() {
        let slow: Vec<Check> = checks.iter().filter(|c| !c.spec.quick).cloned().collect();
        scores.0.extend(run(&cx, &slow, &probe).await?.0);
    }
    let h = TestResult {
        id: s.id,
        ping_ms: ping,
        checked: Some(now()),
        scores,
        failure,
    };
    let report = Report::of(&checks, &h);
    // One write per server, so results show as they come: composing the
    // mihomo config is in memory, and an unchanged one is not reapplied.
    cx.write(move |w| {
        if w.get(&SERVERS).iter().any(|x| x.id == h.id) {
            w.update(&RESULTS, |l| {
                l.retain(|x| x.id != h.id);
                l.push(h);
            });
        }
        Ok(())
    })
    .await?;
    Ok(Some(report))
}

/// Latency and the quick checks; the others keep their last reading. Uses no
/// check lane, so it never waits for Test all.
async fn quick_test(cx: Cx, op: QuickTest) -> Result<Option<Report>> {
    let snap = cx.snapshot();
    let Some(s) = server(&snap, op.id).filter(|s| !skipped(&snap, s)) else {
        return Ok(None);
    };
    let (probe, checks) = (Probe::of(&snap, &s, true), checks(&snap));
    let Answer {
        ping,
        failure,
        scores: fresh,
    } = answer(&cx, &checks, &probe).await?;
    let mut scores = last_test(&snap, s.id).map(|h| h.scores).unwrap_or_default();
    for c in checks.iter().filter(|c| c.spec.quick) {
        scores.0.remove(c.spec.id);
    }
    scores.0.extend(fresh.0);
    Ok(Some(Report {
        ping_ms: ping,
        rating: Some(rating(&checks, ping, &scores)),
        checked: Some(now()),
        values: values(&checks, &scores),
        failure,
    }))
}

/// Servers' own check: a short download through the server.
async fn speed(cx: Cx, probe: Probe) -> Result<Option<Reading>> {
    let lease = probe.lease(&cx).await?;
    let measured = cx.or_cancel(download_speed(lease.http())).await;
    lease.release().await;
    let bytes = measured?;
    Ok(Some(Reading {
        value: (bytes.unwrap_or(0) as f64 / FULL_SPEED as f64).min(1.),
        text: bytes.map_or_else(|| "No data".into(), kit::rate),
        delay_ms: None,
    }))
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

/// Test every eligible server, or only those without a result yet.
async fn check_all(cx: &Cx, untested: bool) -> Result<()> {
    let snap = cx.snapshot();
    // The connected server first, then the best ones: their results matter most.
    let selected = *snap.get(&SELECTED);
    let mut ids: Vec<(Id, i64)> = snap
        .get(&SERVERS)
        .iter()
        .filter(|s| !skipped(&snap, s) && !(untested && last_test(&snap, s.id).is_some()))
        .map(|s| {
            let key = match rating_of(&snap, s.id) {
                _ if Some(s.id) == selected => i64::MAX,
                rating => rating.map_or(-1, i64::from),
            };
            (s.id, key)
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
            (id, cx.call(TestServer { id }).await)
        });
    }
    let (mut checked, mut failures, mut done, mut answered) = (Vec::new(), 0, 0, 0);
    while let Some(next) = cx.or_cancel(set.join_next()).await? {
        match next {
            Ok((id, Ok(Some(h)))) => {
                answered += usize::from(h.rating.is_some_and(|r| r > 0));
                checked.push(id);
            }
            Ok((_, Ok(None))) => {}
            Ok((_, Err(e))) if e.cancelled() => return Err(e),
            _ => failures += 1,
        }
        done += 1;
        cx.progress(done, total);
    }
    if !checked.is_empty() {
        // Tasks says what the pass found, not only that it ran.
        cx.summary(match answered {
            0 => "No server answered".to_owned(),
            n => format!("{} of {} answered", n, checked.len()),
        });
        info!(
            "Tested {}: {} answered",
            kit::plural(checked.len(), "server", "servers"),
            if answered == 0 {
                "none".to_owned()
            } else {
                answered.to_string()
            }
        );
    }
    cx.emit(Tested { servers: checked });
    if failures > 0 {
        bail!(
            "{} could not complete.",
            kit::plural(failures, "server test", "server tests")
        );
    }
    Ok(())
}

async fn check_all_servers(cx: Cx, _: TestAll) -> Result<()> {
    /// Clears the flag however the pass ends: done, failed, cancelled or crashed.
    struct Pass(Cx);
    impl Drop for Pass {
        fn drop(&mut self) {
            self.0.set_live(&TESTING, false);
        }
    }
    cx.set_live(&TESTING, true);
    let result = {
        let _pass = Pass(cx.clone());
        check_all(&cx, false).await
    };
    // Servers loaded while it ran: the loads saw a pass running and left them.
    let snap = cx.snapshot();
    let untested = |s: &Server| !skipped(&snap, s) && last_test(&snap, s.id).is_none();
    // Not after Stop: that would test what the user just stopped.
    let stopped = result.as_ref().is_err_and(|e| e.cancelled());
    if !stopped && snap.get(&SERVERS).iter().any(untested) {
        let _ = cx.spawn(TestNew);
    }
    result
}

pub(crate) fn register(p: &mut Setup) {
    p.collection(&RESULTS);
    p.editable_setting(&TEST_SETTINGS);
    p.live(&TESTING, || false);
    p.live(&IN_FLIGHT, Vec::new);
    p.cascade(&SERVERS, &RESULTS, |h| h.id);
    p.handle(check_all_servers);
    p.handle(|cx: Cx, _: TestNew| async move { check_all(&cx, true).await });
    p.handle(check_server);
    p.handle(quick_test);
    add_check(
        p,
        CheckSpec {
            id: "speed",
            title: "Speed",
            weight: 30.,
            quick: false,
        },
        |_| true,
        speed,
    );
    // New servers get a rating right away; a running Test all tests them when it ends.
    p.on(|cx: Cx, e: crate::ServersChanged| async move {
        if !e.added.is_empty() && !*cx.live(&TESTING) {
            cx.spawn(TestNew)?;
        }
        Ok(())
    });
    p.every(
        "check",
        // Nothing to test without servers.
        |s| {
            (!s.get(&SERVERS).is_empty())
                .then(|| Duration::from_secs(s.get(&TEST_SETTINGS).check_secs.into()))
        },
        || TestAll,
    );
    p.hook(&crate::EXCLUDE, |snap, s| {
        let h = last_test(snap, s.id).filter(|h| h.unreachable())?;
        Some(Exclusion {
            reason: h.failed_text(),
            group: "Failed",
            permanent: false,
        })
    });
    view::register(p);
}
