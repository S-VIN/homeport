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
    ApplyReport, BINARY, CHECK_LANES, Controller, Delay, Mihomo, bundled, bundled_version,
    executable, grant_tun_script, installed, installed_update, installed_version_file,
    local_address, newer, privileged, unprivileged, version_of,
};

#[cfg(windows)]
mod windows_job;

/// Every HTTP client of the app starts here: TLS uses ring (installed once).
pub fn http() -> reqwest::ClientBuilder {
    static RING: std::sync::Once = std::sync::Once::new();
    RING.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    reqwest::Client::builder()
}
