//! The pure Auto select policy: ranking, quarantine, switch budget and
//! the watch schedule, with no I/O.
use super::*;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Reason {
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
    pub(super) fn label(self) -> &'static str {
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
    pub(super) fn limited(self) -> bool {
        matches!(self, Self::Degradation | Self::Improvement)
    }
    pub(super) fn punished(self) -> bool {
        matches!(self, Self::Failure | Self::Failover | Self::Degradation)
    }
    /// Without a connection (or a working one) unchecked servers are tried too.
    pub(super) fn urgent(self) -> bool {
        matches!(self, Self::Empty | Self::Failure)
    }
}

/// Wait before the next search after `misses` searches in a row found nothing.
pub(super) fn backoff(misses: u32) -> u64 {
    (10u64 << misses.saturating_sub(1).min(5)).min(300)
}

/// Read-only view of what the policy needs.
pub(super) struct World {
    pub(super) snap: Snapshot,
    pub(super) servers: Vec<Server>,
    pub(super) selected: Option<Id>,
    /// Ranked sources ([`order`]).
    pub(super) sources: Vec<Id>,
    /// Every tested server's report, read once.
    pub(super) reports: HashMap<Id, Report>,
}
impl World {
    pub(super) fn new(snap: Snapshot) -> Self {
        let checks = servers::checks::checks(&snap);
        let reports = (snap.get(&servers::RESULTS).iter())
            .map(|h| (h.id, Report::of(&checks, h)))
            .collect();
        Self {
            reports,
            servers: snap.get(&servers::SERVERS).to_vec(),
            selected: *snap.get(&servers::SELECTED),
            sources: order(&snap),
            snap,
        }
    }
    pub(super) fn last_test(&self, id: Id) -> Option<Report> {
        self.reports.get(&id).cloned()
    }
    pub(super) fn rating(&self, id: Id) -> Option<u8> {
        self.last_test(id).and_then(|h| h.rating)
    }
    pub(super) fn excluded(&self, s: &Server) -> bool {
        servers::exclusion(&self.snap, s).is_some()
    }
    /// Higher for higher-priority sources; 0 for unranked ones.
    pub(super) fn level(&self, s: &Server) -> usize {
        self.sources
            .iter()
            .position(|x| *x == s.source)
            .map_or(0, |i| self.sources.len() - i)
    }
    pub(super) fn ideal(&self, s: &Server) -> bool {
        self.last_test(s.id)
            .is_some_and(|h| h.checked.is_some() && h.ideal())
    }
    pub(super) fn current(&self) -> Option<&Server> {
        self.servers.iter().find(|s| Some(s.id) == self.selected)
    }
}

/// A server the policy may connect to: `value` is `None` while it has no rating.
pub(super) struct Candidate {
    pub(super) id: Id,
    pub(super) value: Option<f64>,
    pub(super) level: usize,
}
/// Rated candidates by value (then source priority), then unrated ones by source priority.
pub(super) fn rank(mut c: Vec<Candidate>) -> Vec<Id> {
    c.sort_by(|a, b| match (a.value, b.value) {
        (Some(x), Some(y)) => y.total_cmp(&x).then_with(|| b.level.cmp(&a.level)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => b.level.cmp(&a.level),
    });
    c.into_iter().map(|c| c.id).collect()
}

#[derive(Clone)]
pub(super) struct Policy {
    pub(super) auto: Auto,
    pub(super) run: Runtime,
    pub(super) memory: Vec<Memory>,
    pub(super) strikes: Vec<Strike>,
}
impl Policy {
    pub(super) fn load(snap: &Snapshot) -> Self {
        Self {
            auto: (*snap.get(&AUTO)).clone(),
            run: (*snap.live(&RUNTIME)).clone(),
            memory: snap.get(&MEMORY).to_vec(),
            strikes: snap.get(&STRIKES).to_vec(),
        }
    }
    pub(super) fn prune(&mut self, w: &World, now: u64) {
        self.memory
            .retain(|m| w.servers.iter().any(|s| s.id == m.id));
        self.strikes.retain(|e| {
            now.saturating_sub(e.at) < DAY
                && e.server
                    .is_none_or(|id| w.servers.iter().any(|s| s.id == id))
        });
    }
    pub(super) fn memory(&mut self, id: Id) -> &mut Memory {
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
    pub(super) fn score(&self, w: &World, s: &Server) -> f64 {
        self.memory
            .iter()
            .find(|m| m.id == s.id)
            .and_then(|m| m.score)
            .unwrap_or(f64::from(w.rating(s.id).unwrap_or(0)))
    }
    pub(super) fn record(&mut self, w: &World, id: Id) {
        if let Some(r) = w.rating(id) {
            let m = self.memory(id);
            let r = f64::from(r);
            m.score = Some(m.score.map_or(r, |old| (old + r) / 2.));
        }
    }
    pub(super) fn value(&self, w: &World, s: &Server) -> f64 {
        self.score(w, s)
            + if w.ideal(s) {
                1000. * (w.level(s) + 1) as f64
            } else {
                0.
            }
    }
    pub(super) fn quarantined(&self, id: Id, now: u64) -> bool {
        self.memory
            .iter()
            .find(|m| m.id == id)
            .is_some_and(|m| m.quarantine_until > now)
    }
    /// Servers to switch to, best first. Rated ones need a value of at least
    /// `above`; `unrated` also admits servers that were never checked.
    pub(super) fn candidates(
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
    pub(super) fn backups(&self, w: &World, now: u64, selected: Option<Id>) -> Vec<Id> {
        let Some(id) = selected else { return vec![] };
        self.candidates(w, now, 0., true, Some(id))
            .into_iter()
            .take(BACKUPS)
            .collect()
    }
    pub(super) fn may_switch(&self, now: u64, min_stay: u64) -> bool {
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
    pub(super) fn reset_watch(&mut self, id: Option<Id>, next_check: u64) {
        self.run = Runtime {
            watched: id,
            failures: 0,
            degradations: 0,
            next_check,
            ..std::mem::take(&mut self.run)
        };
    }
    pub(super) fn switched(
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
        self.run.last_reason = Some(format!("Why this server: {}", reason.label()));
        self.run.misses = 0;
        self.run.retry_at = 0;
        for m in &mut self.memory {
            m.better_runs = 0;
        }
        self.reset_watch(Some(next), now + CHECK_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy {
            auto: Auto::default(),
            run: Runtime::default(),
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
        assert_eq!(p.run.next_check, 160);
        assert_eq!(p.run.failures, 0);
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
        p.run.misses = 4;
        p.run.retry_at = 1000;
        p.switched(None, Id::random(), Reason::Empty, &[], 100);
        assert_eq!((p.run.misses, p.run.retry_at), (0, 0));
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
