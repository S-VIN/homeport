//! The mihomo core as a kernel service. Plugins describe configuration with
//! [`Setup::core`](crate::Setup::core) fragments; the kernel composes and applies
//! them on every commit. Imperative access goes through [`CoreApi`].
use crate::{Error, Result, Snapshot, changes::Changes, mihomo, setup::Registry};
use mihomo::{Config, Mihomo, Target};
use std::{
    collections::BTreeMap,
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
    /// mihomo's home folder: rule-set caches and an updated core survive restarts.
    pub(crate) home: std::path::PathBuf,
}

pub(crate) fn compose(reg: &Registry, snap: &Snapshot) -> Result<Config> {
    Config::compose(
        reg.fragments
            .iter()
            .map(|(p, f)| (reg.plugins[*p].id, f(snap))),
    )
    .map_err(Error::msg)
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
        }
    }
    pub fn status(&self) -> CoreStatus {
        self.status.read().unwrap().clone()
    }
    fn set(&self, f: impl FnOnce(&mut CoreStatus)) {
        f(&mut self.status.write().unwrap());
        self.changes.notify();
    }
    /// (Re)start the process and apply the configuration of `snap`.
    pub async fn start(&self, reg: &Registry, snap: &Snapshot) -> Result<()> {
        let _lanes = self.lanes.acquire_many(mihomo::CHECK_LANES as u32).await;
        let mut proc = self.proc.lock().await;
        if let Some(mut old) = proc.take() {
            old.stop().await;
        }
        self.set(|s| *s = CoreStatus::default());
        let result: Result<(Mihomo, mihomo::ApplyReport)> = async {
            let cfg = compose(reg, snap)?;
            let ports: Vec<u16> = cfg.listeners.iter().map(|l| l.port).collect();
            let mut m = Mihomo::start(&ports, &self.home)
                .await
                .map_err(Error::msg)?;
            match m.apply(&cfg).await {
                Ok(report) => Ok((m, report)),
                Err(e) => {
                    m.stop().await;
                    Err(Error::msg(e))
                }
            }
        }
        .await;
        match result {
            Ok((m, report)) => {
                let version = m.version().await.ok();
                *proc = Some(m);
                self.set(|s| {
                    s.state = CoreState::Running;
                    s.version = version;
                    s.listener_errors = report.listener_errors;
                });
                Ok(())
            }
            Err(e) => {
                self.set(|s| s.state = CoreState::Failed(e.message.clone()));
                Err(e)
            }
        }
    }
    /// Apply the configuration of `snap` if the core runs. While it is offline
    /// configuration stays editable and is applied on the next start.
    pub async fn sync(&self, reg: &Registry, snap: &Snapshot) -> Result<()> {
        let cfg = compose(reg, snap)?;
        let mut proc = self.proc.lock().await;
        let Some(m) = proc.as_mut() else {
            return Ok(());
        };
        if m.applied() == Some(&cfg) {
            return Ok(());
        }
        let report = m.apply(&cfg).await.map_err(Error::msg)?;
        if self.status.read().unwrap().listener_errors != report.listener_errors {
            self.set(|s| s.listener_errors = report.listener_errors);
        }
        Ok(())
    }
    pub async fn monitor(&self) {
        let mut proc = self.proc.lock().await;
        if proc.as_mut().is_some_and(|m| !m.alive()) {
            *proc = None;
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
    /// `GET /connections` as returned by mihomo.
    pub async fn connections(&self) -> Result<serde_json::Value> {
        self.core
            .controller()
            .await?
            .connections()
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
    /// Rule sets known to the core: `{name: {ruleCount, updatedAt, …}}`.
    pub async fn rule_providers(&self) -> Result<serde_json::Value> {
        let v = self
            .core
            .controller()
            .await?
            .rule_providers()
            .await
            .map_err(Error::msg)?;
        Ok(v["providers"].clone())
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
    /// The running general settings (`GET /configs`).
    pub async fn configs(&self) -> Result<serde_json::Value> {
        self.core
            .controller()
            .await?
            .configs()
            .await
            .map_err(Error::msg)
    }
}
