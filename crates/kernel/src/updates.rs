//! Updating the mihomo core from its GitHub releases.
//!
//! The bundled core stays untouched; an update is installed into the core's
//! home folder (`<data>/core/bin/`, with its version in `mihomo.version`) and
//! wins over the bundled one while it is newer. Downloads try
//! the direct path first and then the ordinary route (the selected server), are
//! checked against the SHA-256 digest GitHub publishes for the asset, and the new
//! binary must report the expected version before it replaces anything.
use crate::{Error, Kernel, Result, jobs::Task};
use std::{path::Path, sync::Arc, time::Duration};

const LATEST: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest";
const MAX_SIZE: u64 = 120 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Without the leading `v`, e.g. `1.19.34`.
    pub version: String,
    pub url: String,
    pub sha256: Option<String>,
}

/// What Settings shows about core updates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoreUpdates {
    pub latest: Option<Release>,
    /// Unix time of the last successful check.
    pub checked: Option<u64>,
    /// An update replaced the bundled core.
    pub installed: bool,
}

/// Release asset for this platform, as named by the mihomo project.
fn asset(version: &str) -> Option<String> {
    let (os, ext) = match std::env::consts::OS {
        "linux" => ("linux", "gz"),
        "macos" => ("darwin", "gz"),
        "windows" => ("windows", "zip"),
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64-compatible",
        "aarch64" => "arm64",
        _ => return None,
    };
    Some(format!("mihomo-{os}-{arch}-v{version}.{ext}"))
}

pub use homeport_mihomo::{newer, version_of};

impl Kernel {
    pub fn core_updates(&self) -> CoreUpdates {
        // Read from disk: an app update can bring a newer bundled core, and a
        // broken update is removed when it fails to start.
        CoreUpdates {
            installed: self.installed_core().is_some(),
            ..self.updates.lock().unwrap().clone()
        }
    }
    /// The installed update's executable, if it is used instead of the bundled core.
    pub fn installed_core(&self) -> Option<std::path::PathBuf> {
        homeport_mihomo::installed_update(&self.core.home)
    }
    /// Stop the core, change its file, and start it again — also when the change failed.
    async fn swap_core(&self, change: impl FnOnce() -> std::io::Result<()>) -> Result<()> {
        self.core.stop().await;
        let changed = change().map_err(|e| Error::msg(e.to_string()));
        self.updates.lock().unwrap().installed = self.installed_core().is_some();
        let started = self.restart_core().await;
        changed.and(started)
    }

    /// GET from GitHub, directly or through the ordinary route.
    async fn fetch(&self, url: &str, timeout: Duration) -> Result<reqwest::Response> {
        self.core_api()
            .direct_or_routed("GitHub", |c| {
                c.get(url)
                    .header(reqwest::header::USER_AGENT, "homeport")
                    .timeout(timeout)
                    .send()
            })
            .await
    }

    async fn latest_release(&self) -> Result<Release> {
        let v: serde_json::Value = self
            .fetch(LATEST, Duration::from_secs(20))
            .await?
            .json()
            .await
            .map_err(|e| Error::msg(e.to_string()))?;
        let tag = v["tag_name"]
            .as_str()
            .ok_or_else(|| Error::msg("GitHub returned no release."))?;
        let version = tag.trim_start_matches('v').to_owned();
        let name =
            asset(&version).ok_or_else(|| Error::msg("No mihomo build for this platform."))?;
        let a = v["assets"]
            .as_array()
            .and_then(|l| l.iter().find(|a| a["name"] == name.as_str()))
            .ok_or_else(|| Error::msg(format!("Release {tag} has no {name}.")))?;
        Ok(Release {
            version,
            url: a["browser_download_url"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            sha256: a["digest"]
                .as_str()
                .and_then(|d| d.strip_prefix("sha256:"))
                .map(str::to_lowercase),
        })
    }

    /// Start the job "Check for core update".
    pub fn check_core_update(self: &Arc<Self>) -> Result<()> {
        let k = self.clone();
        let task = Task::new("kernel/core-update", move |job| {
            let k = k.clone();
            async move {
                let r = logged("check", k.latest_release()).await?;
                let current = k.core_status().version.unwrap_or_default();
                if newer(&r.version, &current) {
                    log::info!("mihomo {} is available", r.version);
                    job.summary(format!("mihomo {} is available", r.version));
                } else {
                    job.summary("The core is up to date");
                }
                let mut u = k.updates.lock().unwrap();
                (u.latest, u.checked) = (Some(r), Some(crate::now()));
                drop(u);
                k.repaint();
                Ok(())
            }
        })
        .label("Check for core update");
        self.jobs.start(task).map(drop)
    }

    /// Start the job "Update core": download, verify, install and restart the core.
    pub fn install_core_update(self: &Arc<Self>) -> Result<()> {
        let k = self.clone();
        let task = Task::new("kernel/core-update", move |job| {
            let k = k.clone();
            logged("update", async move {
                let r = match k.core_updates().latest {
                    Some(r) => r,
                    None => k.latest_release().await?,
                };
                let mut response = k.fetch(&r.url, Duration::from_secs(600)).await?;
                let total = response.content_length().filter(|n| *n <= MAX_SIZE);
                let mut archive = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|e| Error::msg(format!("Download failed: {e}")))?
                {
                    job.checkpoint()?;
                    archive.extend_from_slice(&chunk);
                    if archive.len() as u64 > MAX_SIZE {
                        crate::bail!("The download is larger than expected.");
                    }
                    job.progress(archive.len() as u64, total);
                }
                if let Some(expected) = &r.sha256 {
                    use sha2::Digest;
                    if &crate::hex(&sha2::Sha256::digest(&archive)) != expected {
                        crate::bail!("The download does not match its published checksum.");
                    }
                }
                let binary = unpack(&archive)?;
                let home = k.core.home.clone();
                let version = r.version.clone();
                tokio::task::spawn_blocking(move || stage(&home, &binary, &version))
                    .await
                    .map_err(|e| Error::msg(e.to_string()))??;
                // Replaced while stopped: Windows locks running executables.
                // The version file follows the binary, so a failed rename keeps a matching pair.
                let home = k.core.home.clone();
                let target = homeport_mihomo::installed(&home);
                k.swap_core(|| {
                    std::fs::rename(target.with_extension("download"), &target)?;
                    std::fs::write(homeport_mihomo::installed_version_file(&home), &r.version)
                })
                .await
                .map_err(|e| Error::msg(format!("Cannot install the core: {}", e.message)))?;
                log::info!("Core updated to mihomo {}", r.version);
                job.summary(format!("Updated to mihomo {}", r.version));
                Ok(())
            })
        })
        .label("Update core");
        self.jobs.start(task).map(drop)
    }

    /// Go back to the core shipped with the app.
    pub fn remove_core_update(self: &Arc<Self>) -> Result<()> {
        let k = self.clone();
        let task = Task::new("kernel/core-update", move |job| {
            let k = k.clone();
            async move {
                let home = k.core.home.clone();
                k.swap_core(|| {
                    // Also an older update that no longer runs.
                    for file in [
                        homeport_mihomo::installed(&home),
                        homeport_mihomo::installed_version_file(&home),
                    ] {
                        if file.is_file() {
                            std::fs::remove_file(file)?;
                        }
                    }
                    Ok(())
                })
                .await?;
                log::info!("Switched to the bundled core");
                job.summary("Using the bundled core");
                Ok(())
            }
        })
        .label("Use bundled core");
        self.jobs.start(task).map(drop)
    }
}

/// The executable inside a `.gz` (Linux, macOS) or `.zip` (Windows) archive.
fn unpack(archive: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        use std::io::Read;
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
            .map_err(|e| Error::msg(e.to_string()))?;
        let index = (0..zip.len())
            .find(|i| {
                zip.by_index(*i)
                    .is_ok_and(|f| f.name().is_ok_and(|n| n.ends_with(".exe")))
            })
            .ok_or_else(|| Error::msg("The archive has no executable."))?;
        zip.by_index(index)
            .map_err(|e| Error::msg(e.to_string()))?
            .take(MAX_SIZE)
            .read_to_end(&mut out)
            .map_err(|e| Error::msg(e.to_string()))?;
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        flate2::read::GzDecoder::new(archive)
            .take(MAX_SIZE)
            .read_to_end(&mut out)
            .map_err(|e| Error::msg(format!("Cannot unpack the core: {e}")))?;
    }
    Ok(out)
}

/// Write `<home>/bin/mihomo.download` and check that it runs and reports `version`.
fn stage(home: &Path, binary: &[u8], version: &str) -> Result<()> {
    let io = |e: std::io::Error| Error::msg(format!("Cannot install the core: {e}"));
    let path = homeport_mihomo::installed(home).with_extension("download");
    std::fs::create_dir_all(path.parent().expect("bin folder")).map_err(io)?;
    std::fs::write(&path, binary).map_err(io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(io)?;
    }
    let mut command = std::process::Command::new(&path);
    command.arg("-v");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = command.output();
    let reported = out
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    if !reported.contains(&format!("v{version}")) {
        let _ = std::fs::remove_file(&path);
        crate::bail!("The downloaded core did not start on this system.");
    }
    Ok(())
}

/// `work`, with its failure written to the log (core updates show no toast).
async fn logged<T>(what: &str, work: impl Future<Output = Result<T>>) -> Result<T> {
    let r = work.await;
    if let Err(e) = &r
        && !e.cancelled()
    {
        log::warn!("Core {what} failed: {e}");
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_platform_has_a_release_asset() {
        assert!(
            asset("1.19.34").is_some_and(|a| a.starts_with("mihomo-") && a.contains("1.19.34"))
        );
        assert!(newer("1.19.34", "v1.19.32"));
    }
}
