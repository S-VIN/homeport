//! Typed access to the bundled mihomo core.
//!
//! * [`config`] — fragments that plugins contribute and the composer that merges them;
//! * [`process`] — the supervised child process and its controller API.
pub mod config;
pub mod process;
pub use config::*;
pub use ipnet;
pub use process::{
    bundled, executable, installed, local_address, ApplyReport, Controller, Delay, Mihomo, BINARY,
    CHECK_LANES,
};

#[cfg(windows)]
mod windows_job;
