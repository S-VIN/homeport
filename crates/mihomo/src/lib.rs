//! Typed access to the bundled mihomo core.
//!
//! * [`config`] — fragments that plugins contribute and the composer that merges them;
//! * [`process`] — the supervised child process and its controller API.
pub mod config;
pub mod process;
pub use config::*;
pub use process::{local_address, ApplyReport, Controller, Delay, Mihomo, CHECK_LANES};
pub use ipnet;

#[cfg(windows)]
mod windows_job;
