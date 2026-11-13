//! Typed access to the bundled mihomo core.
//!
//! * [`config`] — fragments that plugins contribute and the composer that merges them;
//! * [`process`] — the supervised child process and its controller API.
pub mod config;
pub mod process;
pub mod supervisor;
pub use config::*;
pub use ipnet;
pub use process::{
    bundled, bundled_version, executable, grant_tun_script, installed, installed_update, installed_version_file,
    local_address, newer, privileged, unprivileged, version_of, ApplyReport, Controller, Delay, Mihomo, BINARY,
    CHECK_LANES,
};

#[cfg(windows)]
mod windows_job;
