//! Auto select: connects to the best healthy server by the tests' results,
//! rating and subscription order, re-checks candidates (in parallel) before
//! switching, and limits flapping: smoothing, dwell time, a daily switch budget
//! and escalating quarantine for servers that failed.
//!
//! The data plane does not wait for decisions: while Auto is on, the main group
//! is a mihomo `fallback` group `[selected, backups…]` ([`failover`]),
//! so traffic moves to a live backup within seconds. Every 10 s the watch adopts
//! such a failover, probes the connected server through mihomo's delay test and
//! switches after two failed probes.
//!
//! A manual [`Select`] turns Auto off; it reacts to `ServersChanged`,
//! [`Tested`] and subscription order changes.
use crate::{self as servers, QuickTest, Report, Select, Server, TestAll, Tested};
use homeport_kernel::{kit, prelude::*};
use std::time::Duration;
use tokio::task::JoinSet;

mod policy;
use policy::*;

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

/// On/off and the decisions that must survive a restart (the fallback
/// group, the switch budget); times are Unix seconds. The rest is [`Runtime`].
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Auto {
    pub enabled: bool,
    pub last_switch: Option<u64>,
    /// Backups of the connected server, best first (the fallback group's order).
    pub backups: Vec<Id>,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "servers_selection_memory")]
pub struct Memory {
    pub id: Id,
    pub score: Option<f64>,
    pub quarantine_until: u64,
    pub better_runs: u32,
}
/// `server: Some` records a strike, `None` a rate-limited switch.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "servers_selection_events")]
pub struct Strike {
    pub id: Id,
    pub server: Option<Id>,
    pub at: u64,
}

/// What Auto select needs only while the app runs: the watch of the connected
/// server (probe results in a row) and the search backoff. Live, so the
/// probes every few seconds do not write the database.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Runtime {
    pub watched: Option<Id>,
    pub failures: u8,
    pub degradations: u8,
    pub next_check: u64,
    /// Why the connected server was chosen, or why none was.
    pub last_reason: Option<String>,
    /// Searches in a row in which no server answered (sets the retry backoff).
    pub misses: u32,
    /// No new search before this time after a search found nothing.
    pub retry_at: u64,
    /// The connected server was chosen without a rating; the next full check may replace it at once.
    pub provisional: bool,
}

pub static AUTO: State<Auto> = State::new("servers.auto");
pub static RUNTIME: State<Runtime> = State::new("servers.runtime");
pub static MEMORY: State<Vec<Memory>> = State::new("servers.memory");
pub static STRIKES: State<Vec<Strike>> = State::new("servers.strikes");

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
    /// Servers were checked ([`Decide::checked`]).
    Tested,
    Priorities,
    Enabled,
}
/// Ranked sources, first = highest priority ([`servers::SOURCE`]).
fn order(snap: &Snapshot) -> Vec<Id> {
    let mut ranked: Vec<(usize, Id)> = Vec::new();
    for s in snap.get(&servers::SERVERS).iter() {
        if !ranked.iter().any(|(_, id)| *id == s.source) {
            ranked.push((servers::source(snap, s.source).rank, s.source));
        }
    }
    ranked.sort();
    ranked.into_iter().map(|(_, id)| id).collect()
}
/// A refresh rewrites servers without reordering sources: not a new priority.
async fn priorities_changed(cx: Cx, _: Vec<Id>) -> Result<()> {
    request(&cx, Trigger::Priorities, vec![])
}

/// Ask for a decision.
fn request(cx: &Cx, trigger: Trigger, checked: Vec<Id>) -> Result<()> {
    if !cx.get(&AUTO).enabled {
        return Ok(());
    }
    cx.spawn(Decide { trigger, checked }).map(drop)
}
/// Re-evaluate the connection; [`Trigger::Watch`] is the periodic look at
/// the connected server.
pub struct Decide {
    pub trigger: Trigger,
    /// Servers checked since the last decision: their results count.
    pub checked: Vec<Id>,
}
impl Op for Decide {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        // Requests must be evaluated: queue them behind a running decision.
        // The periodic watch just joins it.
        let spec = JobSpec::new("Auto select");
        Some(if self.trigger == Trigger::Watch {
            spec.quiet()
        } else {
            spec.queued()
        })
    }
    /// Waiting decisions become one: the most important trigger, every checked batch.
    fn merge(mut self, newer: Self) -> Self {
        self.trigger = self.trigger.max(newer.trigger);
        self.checked.extend(newer.checked);
        self
    }
}

/// Keep the policy state unless the user changed the selection or Auto meanwhile.
async fn save(cx: &Cx, before: &World, mut p: Policy) -> Result<()> {
    let run = std::mem::take(&mut p.run);
    let current = cx
        .write(move |w| {
            let latest_sel = *w.base().get(&servers::SELECTED);
            let latest = (*w.get(&AUTO)).clone();
            if latest_sel != before.selected || latest.enabled != p.auto.enabled {
                return Ok(false);
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
            Ok(true)
        })
        .await?;
    if current {
        cx.set_live(&RUNTIME, run);
    }
    Ok(())
}

async fn quick(cx: &Cx, id: Id) -> Option<Report> {
    cx.call(QuickTest { id }).await.ok().flatten()
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
    let id = servers::server_in(&[now]).filter(|id| w.servers.iter().any(|s| s.id == *id))?;
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

async fn decide(cx: Cx, Decide { trigger, checked }: Decide) -> Result<()> {
    let cx = &cx;
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
    for id in &checked {
        p.record(&w, *id);
    }
    if !p.auto.enabled || !running {
        return save(cx, &w, p).await;
    }
    if matches!(trigger, Trigger::Enabled) {
        p.reset_watch(w.selected, time);
        p.run.misses = 0;
        p.run.retry_at = 0;
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
        return Ok(if !watch || time >= p.run.retry_at {
            Verdict::Search(Reason::Empty, 0.)
        } else {
            Verdict::Stay
        });
    };
    if w.excluded(&c) {
        p.run.next_check = time + CHECK_INTERVAL;
        return Ok(if !watch || time >= p.run.retry_at {
            Verdict::Search(Reason::Failure, 0.)
        } else {
            Verdict::Stay
        });
    }
    let above = p.value(w, &c) + MARGIN;
    Ok(match trigger {
        Trigger::Enabled => Verdict::Search(Reason::Enabled, above),
        Trigger::Priorities => Verdict::Search(Reason::Priorities, above),
        Trigger::Tested => {
            let ahead = p.candidates(w, time, above, false, w.selected);
            for s in &w.servers {
                let m = p.memory(s.id);
                m.better_runs = if ahead.contains(&s.id) {
                    m.better_runs.saturating_add(1)
                } else {
                    0
                };
            }
            if std::mem::take(&mut p.run.provisional) {
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
    if p.run.watched != Some(c.id) {
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
        p.run.failures = p.run.failures.saturating_add(1);
        p.run.degradations = 0;
        let failed = p.run.failures >= FAILURES && time >= p.run.retry_at;
        return Ok(if failed {
            Verdict::Search(Reason::Failure, 0.)
        } else {
            Verdict::Stay
        });
    };
    p.run.failures = 0;
    if time < p.run.next_check {
        return Ok(Verdict::Stay);
    }
    p.run.next_check = time + CHECK_INTERVAL;
    let rating = f64::from(q.rating.unwrap_or(0));
    p.run.degradations = if rating < p.score(w, c) - 20. {
        p.run.degradations.saturating_add(1)
    } else {
        0
    };
    if p.run.degradations < 2 || !p.may_switch(time, 900) {
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
            match cx.call(Select::auto(Some(id), w.selected)).await {
                Ok(()) => {}
                // The selection changed meanwhile: the decision is overtaken.
                Err(e) if e.cancelled() => return Ok(true),
                Err(_) => {
                    failed.push(id);
                    continue;
                }
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
            next.run.provisional = after.rating(id).is_none();
            if next.run.provisional {
                // Rate everything soon so the provisional choice can be refined.
                let _ = cx.spawn(TestAll);
            }
            save(cx, &after, next).await?;
            return Ok(true);
        }
        failed.extend(batch[from..].iter().copied());
    }
    if reason.urgent() {
        // Nothing answered (often the network itself): keep what is connected
        // (its fallback group still works) and retry with backoff.
        p.run.misses = p.run.misses.saturating_add(1);
        // Once per outage, not per retry.
        if p.run.misses == 1 {
            match candidates.is_empty() {
                true => warn!("No eligible server to switch to"),
                false => warn!("No server answered; keeping the current one and retrying"),
            }
        }
        let wait = backoff(p.run.misses);
        p.run.retry_at = now() + wait;
        // No countdown in the text: it would stand still on screen.
        p.run.last_reason = Some(if candidates.is_empty() && w.selected.is_none() {
            "Waiting for a usable server: check subscriptions and exclusions".into()
        } else if candidates.is_empty() {
            "Keeping the current server: no other usable server".into()
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
        p.run.degradations = 0;
    }
    Ok(false)
}

/// Say why the connection moved, so a switch is never silent.
fn announce(cx: &Cx, to: Id, reason: Reason) {
    let name = servers::server(&cx.snapshot(), to).map(|s| s.name);
    let name = name.as_deref().unwrap_or("another server");
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
    p.run.provisional = false;
    save(cx, &after, p).await
}

async fn set_auto(cx: Cx, SetAuto(on): SetAuto) -> Result<()> {
    cx.write(move |w| {
        w.update(&AUTO, |a| a.enabled = on);
        Ok(())
    })
    .await?;
    info!("Auto select turned {}", if on { "on" } else { "off" });
    if !on {
        cx.update_live(&RUNTIME, |r| r.last_reason = None);
    }
    if on {
        request(&cx, Trigger::Enabled, vec![])?;
    }
    Ok(())
}

/// After a manual connect or disconnect ([`Select`]): Auto turns off.
pub(crate) async fn manual_choice(cx: &Cx) -> Result<()> {
    if cx.get(&AUTO).enabled {
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
        let mut on = cx.get(&AUTO).enabled;
        let mut help = "Connecting manually turns Auto select off.".to_owned();
        if let Some(reason) = cx.live(&RUNTIME).last_reason.as_ref().filter(|_| on) {
            help = format!("{reason}\n{help}");
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
            let tip = cx.live(&RUNTIME).last_reason.clone().unwrap_or_default();
            cx.status_chip(ui, kit::Lead::Icon(Icon::Auto, pal.accent), "Auto", &tip);
        }
    });
    p.fill(&PANELS, "auto", 5, |ui, cx, _| {
        let auto = cx.get(&AUTO);
        if !auto.enabled {
            return;
        }
        let pal = kit::colors(ui);
        // Turned on and off in the Servers toolbar.
        homeport_kernel::shell::panel_title(ui, "Auto select", |_| {});
        // Always say what Auto select is doing, also before its first decision.
        let text = cx.live(&RUNTIME).last_reason.clone().unwrap_or_else(|| {
            "Keeps the server while it works and switches when a clearly better one appears."
                .to_owned()
        });
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .size(kit::theme::SMALL)
                    .color(pal.muted),
            )
            .wrap(),
        );
    });
}

pub(crate) fn setup(p: &mut Setup) {
    p.setting(&AUTO);
    p.live(&RUNTIME, Runtime::default);
    p.collection(&MEMORY);
    p.collection(&STRIKES);
    p.cascade(&servers::SERVERS, &MEMORY, |m| m.id);

    p.handle(set_auto);
    p.handle(decide);
    // A filter dropped the connected server: stay on and connect another one.
    p.on(|cx: Cx, e: servers::Excluded| async move {
        if !cx.get(&AUTO).enabled {
            return Ok(());
        }
        cx.update_live(&RUNTIME, |r| {
            r.last_reason = Some(format!("Left {}: {}", e.server.name, e.reason))
        });
        request(&cx, Trigger::Servers, vec![])
    });
    p.on(
        |cx: Cx, _: servers::ServersChanged| async move { request(&cx, Trigger::Servers, vec![]) },
    );
    p.on(|cx: Cx, e: Tested| async move { request(&cx, Trigger::Tested, e.servers) });
    p.watch(order, priorities_changed);
    p.every(
        "watch",
        |s| {
            s.get(&AUTO)
                .enabled
                .then_some(Duration::from_secs(WATCH_SECS))
        },
        || Decide {
            trigger: Trigger::Watch,
            checked: vec![],
        },
    );
    ui(p);
}

/// Why nothing is connected while Auto is on, for the connection card.
pub(crate) fn idle(snap: &Snapshot) -> Option<String> {
    snap.get(&AUTO).enabled.then(|| {
        format!(
            "Auto select · {}",
            snap.live(&RUNTIME)
                .last_reason
                .as_deref()
                .unwrap_or("Looking for a server that answers…")
        )
    })
}

/// Backup servers for the connected one (Auto select's). With at least one
/// backup, [`servers::MAIN`] becomes a mihomo `fallback` group `[selected, backups…]`:
/// traffic moves to the first live backup within seconds, without waiting for
/// a decision. They change the member order, which is a full core reload, so
/// they should change only when the policy decides.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Failover {
    pub backups: Vec<Id>,
    /// URL mihomo tests the members with.
    pub url: String,
    pub interval_secs: u32,
}

/// While Auto is on, the main group fails over to the policy's backups by itself.
pub(crate) fn failover(snap: &Snapshot) -> Option<Failover> {
    let auto = snap.get(&AUTO);
    (auto.enabled && !auto.backups.is_empty()).then(|| Failover {
        backups: auto.backups.clone(),
        url: servers::probe_url(snap),
        interval_secs: FAILOVER_SECS,
    })
}
