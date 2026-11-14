//! # auto-select
//! A server-selection strategy (exclusive category: one strategy at a time).
//! Chooses by health, rating and subscription priority, re-checks candidates
//! (in parallel) before switching, and limits flapping: smoothing, dwell time, a
//! daily switch budget and escalating quarantine for servers that failed.
//!
//! The data plane does not wait for decisions: while Auto is on, the main group
//! is a mihomo `fallback` group `[selected, backups…]` ([`servers::FAILOVER`]),
//! so traffic moves to a live backup within seconds. Every 10 s the watch adopts
//! such a failover, probes the connected server through mihomo's delay test and
//! switches after two failed probes.
//!
//! Integration: wraps `servers::Select` (a manual choice turns Auto off), reacts
//! to `ServersChanged`, `health::Checked` and subscription order changes.
use plugin_servers::{self as servers, Select, Server};
use plugin_subscriptions as subscriptions;
use rproxy_contracts::health::{self, QuickCheck, Report};
use rproxy_kernel::{kit, prelude::*};
use std::time::Duration;
use tokio::task::JoinSet;

pub static PLUGIN: Plugin = Plugin {
    id: "auto-select",
    name: "Auto select",
    description: "Connect to the best healthy server and switch when it fails or a clearly better one appears.",
    category: categories::SERVER_SELECTION,
    requires: &["servers", health::CAPABILITY, "subscriptions"],
    setup,
    ..Plugin::DEFAULT
};

/// How often the connected server is probed.
const WATCH_SECS: u64 = 10;
/// How often a passing probe is also compared with the server's rating (degradation).
const CHECK_INTERVAL: u64 = 60;
/// Consecutive failed probes that count as a failure.
const FAILURES: u8 = 2;
/// Candidates probed at the same time.
const BATCH: usize = 8;
/// Backup servers in the fallback group.
const BACKUPS: usize = 4;
/// mihomo re-tests the fallback members on this interval (and after failed dials).
const FAILOVER_SECS: u32 = 30;
const DAY: u64 = 86_400;
const MARGIN: f64 = 10.;

/// Settings and watch state; times are Unix seconds and survive restarts.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Auto {
    pub enabled: bool,
    pub last_switch: Option<u64>,
    pub last_reason: Option<String>,
    pub watched: Option<Id>,
    pub failures: u8,
    pub degradations: u8,
    pub next_check: u64,
    /// Backups of the connected server, best first (the fallback group's order).
    pub backups: Vec<Id>,
    /// Searches in a row in which no server answered (sets the retry backoff).
    pub misses: u32,
    /// No new search before this time after a search found nothing.
    pub retry_at: u64,
    /// The connected server was chosen without a rating; the next full check may replace it at once.
    pub provisional: bool,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "selection_memory")]
pub struct Memory {
    pub id: Id,
    pub score: Option<f64>,
    pub quarantine_until: u64,
    pub better_runs: u32,
}
/// `server: Some` records a strike, `None` a rate-limited switch.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "selection_events")]
pub struct Strike {
    pub id: Id,
    pub server: Option<Id>,
    pub at: u64,
}

pub static AUTO: State<Auto> = State::new("auto-select.auto");
pub static MEMORY: State<Vec<Memory>> = State::new("auto-select.memory");
pub static STRIKES: State<Vec<Strike>> = State::new("auto-select.strikes");

/// Turn automatic selection on or off.
pub struct SetAuto(pub bool);
impl Op for SetAuto {
    type Reply = ();
    const SAVES: bool = true;
}
/// Why a decision is due, least important first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Trigger {
    Watch,
    Servers,
    /// Servers were checked (their ids wait in [`PENDING`]).
    Checked,
    Priorities,
    Enabled,
}
/// What waits for the next decision. Queued decisions merge into one, so the
/// requests collect here and that decision handles the most important trigger.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    pub triggers: Vec<Trigger>,
    pub checked: Vec<Id>,
}
pub static PENDING: State<Pending> = State::new("auto-select.pending");

/// Ask for a decision (see [`PENDING`]).
fn request(cx: &Cx, trigger: Trigger, checked: Vec<Id>) -> Result<()> {
    cx.update_live(&PENDING, |p| {
        p.triggers.push(trigger);
        p.checked.extend(checked);
    });
    cx.spawn(Decide { watch: false }).map(drop)
}
/// Re-evaluate the connection for the triggers in [`PENDING`]; `watch` is the
/// periodic look at the connected server.
pub struct Decide {
    pub watch: bool,
}
impl Op for Decide {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        // Requests must be evaluated: queue them behind a running decision.
        // The periodic watch just joins it.
        let spec = JobSpec::new("Auto select");
        Some(if self.watch {
            spec.quiet()
        } else {
            spec.queued()
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Reason {
    Empty,
    Enabled,
    Priorities,
    Failure,
    Failover,
    Degradation,
    Improvement,
    Refined,
}
impl Reason {
    fn label(self) -> &'static str {
        match self {
            Self::Empty => "Nothing was connected",
            Self::Enabled => "Auto select turned on",
            Self::Priorities => "Subscription order changed",
            Self::Failure => "The selected server failed",
            Self::Failover => "The selected server failed; traffic moved to a backup",
            Self::Degradation => "The selected server degraded",
            Self::Improvement => "A better server was confirmed",
            Self::Refined => "Best server after the first full test",
        }
    }
    fn limited(self) -> bool {
        matches!(self, Self::Degradation | Self::Improvement)
    }
    fn punished(self) -> bool {
        matches!(self, Self::Failure | Self::Failover | Self::Degradation)
    }
    /// Without a connection (or a working one) unchecked servers are tried too.
    fn urgent(self) -> bool {
        matches!(self, Self::Empty | Self::Failure)
    }
}

/// Wait before the next search after `misses` searches in a row found nothing.
fn backoff(misses: u32) -> u64 {
    (10u64 << misses.saturating_sub(1).min(5)).min(300)
}

/// Read-only view of what the policy needs.
struct World {
    snap: Snapshot,
    servers: Vec<Server>,
    selected: Option<Id>,
}
impl World {
    fn new(snap: Snapshot) -> Self {
        Self {
            servers: snap.get(&servers::SERVERS).to_vec(),
            selected: *snap.get(&servers::SELECTED),
            snap,
        }
    }
    fn health(&self, id: Id) -> Option<Report> {
        health::report(&self.snap, id)
    }
    fn rating(&self, id: Id) -> Option<u8> {
        self.health(id).and_then(|h| h.rating)
    }
    fn excluded(&self, s: &Server) -> bool {
        servers::exclusion(&self.snap, s).is_some()
    }
    /// Higher for earlier subscriptions.
    fn level(&self, s: &Server) -> usize {
        let sources = self.snap.get(&subscriptions::SOURCES);
        sources
            .iter()
            .position(|x| x.id == s.source)
            .map_or(0, |i| sources.len() - i)
    }
    fn ideal(&self, s: &Server) -> bool {
        self.health(s.id)
            .is_some_and(|h| h.checked.is_some() && h.ideal())
    }
    fn current(&self) -> Option<&Server> {
        self.servers.iter().find(|s| Some(s.id) == self.selected)
    }
}

/// A server the policy may connect to: `value` is `None` while it has no rating.
struct Candidate {
    id: Id,
    value: Option<f64>,
    level: usize,
}
/// Rated candidates by value (then source priority), then unrated ones by source priority.
fn rank(mut c: Vec<Candidate>) -> Vec<Id> {
    c.sort_by(|a, b| match (a.value, b.value) {
        (Some(x), Some(y)) => y.total_cmp(&x).then_with(|| b.level.cmp(&a.level)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b.level.cmp(&a.level),
    });
    c.into_iter().map(|c| c.id).collect()
}

#[derive(Clone)]
struct Policy {
    auto: Auto,
    memory: Vec<Memory>,
    strikes: Vec<Strike>,
}
impl Policy {
    fn load(snap: &Snapshot) -> Self {
        Self {
            auto: (*snap.get(&AUTO)).clone(),
            memory: snap.get(&MEMORY).to_vec(),
            strikes: snap.get(&STRIKES).to_vec(),
        }
    }
    fn prune(&mut self, w: &World, now: u64) {
        self.memory
            .retain(|m| w.servers.iter().any(|s| s.id == m.id));
        self.strikes.retain(|e| {
            now.saturating_sub(e.at) < DAY
                && e.server
                    .is_none_or(|id| w.servers.iter().any(|s| s.id == id))
        });
    }
    fn memory(&mut self, id: Id) -> &mut Memory {
        let i = self
            .memory
            .iter()
            .position(|m| m.id == id)
            .unwrap_or_else(|| {
                self.memory.push(Memory {
                    id,
                    score: None,
                    quarantine_until: 0,
                    better_runs: 0,
                });
                self.memory.len() - 1
            });
        &mut self.memory[i]
    }
    fn score(&self, w: &World, s: &Server) -> f64 {
        self.memory
            .iter()
            .find(|m| m.id == s.id)
            .and_then(|m| m.score)
            .unwrap_or(f64::from(w.rating(s.id).unwrap_or(0)))
    }
    fn record(&mut self, w: &World, id: Id) {
        if let Some(r) = w.rating(id) {
            let m = self.memory(id);
            let r = f64::from(r);
            m.score = Some(m.score.map_or(r, |old| (old + r) / 2.));
        }
    }
    fn value(&self, w: &World, s: &Server) -> f64 {
        self.score(w, s)
            + if w.ideal(s) {
                1000. * (w.level(s) + 1) as f64
            } else {
                0.
            }
    }
    fn quarantined(&self, id: Id, now: u64) -> bool {
        self.memory
            .iter()
            .find(|m| m.id == id)
            .is_some_and(|m| m.quarantine_until > now)
    }
    /// Servers to switch to, best first. Rated ones need a value of at least
    /// `above`; `unrated` also admits servers that were never checked.
    fn candidates(
        &self,
        w: &World,
        now: u64,
        above: f64,
        unrated: bool,
        except: Option<Id>,
    ) -> Vec<Id> {
        let c = w
            .servers
            .iter()
            .filter(|s| Some(s.id) != except && !w.excluded(s) && !self.quarantined(s.id, now))
            .filter_map(|s| {
                let level = w.level(s);
                match w.rating(s.id) {
                    Some(0) => None,
                    Some(_) => {
                        let v = self.value(w, s);
                        (v >= above).then_some(Candidate {
                            id: s.id,
                            value: Some(v),
                            level,
                        })
                    }
                    None => unrated.then_some(Candidate {
                        id: s.id,
                        value: None,
                        level,
                    }),
                }
            })
            .collect();
        rank(c)
    }
    /// The fallback group's backups for `selected`.
    fn backups(&self, w: &World, now: u64, selected: Option<Id>) -> Vec<Id> {
        let Some(id) = selected else { return vec![] };
        self.candidates(w, now, 0., true, Some(id))
            .into_iter()
            .take(BACKUPS)
            .collect()
    }
    fn may_switch(&self, now: u64, min_stay: u64) -> bool {
        self.auto
            .last_switch
            .is_none_or(|at| now.saturating_sub(at) >= min_stay)
            && self
                .strikes
                .iter()
                .filter(|e| e.server.is_none() && now.saturating_sub(e.at) < DAY)
                .count()
                < 4
    }
    fn reset_watch(&mut self, id: Option<Id>, next_check: u64) {
        self.auto.watched = id;
        self.auto.failures = 0;
        self.auto.degradations = 0;
        self.auto.next_check = next_check;
    }
    fn switched(
        &mut self,
        previous: Option<Id>,
        next: Id,
        reason: Reason,
        failed: &[Id],
        now: u64,
    ) {
        for id in failed {
            let m = self.memory(*id);
            m.quarantine_until = m.quarantine_until.max(now + 600);
        }
        if let Some(id) = previous.filter(|_| reason.punished()) {
            let strikes = self
                .strikes
                .iter()
                .filter(|e| e.server == Some(id) && now.saturating_sub(e.at) < DAY)
                .count();
            let duration = (1800u64 << strikes.min(3)).min(14_400);
            let m = self.memory(id);
            m.quarantine_until = m.quarantine_until.max(now + duration);
            self.strikes.push(Strike {
                id: Id::random(),
                server: Some(id),
                at: now,
            });
        }
        if reason.limited() {
            self.strikes.push(Strike {
                id: Id::random(),
                server: None,
                at: now,
            });
        }
        self.auto.last_switch = Some(now);
        self.auto.last_reason = Some(reason.label().into());
        self.auto.misses = 0;
        self.auto.retry_at = 0;
        for m in &mut self.memory {
            m.better_runs = 0;
        }
        self.reset_watch(Some(next), now + CHECK_INTERVAL);
    }
}

/// Persist policy state unless the user changed the selection or Auto meanwhile.
async fn save(cx: &Cx, before: &World, mut p: Policy) -> Result<()> {
    cx.write(move |w| {
        let latest_sel = *w.base().get(&servers::SELECTED);
        let latest = (*w.get(&AUTO)).clone();
        if latest_sel != before.selected || latest.enabled != p.auto.enabled {
            return Ok(());
        }
        let world = World::new(w.base().clone());
        p.prune(&world, now());
        if *w.get(&AUTO) != p.auto {
            w.set(&AUTO, p.auto);
        }
        if *w.get(&MEMORY) != p.memory {
            w.set(&MEMORY, p.memory);
        }
        if *w.get(&STRIKES) != p.strikes {
            w.set(&STRIKES, p.strikes);
        }
        Ok(())
    })
    .await
}

async fn quick(cx: &Cx, id: Id) -> Option<Report> {
    cx.call(QuickCheck { id }).await.ok().flatten()
}
/// Quick checks of a batch of candidates running in parallel.
struct Probes {
    set: JoinSet<(usize, bool)>,
    done: Vec<Option<bool>>,
}
impl Probes {
    fn start(cx: &Cx, ids: &[Id]) -> Self {
        let mut set = JoinSet::new();
        for (i, id) in ids.iter().copied().enumerate() {
            let cx = cx.clone();
            set.spawn(async move { (i, quick(&cx, id).await.is_some_and(|q| q.works())) });
        }
        Self {
            set,
            done: vec![None; ids.len()],
        }
    }
    /// The first candidate at or after `from` that passed, as soon as every one
    /// before it has failed: the best candidate wins without waiting for slower ones.
    async fn next(&mut self, from: usize) -> Option<usize> {
        loop {
            match self.done[from..].iter().position(|d| *d != Some(false)) {
                None => return None,
                Some(i) if self.done[from + i] == Some(true) => return Some(from + i),
                Some(_) => match self.set.join_next().await {
                    Some(Ok((i, ok))) => self.done[i] = Some(ok),
                    // A crashed probe failed; with nothing left running, the rest did too.
                    Some(Err(_)) | None => {
                        if self.set.is_empty() {
                            for d in &mut self.done {
                                d.get_or_insert(false);
                            }
                        }
                    }
                },
            }
        }
    }
}
/// The backup mihomo moved traffic to, if it left the selected server.
async fn failed_over(cx: &Cx, w: &World, p: &Policy, current: Id) -> Option<Id> {
    if p.auto.backups.is_empty() {
        return None;
    }
    let now = cx.core().group_now(servers::MAIN).await.ok()??;
    let id = w.servers.iter().find(|s| s.id.to_string() == now)?.id;
    (id != current && p.auto.backups.contains(&id)).then_some(id)
}

/// What to do about the current server.
enum Verdict {
    Stay,
    /// Look for a server whose value is above the given one.
    Search(Reason, f64),
    /// mihomo already failed over from the first server to the second.
    Adopt(Id, Id),
}

async fn decide(cx: Cx, op: Decide) -> Result<()> {
    let pending = cx.update_live(&PENDING, std::mem::take);
    let watch = if op.watch {
        Trigger::Watch
    } else {
        Trigger::Servers
    };
    let result = decide_now(&cx, watch, &pending).await;
    if result.is_err() {
        // Not handled: the next decision takes it again.
        cx.update_live(&PENDING, |p| {
            p.triggers.extend(&pending.triggers);
            p.checked.extend(&pending.checked);
        });
    }
    result
}

async fn decide_now(cx: &Cx, trigger: Trigger, pending: &Pending) -> Result<()> {
    let trigger = pending.triggers.iter().copied().fold(trigger, Ord::max);
    let w = World::new(cx.snapshot());
    let time = now();
    let watch = matches!(trigger, Trigger::Watch);
    let running = cx.core().status().running();
    let mut p = Policy::load(&w.snap);
    if watch && (!p.auto.enabled || !running) {
        return Ok(());
    }
    p.prune(&w, time);
    // Every checked batch counts, even when several decisions merged into one.
    for id in &pending.checked {
        p.record(&w, *id);
    }
    if !p.auto.enabled || !running {
        return save(cx, &w, p).await;
    }
    if matches!(trigger, Trigger::Enabled) {
        p.reset_watch(w.selected, time);
        p.auto.misses = 0;
        p.auto.retry_at = 0;
    }
    if matches!(trigger, Trigger::Enabled | Trigger::Priorities) {
        for m in &mut p.memory {
            m.better_runs = 0;
        }
    }
    match verdict(cx, &w, &mut p, &trigger, time).await? {
        Verdict::Stay => {}
        Verdict::Adopt(from, backup) => return adopt(cx, &w, p, from, backup).await,
        Verdict::Search(reason, above) => {
            if search(cx, &w, &mut p, reason, above).await? {
                return Ok(());
            }
        }
    }
    // Keep the backups current. Their order is the fallback group's, so change it
    // only on events, or when a backup is no longer usable.
    let time = now();
    let stale = p.auto.backups.iter().any(|id| {
        p.quarantined(*id, time)
            || w.servers
                .iter()
                .find(|s| s.id == *id)
                .is_none_or(|s| w.excluded(s) || w.rating(s.id) == Some(0))
    });
    if !watch || stale || (p.auto.backups.is_empty() && w.selected.is_some()) {
        p.auto.backups = p.backups(&w, time, w.selected);
    }
    save(cx, &w, p).await
}

/// Whether the trigger is a reason to look for another server.
async fn verdict(
    cx: &Cx,
    w: &World,
    p: &mut Policy,
    trigger: &Trigger,
    time: u64,
) -> Result<Verdict> {
    let watch = matches!(trigger, Trigger::Watch);
    let Some(c) = w.current().cloned() else {
        // Retry an empty connection with backoff; events (new servers, a check) retry at once.
        return Ok(if !watch || time >= p.auto.retry_at {
            Verdict::Search(Reason::Empty, 0.)
        } else {
            Verdict::Stay
        });
    };
    if w.excluded(&c) {
        p.auto.next_check = time + CHECK_INTERVAL;
        return Ok(if !watch || time >= p.auto.retry_at {
            Verdict::Search(Reason::Failure, 0.)
        } else {
            Verdict::Stay
        });
    }
    let above = p.value(w, &c) + MARGIN;
    Ok(match trigger {
        Trigger::Enabled => Verdict::Search(Reason::Enabled, above),
        Trigger::Priorities => Verdict::Search(Reason::Priorities, above),
        Trigger::Checked => {
            let ahead = p.candidates(w, time, above, false, w.selected);
            for s in &w.servers {
                let m = p.memory(s.id);
                m.better_runs = if ahead.contains(&s.id) {
                    m.better_runs.saturating_add(1)
                } else {
                    0
                };
            }
            if std::mem::take(&mut p.auto.provisional) {
                Verdict::Search(Reason::Refined, above)
            } else if p.may_switch(time, 3600) {
                Verdict::Search(Reason::Improvement, above)
            } else {
                Verdict::Stay
            }
        }
        Trigger::Watch => watch_current(cx, w, p, &c, time).await?,
        Trigger::Servers => Verdict::Stay,
    })
}

/// The periodic check of the connected server: is it alive, and still good?
async fn watch_current(
    cx: &Cx,
    w: &World,
    p: &mut Policy,
    c: &Server,
    time: u64,
) -> Result<Verdict> {
    if p.auto.watched != Some(c.id) {
        p.reset_watch(Some(c.id), time);
    }
    // A passing probe also tells mihomo the server is alive again (same URL),
    // so only a failed one confirms a failover that mihomo made.
    let q = quick(cx, c.id).await;
    cx.checkpoint()?;
    if !q.as_ref().is_some_and(|q| q.works())
        && let Some(backup) = failed_over(cx, w, p, c.id).await
    {
        return Ok(Verdict::Adopt(c.id, backup));
    }
    let time = now();
    let Some(q) = q.filter(|q| q.works()) else {
        p.auto.failures = p.auto.failures.saturating_add(1);
        p.auto.degradations = 0;
        let failed = p.auto.failures >= FAILURES && time >= p.auto.retry_at;
        return Ok(if failed {
            Verdict::Search(Reason::Failure, 0.)
        } else {
            Verdict::Stay
        });
    };
    p.auto.failures = 0;
    if time < p.auto.next_check {
        return Ok(Verdict::Stay);
    }
    p.auto.next_check = time + CHECK_INTERVAL;
    let rating = f64::from(q.rating.unwrap_or(0));
    p.auto.degradations = if rating < p.score(w, c) - 20. {
        p.auto.degradations.saturating_add(1)
    } else {
        0
    };
    if p.auto.degradations < 2 || !p.may_switch(time, 900) {
        return Ok(Verdict::Stay);
    }
    let ideal = q.ideal();
    Ok(Verdict::Search(
        Reason::Degradation,
        rating
            + if ideal {
                1000. * (w.level(c) + 1) as f64
            } else {
                0.
            }
            + MARGIN,
    ))
}

/// Probe candidates above `above` and switch to the first that answers.
/// True when the decision is finished (switched and saved, or overtaken).
async fn search(cx: &Cx, w: &World, p: &mut Policy, reason: Reason, above: f64) -> Result<bool> {
    let mut candidates = p.candidates(w, now(), above, reason.urgent(), w.selected);
    if matches!(reason, Reason::Improvement) {
        candidates.retain(|id| p.memory.iter().any(|m| m.id == *id && m.better_runs >= 2));
    }
    // Urgent searches go through every candidate, a batch at a time; others try the best batch.
    let limit = if reason.urgent() {
        candidates.len()
    } else {
        candidates.len().min(BATCH)
    };
    let mut failed = Vec::new();
    for batch in candidates[..limit].chunks(BATCH) {
        let mut probes = Probes::start(cx, batch);
        let mut from = 0;
        while let Some(i) = cx.or_cancel(probes.next(from)).await? {
            failed.extend(batch[from..i].iter().copied());
            from = i + 1;
            let id = batch[i];
            let latest = World::new(cx.snapshot());
            if !cx.get(&AUTO).enabled || latest.selected != w.selected {
                return Ok(true);
            }
            if cx.call(Select::auto(Some(id), w.selected)).await.is_err() {
                failed.push(id);
                continue;
            }
            drop(probes);
            announce(cx, id, reason);
            let mut next = p.clone();
            next.switched(w.selected, id, reason, &failed, now());
            let after = World {
                selected: Some(id),
                ..World::new(cx.snapshot())
            };
            next.auto.backups = next.backups(&after, now(), Some(id));
            next.auto.provisional = after.rating(id).is_none();
            if next.auto.provisional {
                // Rate everything soon so the provisional choice can be refined.
                let _ = cx.spawn(health::CheckAll);
            }
            save(cx, &after, next).await?;
            return Ok(true);
        }
        failed.extend(batch[from..].iter().copied());
    }
    if reason.urgent() {
        // Nothing answered (often the network itself): keep what is connected
        // (its fallback group still works) and retry with backoff.
        p.auto.misses = p.auto.misses.saturating_add(1);
        let wait = backoff(p.auto.misses);
        p.auto.retry_at = now() + wait;
        // No countdown in the text: it would stand still on screen.
        p.auto.last_reason = Some(if candidates.is_empty() {
            "No eligible server: check subscriptions and exclusions".into()
        } else {
            "Keeping the current server: no candidate answered the quick test; retrying after the next test".into()
        });
        cx.summary(if candidates.is_empty() {
            "No eligible server"
        } else {
            "No server answers"
        });
    }
    if matches!(reason, Reason::Degradation) {
        p.auto.degradations = 0;
    }
    Ok(false)
}

/// Say why the connection moved, so a switch is never silent.
fn announce(cx: &Cx, to: Id, reason: Reason) {
    let servers = cx.get(&servers::SERVERS);
    let name = servers
        .iter()
        .find(|s| s.id == to)
        .map_or("another server", |s| s.name.as_str());
    cx.notify(&format!("Switched to {name}. {}.", reason.label()));
}

/// mihomo already moved traffic to `backup`: make it the selection.
async fn adopt(cx: &Cx, w: &World, mut p: Policy, from: Id, backup: Id) -> Result<()> {
    if cx
        .call(Select::auto(Some(backup), Some(from)))
        .await
        .is_err()
    {
        return save(cx, w, p).await;
    }
    announce(cx, backup, Reason::Failover);
    p.switched(Some(from), backup, Reason::Failover, &[], now());
    let after = World {
        selected: Some(backup),
        ..World::new(cx.snapshot())
    };
    p.auto.backups = p.backups(&after, now(), Some(backup));
    p.auto.provisional = false;
    save(cx, &after, p).await
}

async fn set_auto(cx: Cx, SetAuto(on): SetAuto) -> Result<()> {
    cx.write(move |w| {
        w.update(&AUTO, |a| {
            a.enabled = on;
            if !on {
                a.last_reason = None;
            }
        });
        Ok(())
    })
    .await?;
    if on {
        request(&cx, Trigger::Enabled, vec![])?;
    }
    Ok(())
}

/// A manual connect or disconnect turns Auto off.
async fn manual_choice_turns_auto_off(cx: Cx, op: Select, next: Next<Select>) -> Result<()> {
    let manual = op.auto.is_none();
    next.run(&cx, op).await?;
    if manual && cx.get(&AUTO).enabled {
        cx.call(SetAuto(false)).await?;
        cx.notify_with(
            "Auto select is off because you picked or disconnected a server yourself.",
            "Turn back on",
            || SetAuto(true),
        );
    }
    Ok(())
}

fn ui(p: &mut Setup) {
    p.fill(&servers::TOOLBAR, "auto", 10, |ui, cx, _| {
        let auto = cx.get(&AUTO);
        let mut on = auto.enabled;
        let mut help = "Connecting manually turns Auto select off.".to_owned();
        if let Some(reason) = auto.last_reason.as_ref().filter(|_| auto.enabled) {
            help.push_str(&format!("\nWhy this server: {reason}"));
        }
        let r = kit::toggle(ui, &mut on, "Auto select").on_hover_text(help);
        if r.changed() {
            cx.submit("auto", SetAuto(on));
        }
    });
    p.fill(&STATUS, "auto", 5, |ui, cx, _| {
        let auto = cx.get(&AUTO);
        if auto.enabled {
            let pal = kit::colors(ui);
            let tip = auto
                .last_reason
                .clone()
                .map_or_else(String::new, |r| format!("Why this server: {r}"));
            cx.status_chip(ui, kit::Lead::Icon(Icon::Auto, pal.accent), "Auto", &tip);
        }
    });
    p.fill(&servers::CONNECTION, "auto", 50, |ui, cx, _| {
        let auto = cx.get(&AUTO);
        if auto.enabled {
            let pal = kit::colors(ui);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.;
                kit::icon(ui, Icon::Auto, 14., pal.accent);
                // Always say what Auto select is doing, also before its first decision.
                let text = auto.last_reason.clone().map_or_else(
                    || "Auto select keeps this server while it works and switches when a clearly better one appears.".to_owned(),
                    |r| format!("Why this server: {r}"),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(text)
                            .size(kit::theme::SMALL)
                            .color(pal.muted),
                    )
                    .wrap(),
                );
            });
            ui.add_space(4.);
        }
    });
}

fn setup(p: &mut Setup) -> Result<()> {
    p.setting(&AUTO);
    p.live(&PENDING, Pending::default);
    p.collection(&MEMORY);
    p.collection(&STRIKES);
    p.cascade(&servers::SERVERS, &MEMORY, |m| m.id);

    p.handle(set_auto);
    p.handle(decide);
    p.wrap(manual_choice_turns_auto_off);
    // A filter dropped the connected server: stay on and connect another one.
    p.on(|cx: Cx, e: servers::Excluded| async move {
        if !cx.get(&AUTO).enabled {
            return Ok(());
        }
        cx.write(move |w| {
            w.update(&AUTO, |a| {
                a.last_reason = Some(format!("{} ({})", e.reason, e.server.name))
            });
            Ok(())
        })
        .await?;
        request(&cx, Trigger::Servers, vec![])
    });
    p.on(
        |cx: Cx, _: servers::ServersChanged| async move { request(&cx, Trigger::Servers, vec![]) },
    );
    p.on(|cx: Cx, e: health::Checked| async move { request(&cx, Trigger::Checked, e.servers) });
    p.on_change([subscriptions::SOURCES.name], |cx: Cx| async move {
        if cx.get(&AUTO).enabled {
            request(&cx, Trigger::Priorities, vec![])?;
        }
        Ok(())
    });
    p.every(
        "watch",
        |s| {
            s.get(&AUTO)
                .enabled
                .then_some(Duration::from_secs(WATCH_SECS))
        },
        || Decide { watch: true },
    );
    p.hook(&servers::IDLE, |snap, _| {
        let auto = snap.get(&AUTO);
        auto.enabled.then(|| {
            format!(
                "Auto select · {}",
                auto.last_reason
                    .as_deref()
                    .unwrap_or("Looking for a server that answers…")
            )
        })
    });
    // While Auto is on, the main group fails over to the policy's backups by itself.
    p.hook(&servers::FAILOVER, |snap, _| {
        let auto = snap.get(&AUTO);
        (auto.enabled && !auto.backups.is_empty()).then(|| servers::Failover {
            backups: auto.backups.clone(),
            url: health::check_url(snap),
            interval_secs: FAILOVER_SECS,
        })
    });

    ui(p);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            auto: Auto::default(),
            memory: vec![],
            strikes: vec![],
        }
    }
    #[test]
    fn strikes_escalate_but_expire_and_failures_bypass_switch_budget() {
        let mut p = policy();
        let (old, new) = (Id::random(), Id::random());
        for (time, duration) in [
            (100, 1800),
            (200, 3600),
            (300, 7200),
            (400, 14400),
            (500, 14400),
        ] {
            p.switched(Some(old), new, Reason::Failure, &[], time);
            assert_eq!(p.memory(old).quarantine_until, time + duration);
        }
        assert!(
            p.strikes.iter().all(|e| e.server.is_some()),
            "failures do not use the switch budget"
        );
        p.switched(Some(old), new, Reason::Failure, &[], DAY + 501);
        assert_eq!(p.memory(old).quarantine_until, DAY + 501 + 1800);
    }
    #[test]
    fn improvements_have_dwell_and_rolling_daily_limit() {
        let mut p = policy();
        let id = Id::random();
        for time in [0, 3600, 7200, 10800] {
            assert!(p.may_switch(time, 3600));
            p.switched(None, id, Reason::Improvement, &[], time);
            assert!(!p.may_switch(time + 3599, 3600));
        }
        assert!(!p.may_switch(14400, 3600), "four limited switches per day");
        assert!(p.may_switch(DAY, 3600));
    }
    #[test]
    fn only_successful_switch_quarantines_failed_candidates() {
        let mut p = policy();
        let failed = Id::random();
        p.switched(None, Id::random(), Reason::Empty, &[failed], 100);
        assert_eq!(p.memory(failed).quarantine_until, 700);
        assert_eq!(p.auto.next_check, 160);
        assert_eq!(p.auto.failures, 0);
    }
    #[test]
    fn rated_candidates_come_first_then_unrated_by_source_priority() {
        let ids: Vec<Id> = (0..5).map(|_| Id::random()).collect();
        let c = |i: usize, value: Option<f64>, level: usize| Candidate {
            id: ids[i],
            value,
            level,
        };
        let order = rank(vec![
            c(0, None, 1),
            c(1, Some(50.), 1),
            c(2, None, 3),
            c(3, Some(80.), 0),
            c(4, Some(50.), 2),
        ]);
        assert_eq!(order, vec![ids[3], ids[4], ids[1], ids[2], ids[0]]);
    }
    #[test]
    fn searches_without_answer_back_off_and_a_switch_resets_it() {
        assert_eq!(
            [1, 2, 3, 4, 5, 6, 9].map(backoff),
            [10, 20, 40, 80, 160, 300, 300]
        );
        let mut p = policy();
        p.auto.misses = 4;
        p.auto.retry_at = 1000;
        p.switched(None, Id::random(), Reason::Empty, &[], 100);
        assert_eq!((p.auto.misses, p.auto.retry_at), (0, 0));
    }
    #[test]
    fn failover_punishes_the_server_it_left() {
        let mut p = policy();
        let (old, new) = (Id::random(), Id::random());
        p.switched(Some(old), new, Reason::Failover, &[], 100);
        assert_eq!(p.memory(old).quarantine_until, 1900);
        assert!(
            p.strikes.iter().all(|e| e.server == Some(old)),
            "a failover does not use the switch budget"
        );
        assert!(!Reason::Refined.limited() && !Reason::Refined.punished());
    }
}
