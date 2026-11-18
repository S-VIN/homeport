//! The mihomo core as a kernel service. Plugins describe configuration with
//! [`Setup::core`](crate::Setup::core) fragments; the kernel composes and applies
//! them on every commit. Imperative access goes through [`CoreApi`].
use crate::{Error, Result, Snapshot, changes::Changes, mihomo, setup::Registry};
use mihomo::{Config, Mihomo, Target};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, RwLock},
};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum CoreState {
    #[default]
    Starting,
    Running,
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreStatus {
    pub state: CoreState,
    pub version: Option<String>,
    /// Listener name → why it is not running.
    pub listener_errors: BTreeMap<String, String>,
    /// Saved TUN could not be authorized at startup; ordinary proxies still run.
    pub tun_error: Option<String>,
}
impl CoreStatus {
    pub fn running(&self) -> bool {
        self.state == CoreState::Running
    }
    pub fn error(&self) -> Option<&str> {
        match &self.state {
            CoreState::Failed(e) => Some(e),
            _ => None,
        }
    }
}

pub(crate) struct Core {
    proc: Mutex<Option<Mihomo>>,
    /// One permit per free check lane; a restart takes all of them.
    lanes: Arc<Semaphore>,
    free: std::sync::Mutex<Vec<usize>>,
    status: RwLock<CoreStatus>,
    changes: Changes,
    /// Of the last composed configuration.
    labels: RwLock<Arc<Labels>>,
    /// mihomo's home folder: rule-set caches and an updated core survive restarts.
    pub(crate) home: std::path::PathBuf,
}

pub(crate) fn compose(reg: &Registry, snap: &Snapshot) -> Result<(Config, Labels)> {
    let mut labels = Vec::new();
    let cfg = Config::compose(reg.fragments.iter().map(|(p, f)| {
        let mut f = f(snap);
        labels.append(&mut f.labels);
        (reg.plugins[*p].id, f)
    }))
    .map_err(Error::msg)?;
    let labels = Labels::new(&cfg, labels);
    Ok((cfg, labels))
}

/// mihomo names a refused proxy by its place ("proxy 3: …"): say which
/// server it is, and every other name in the user's words.
fn refused(cfg: &Config, labels: &Labels, text: &str) -> String {
    let at = text.to_ascii_lowercase().find("proxy ");
    let named = at.and_then(|at| {
        let rest = &text[at + "proxy ".len()..];
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let proxy = cfg.proxies.get(rest[..digits].parse::<usize>().ok()?)?;
        let tail = rest[digits..].strip_prefix(':')?;
        Some(format!(
            "{}{}:{tail}",
            &text[..at],
            labels.name(&proxy.name)
        ))
    });
    labels.text(&named.unwrap_or_else(|| text.to_owned()))
}

/// The names the core reports in the user's words, from the fragments'
/// [`labels`](mihomo::Fragment::labels) of the last composed configuration:
/// servers instead of their ids, `*.youtube.com` instead of a regex.
#[derive(Debug, Default)]
pub struct Labels {
    names: HashMap<String, String>,
    groups: HashSet<String>,
    inbounds: Vec<(String, String)>,
}
impl Labels {
    fn new(cfg: &Config, labels: Vec<(String, String)>) -> Self {
        let builtin = [
            ("DIRECT", "Direct"),
            ("REJECT", "Blocked"),
            ("REJECT-DROP", "Blocked"),
        ];
        let mut names: HashMap<String, String> = builtin
            .into_iter()
            .map(|(name, label)| (name.into(), label.into()))
            .collect();
        names.extend(labels);
        let inbounds = cfg
            .listeners
            .iter()
            .filter_map(|l| Some((l.name.clone(), names.get(&l.name)?.clone())))
            .collect();
        Self {
            groups: cfg.groups.iter().map(|g| g.name.clone()).collect(),
            names,
            inbounds,
        }
    }
    /// The user's words for a proxy, group, listener or rule
    /// ([`mihomo::Rule::reported`]), if a plugin gave them.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.names.get(name).map(String::as_str)
    }
    /// The user's words, else the name as the core reports it.
    pub fn name<'a>(&'a self, name: &'a str) -> &'a str {
        self.get(name).unwrap_or(name)
    }
    /// A proxy group (`main`) rather than a single outbound.
    pub fn is_group(&self, name: &str) -> bool {
        self.groups.contains(name)
    }
    /// Listeners with a label: `(name, label)`, e.g. for rules by inbound.
    pub fn inbounds(&self) -> &[(String, String)] {
        &self.inbounds
    }
    /// `text` with every labelled name in it (a whole word) in the user's words.
    pub fn text(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut start = None;
        for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
            let word = c.is_alphanumeric() || c == '-' || c == '_';
            match start {
                None if word => start = Some(i),
                Some(s) if !word => {
                    out.push_str(self.name(&text[s..i]));
                    start = None;
                }
                _ => {}
            }
            if !word && i < text.len() {
                out.push(c);
            }
        }
        out
    }
}

impl Core {
    pub fn new(changes: Changes, home: std::path::PathBuf) -> Self {
        Self {
            home,
            proc: Mutex::new(None),
            lanes: Arc::new(Semaphore::new(mihomo::CHECK_LANES)),
            free: std::sync::Mutex::new((0..mihomo::CHECK_LANES).rev().collect()),
            status: RwLock::default(),
            changes,
            labels: RwLock::default(),
        }
    }
    /// Compose the configuration of `snap` and keep its labels.
    fn compose(&self, reg: &Registry, snap: &Snapshot) -> Result<Config> {
        let (cfg, labels) = compose(reg, snap)?;
        *self.labels.write().unwrap() = Arc::new(labels);
        Ok(cfg)
    }
    pub fn status(&self) -> CoreStatus {
        self.status.read().unwrap().clone()
    }
    fn set(&self, f: impl FnOnce(&mut CoreStatus)) {
        f(&mut self.status.write().unwrap());
        self.changes.notify();
    }
    /// (Re)start the process and apply the configuration of `snap`. A saved
    /// tunnel that is not authorized is left out: the proxies still run and
    /// [`CoreStatus::tun_error`] tells why.
    pub async fn start(&self, reg: &Registry, snap: &Snapshot) -> Result<()> {
        let tun_error = self.restart(self.compose(reg, snap)).await?;
        if tun_error.is_some() {
            self.set(|s| s.tun_error = tun_error);
        }
        Ok(())
    }
    /// Stop the process and start one for `cfg`. A tunnel needs administrator
    /// rights, asked from the system for each such process; when that fails
    /// the core starts without the tunnel and the reason is returned.
    async fn restart(&self, cfg: Result<Config>) -> Result<Option<String>> {
        let _lanes = self.lanes.acquire_many(mihomo::CHECK_LANES as u32).await;
        let mut proc = self.proc.lock().await;
        if let Some(mut old) = proc.take() {
            old.stop().await;
        }
        self.set(|s| *s = CoreStatus::default());
        let mut tun_error = None;
        let result = async {
            let mut cfg = cfg?;
            match self.launch(&cfg).await {
                Err(e) if cfg.tun.is_some() => {
                    tun_error = Some(e.message);
                    cfg.tun = None;
                    self.launch(&cfg).await
                }
                result => result,
            }
        }
        .await;
        match result {
            Ok((m, report)) => {
                let version = m.version().await.ok();
                log::info!(
                    "Mihomo {} started",
                    version.as_deref().unwrap_or("(unknown version)")
                );
                if let Some(e) = &tun_error {
                    log::warn!("TUN is off: {e}");
                }
                log_listeners(&BTreeMap::new(), &report.listener_errors);
                *proc = Some(m);
                self.set(|s| {
                    s.state = CoreState::Running;
                    s.version = version;
                    s.listener_errors = report.listener_errors;
                });
                Ok(tun_error)
            }
            Err(e) => {
                self.set(|s| s.state = CoreState::Failed(e.message.clone()));
                Err(e)
            }
        }
    }
    async fn launch(&self, cfg: &Config) -> Result<(Mihomo, mihomo::ApplyReport)> {
        let ports: Vec<u16> = cfg.listeners.iter().map(|l| l.port).collect();
        let mut m = Mihomo::start(&ports, &self.home, cfg.tun.is_some())
            .await
            .map_err(Error::msg)?;
        match m.apply(cfg).await {
            Ok(report) => Ok((m, report)),
            Err(e) => {
                m.stop().await;
                Err(self.refused(cfg, &e))
            }
        }
    }
    fn refused(&self, cfg: &Config, text: &str) -> Error {
        Error::msg(refused(cfg, &self.labels.read().unwrap(), text))
    }
    /// Apply the configuration of `snap` if the core runs. While it is offline
    /// configuration stays editable and is applied on the next start.
    pub async fn sync(&self, reg: &Registry, snap: &Snapshot) -> Result<()> {
        let mut cfg = self.compose(reg, snap)?;
        // A tunnel refused at startup is asked for again only by an explicit
        // restart or off/on, never by unrelated settings writes.
        if self.status.read().unwrap().tun_error.is_some() {
            if cfg.tun.is_some() {
                cfg.tun = None;
            } else {
                self.set(|s| s.tun_error = None);
            }
        }
        let mut proc = self.proc.lock().await;
        let Some(m) = proc.as_mut() else {
            return Ok(());
        };
        if m.applied() == Some(&cfg) {
            return Ok(());
        }
        if m.elevated() != cfg.tun.is_some() {
            // Turning the tunnel on or off changes the rights of the process.
            drop(proc);
            return match self.restart(Ok(cfg)).await? {
                Some(refused) => Err(Error::msg(refused)),
                None => Ok(()),
            };
        }
        let report = m.apply(&cfg).await.map_err(|e| self.refused(&cfg, &e))?;
        log::debug!("Mihomo configuration applied");
        let before = self.status.read().unwrap().listener_errors.clone();
        if before != report.listener_errors {
            log_listeners(&before, &report.listener_errors);
            self.set(|s| s.listener_errors = report.listener_errors);
        }
        Ok(())
    }
    pub async fn monitor(&self) {
        let mut proc = self.proc.lock().await;
        if proc.as_mut().is_some_and(|m| !m.alive()) {
            *proc = None;
            log::error!("Mihomo stopped unexpectedly");
            self.set(|s| {
                s.state = CoreState::Failed(
                    "Mihomo stopped. Restart the core to restore the saved configuration.".into(),
                );
                s.listener_errors.clear();
            });
        }
    }
    pub async fn force_next(&self) {
        if let Some(m) = self.proc.lock().await.as_mut() {
            m.force_next();
        }
    }
    pub async fn stop(&self) {
        if let Some(mut m) = self.proc.lock().await.take() {
            m.stop().await;
            log::info!("Mihomo stopped");
            // Not running until started again.
            self.set(|s| s.state = CoreState::Starting);
        }
    }
    async fn with<R>(
        &self,
        f: impl AsyncFnOnce(&mut Mihomo) -> mihomo::process::Result<R>,
    ) -> Result<R> {
        let mut proc = self.proc.lock().await;
        let m = proc
            .as_mut()
            .ok_or_else(|| Error::msg("Mihomo is offline."))?;
        f(m).await.map_err(Error::msg)
    }
    /// Controller handle for requests that must not hold the process lock.
    async fn controller(&self) -> Result<mihomo::Controller> {
        self.proc
            .lock()
            .await
            .as_ref()
            .map(Mihomo::controller)
            .ok_or_else(|| Error::msg("Mihomo is offline."))
    }
}

/// Exclusive use of one isolated check lane. Traffic of [`Lease::http`] leaves
/// through `target`; ordinary traffic is not affected. Up to
/// [`mihomo::CHECK_LANES`] leases run at the same time.
pub struct Lease {
    core: Arc<Core>,
    http: reqwest::Client,
    lane: usize,
    permit: Option<OwnedSemaphorePermit>,
}
impl Lease {
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
    /// Route the lane back to REJECT, then let the next lease use it.
    pub async fn release(mut self) {
        if let Some(permit) = self.permit.take() {
            self.core.reset_lane(self.lane, permit).await;
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Cancelled before `release`: reset the route before the lane is reused.
        if let Some(permit) = self.permit.take() {
            let (core, lane) = (self.core.clone(), self.lane);
            // No runtime left (the app is exiting): mihomo is going away too.
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move { core.reset_lane(lane, permit).await });
            }
        }
    }
}
impl Core {
    async fn reset_lane(&self, lane: usize, permit: OwnedSemaphorePermit) {
        let _ = self.with(async |m| m.route_checks(lane, None).await).await;
        self.free.lock().unwrap().push(lane);
        drop(permit);
    }
}

/// Typed controller API available to plugins as `cx.core()`.
pub struct CoreApi {
    pub(crate) core: Arc<Core>,
    pub(crate) kernel: Arc<crate::Kernel>,
}
impl CoreApi {
    /// Apply the current configuration again in full (e.g. retry a listener whose port is free now).
    pub async fn reapply(&self) -> Result<()> {
        self.core.force_next().await;
        self.kernel.resync().await
    }
    pub fn status(&self) -> CoreStatus {
        self.core.status()
    }
    /// Names the core reports, in the user's words.
    pub fn labels(&self) -> Arc<Labels> {
        self.core.labels.read().unwrap().clone()
    }
    /// Route a free check lane to `target` until the lease is released
    /// (waits while every lane is in use).
    pub async fn lease(&self, target: Target) -> Result<Lease> {
        let permit = self
            .core
            .lanes
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::msg("Mihomo is stopping."))?;
        let lane = self
            .core
            .free
            .lock()
            .unwrap()
            .pop()
            .expect("a free lane for every permit");
        // From here on the lease owns the lane: dropping it (an error, or the
        // caller giving up while routing) resets and returns the lane.
        let mut lease = Lease {
            core: self.core.clone(),
            http: crate::mihomo::http().build().expect("an HTTP client"),
            lane,
            permit: Some(permit),
        };
        lease.http = self
            .core
            .with(async move |m| {
                m.route_checks(lane, Some(target)).await?;
                m.check_client(lane)
            })
            .await?;
        Ok(lease)
    }
    /// Latency of a proxy or group measured by mihomo (ms); any status is accepted.
    pub async fn delay(&self, proxy: &str, url: &str, timeout_ms: u32) -> Result<u32> {
        match self.probe(proxy, url, timeout_ms, None).await? {
            mihomo::Delay::Passed(ms) => Ok(ms),
            mihomo::Delay::Timeout => Err(Error::msg("The delay test timed out.")),
            _ => Err(Error::msg("The delay test failed.")),
        }
    }
    /// Delay test of a proxy or group run by mihomo itself (no lease needed, any
    /// number in parallel). `expected` uses mihomo's status syntax, e.g. `200-299`.
    pub async fn probe(
        &self,
        proxy: &str,
        url: &str,
        timeout_ms: u32,
        expected: Option<&str>,
    ) -> Result<mihomo::Delay> {
        self.core
            .controller()
            .await?
            .delay(proxy, url, timeout_ms, expected)
            .await
            .map_err(Error::msg)
    }
    /// The member a group currently carries traffic through (`None` when unknown).
    pub async fn group_now(&self, group: &str) -> Result<Option<String>> {
        self.core
            .controller()
            .await?
            .group_now(group)
            .await
            .map_err(Error::msg)
    }
    /// Open connections and the traffic totals.
    pub async fn connections(&self) -> Result<mihomo::Connections> {
        self.core
            .controller()
            .await?
            .connections()
            .await
            .map_err(Error::msg)
    }
    /// The core's live log (see [`mihomo::Controller::logs`]).
    pub async fn logs(&self, level: &str) -> Result<reqwest::Response> {
        self.core
            .controller()
            .await?
            .logs(level)
            .await
            .map_err(Error::msg)
    }
    pub async fn close_connection(&self, id: &str) -> Result<()> {
        self.core
            .controller()
            .await?
            .close_connection(id)
            .await
            .map_err(Error::msg)
    }
    pub async fn close_all(&self) -> Result<()> {
        self.core
            .controller()
            .await?
            .close_all()
            .await
            .map_err(Error::msg)
    }
    pub async fn version(&self) -> Result<String> {
        self.core
            .controller()
            .await?
            .version()
            .await
            .map_err(Error::msg)
    }
    /// HTTP client whose requests follow the ordinary rules, like user traffic
    /// (for downloads that may be blocked on the direct path).
    pub async fn routed_http(&self) -> Result<reqwest::Client> {
        self.core.with(async |m| m.routed_client()).await
    }
    /// Send a request directly (giving up quickly where `what` is blocked), then
    /// through the ordinary route. `send` builds the request on the given client.
    pub async fn direct_or_routed<F, Fut>(&self, what: &str, send: F) -> Result<reqwest::Response>
    where
        F: Fn(reqwest::Client) -> Fut,
        Fut: std::future::Future<Output = reqwest::Result<reqwest::Response>>,
    {
        let direct = crate::mihomo::http()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| Error::msg(e.to_string()))?;
        let direct_error = match send(direct).await.and_then(|r| r.error_for_status()) {
            Ok(r) => return Ok(r),
            Err(e) => e,
        };
        let routed = self
            .routed_http()
            .await
            .map_err(|_| Error::msg(format!("Cannot reach {what}: {direct_error}")))?;
        send(routed)
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| {
                Error::msg(format!(
                    "Cannot reach {what} directly or through the selected server: {e}"
                ))
            })
    }
    /// Rule sets known to the core, by name.
    pub async fn rule_providers(
        &self,
    ) -> Result<std::collections::BTreeMap<String, mihomo::RuleSetStatus>> {
        self.core
            .controller()
            .await?
            .rule_providers()
            .await
            .map_err(Error::msg)
    }
    pub async fn update_rule_provider(&self, name: &str) -> Result<()> {
        self.core
            .controller()
            .await?
            .update_rule_provider(name)
            .await
            .map_err(Error::msg)
    }
    /// The core executable that runs (an installed update or the bundled one).
    pub fn executable(&self) -> std::path::PathBuf {
        mihomo::executable(&self.core.home)
    }
    /// Whether the tunnel is up in the running core.
    pub async fn tun_enabled(&self) -> Result<bool> {
        self.core
            .controller()
            .await?
            .tun_enabled()
            .await
            .map_err(Error::msg)
    }
    /// Wakes up whenever the core status ([`Self::status`]) or the state changes.
    pub fn changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.kernel.changes()
    }
}

/// Listeners that went down or came back since `before`.
fn log_listeners(before: &BTreeMap<String, String>, now: &BTreeMap<String, String>) {
    for (name, e) in now.iter().filter(|(n, e)| before.get(*n) != Some(e)) {
        log::warn!("Listener {name} is down: {e}");
    }
    for name in before.keys().filter(|n| !now.contains_key(*n)) {
        log::info!("Listener {name} is up again");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_refused_proxy_is_named() {
        let mut cfg = Config::default();
        for name in ["p-a", "p-b"] {
            cfg.proxies.push(mihomo::Proxy {
                name: name.into(),
                protocol: "http".into(),
                server: "127.0.0.1".into(),
                port: 1,
                options: Default::default(),
                dialer: None,
            });
        }
        let labels = Labels::new(&cfg, vec![("p-b".into(), "Amsterdam".into())]);
        assert_eq!(
            refused(&cfg, &labels, "proxy 1: invalid REALITY public key"),
            "Amsterdam: invalid REALITY public key"
        );
        assert_eq!(refused(&cfg, &labels, "proxy 7: bad"), "proxy 7: bad");
    }
    #[test]
    fn labels_replace_whole_words_only() {
        let cfg = Config::default();
        let labels = Labels::new(&cfg, vec![("main".into(), "proxy".into())]);
        assert_eq!(
            labels.text("dial main (domain) using main[DIRECT], ok"),
            "dial proxy (domain) using proxy[Direct], ok"
        );
        assert_eq!(labels.name("REJECT"), "Blocked");
        assert_eq!(labels.name("other"), "other");
    }
}
