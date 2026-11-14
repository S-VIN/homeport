//! Server health: how well each server works, and checks on demand.
//! Implemented by the `health-checks` plugin.
use rproxy_kernel::{Hook, Id, JobSpec, Op, Snapshot};

/// The capability id for `provides` / `requires`.
pub const CAPABILITY: &str = "health";
/// A download speed that counts as fast (bytes per second, 1 Mbit/s).
pub const FULL_SPEED: u64 = 125_000;
/// Probe used when no check URL is configured.
pub const PROBE_URL: &str = "https://www.gstatic.com/generate_204";

/// What the last check of a server found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub ping_ms: Option<u32>,
    pub bytes_per_sec: Option<u64>,
    /// 0–100; `None` while unrated.
    pub rating: Option<u8>,
    /// When the server was checked (unix seconds).
    pub checked: Option<u64>,
    /// Configured test URLs that answered as expected, of `tests`.
    pub passed: usize,
    pub tests: usize,
}
impl Report {
    /// Reachable and at least half of the tests passed.
    pub fn works(&self) -> bool {
        self.ping_ms.is_some() && self.passed * 2 >= self.tests
    }
    /// Every test passed and the download speed is full.
    pub fn ideal(&self) -> bool {
        self.passed == self.tests && self.bytes_per_sec.unwrap_or(0) >= FULL_SPEED
    }
}

/// The last report of a server (answered by the implementation).
pub static REPORT: Hook<Id, Option<Report>> = Hook::new("health.report");
/// The URL that tells whether a server works (answered by the implementation).
pub static CHECK_URL: Hook<(), Option<String>> = Hook::new("health.check-url");

pub fn report(snap: &Snapshot, server: Id) -> Option<Report> {
    snap.hooks(&REPORT, &server).into_iter().flatten().next()
}
pub fn check_url(snap: &Snapshot) -> String {
    snap.hooks(&CHECK_URL, &())
        .into_iter()
        .flatten()
        .next()
        .unwrap_or_else(|| PROBE_URL.into())
}

/// Check every eligible server (job "Check servers").
pub struct CheckAll;
impl Op for CheckAll {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Check servers").page("servers"))
    }
}
/// Fast check without a download, not stored (e.g. a candidate right before
/// switching to it).
pub struct QuickCheck {
    pub id: Id,
}
impl Op for QuickCheck {
    type Reply = Option<Report>;
}
/// Emitted after a full check pass.
#[derive(Clone, Debug)]
pub struct Checked {
    pub servers: Vec<Id>,
}
