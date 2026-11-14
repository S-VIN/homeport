pub type RunId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Cancelling,
    Succeeded,
    Cancelled,
    Failed,
}
impl Phase {
    pub fn active(self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: RunId,
    pub key: String,
    pub label: String,
    pub quiet: bool,
    pub phase: Phase,
    pub done: u64,
    pub total: Option<u64>,
    pub error: Option<crate::Error>,
    /// Unix seconds when the run started and ended.
    pub started: u64,
    pub ended: Option<u64>,
    /// Result in a few words, set by the op ("82 checked · 3 failed").
    pub summary: Option<String>,
    /// Title of the error toast if the run fails ("Couldn't refresh example.com").
    pub failure_title: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub name: String,
    pub key: String,
    /// Label of the job it starts ("Check servers").
    pub label: String,
    /// Frequent background work (not listed under Scheduled).
    pub quiet: bool,
    pub next_unix_ms: u64,
    pub every_ms: Option<u64>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Board {
    pub runs: Vec<Run>,
    pub schedules: Vec<Schedule>,
}
