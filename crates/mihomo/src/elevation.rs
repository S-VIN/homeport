//! OS authorization is per worker lifetime. No permanent root executables.
use std::{
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    time::{sleep, timeout},
};

/// Cancel a pending OS prompt on future cancellation. Once authenticated, the
/// lease owns shutdown; killing the guard first would skip TUN cleanup.
struct PendingWorker(Option<Child>);
impl Drop for PendingWorker {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.start_kill();
        }
    }
}

pub fn is_flatpak() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists())
}

fn helper() -> Result<PathBuf, String> {
    // Embedders/tests must explicitly opt into OS dialogs by configuring the supervisor.
    let app = crate::supervisor::executable()
        .ok_or("Tunnel authorization is unavailable in this host.")?;
    let path = app.with_file_name(rproxy_core_helper::BINARY);
    path.is_file()
        .then_some(path)
        .ok_or_else(|| "The tunnel helper is missing. Reinstall rproxy to enable TUN.".into())
}

pub async fn start(core: &Path, config: &Path, dir: &Path) -> Result<(Child, TcpStream), String> {
    let mut helper = helper()?;
    let mut core = core.to_path_buf();
    if is_flatpak() {
        // /app and sandbox /tmp do not exist on the host. The session folder
        // lives in the app's data directory, visible at the same host path.
        let staged_helper = dir.join(rproxy_core_helper::BINARY);
        let staged_core = dir.join(crate::BINARY);
        fs::copy(&helper, &staged_helper).map_err(|e| e.to_string())?;
        fs::copy(&core, &staged_core).map_err(|e| e.to_string())?;
        helper = staged_helper;
        core = staged_core;
    }
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .port()
        .to_string();
    let mut token = Vec::from(*uuid::Uuid::new_v4().as_bytes());
    token.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    let ticket = dir.join("authorization");
    // Parent directory is private (0700). The token never appears in argv.
    fs::write(&ticket, &token).map_err(|e| e.to_string())?;
    let mut cmd = authorization_command(&helper, &port, &ticket)?;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(false);
    let child = cmd
        .spawn()
        .map_err(|e| format!("Cannot open the system authorization dialog: {e}"))?;
    let mut pending = PendingWorker(Some(child));
    let child = pending.0.as_mut().unwrap();
    let ready = timeout(Duration::from_secs(180), async {
        loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                let mut detail = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut detail).await;
                }
                return Err(if status.code() == Some(126) || detail.contains("-128") || detail.contains("1223") {
                    "Tunnel access was not granted. Turn on TUN to try again.".to_owned()
                } else {
                    format!("Could not obtain tunnel access. {}", detail.trim())
                });
            }
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut lease, _) = accepted.map_err(|e| e.to_string())?;
                    let mut received = [0; 32];
                    if !matches!(timeout(Duration::from_secs(2), lease.read_exact(&mut received)).await, Ok(Ok(_))) || received.as_slice() != token {
                        continue;
                    }
                    let mut request = serde_json::to_vec(&rproxy_core_helper::Launch { core: core.clone(), config: config.to_path_buf() }).map_err(|e| e.to_string())?;
                    request.push(b'\n');
                    lease.write_all(&request).await.map_err(|e| e.to_string())?;
                    return Ok(lease);
                }
                _ = sleep(Duration::from_millis(50)) => {}
            }
        }
    }).await.map_err(|_| "Timed out waiting for tunnel authorization. Turn on TUN to try again.".to_owned())??;
    // The elevated worker now owns its lifetime via the lease, including Drop/crash.
    Ok((pending.0.take().unwrap(), ready))
}

fn authorization_command(helper: &Path, port: &str, ticket: &Path) -> Result<Command, String> {
    #[cfg(target_os = "linux")]
    {
        let mut cmd = if is_flatpak() {
            let mut c = Command::new("flatpak-spawn");
            c.args(["--host", "--directory=/", "pkexec"]);
            c
        } else {
            Command::new("pkexec")
        };
        cmd.arg("--disable-internal-agent")
            .arg(helper)
            .arg("--serve")
            .arg(port)
            .arg(ticket);
        Ok(cmd)
    }
    #[cfg(target_os = "macos")]
    {
        use rproxy_core_helper::{apple_script, shell_quote};
        let script = format!(
            "exec {} --serve {} {}",
            shell_quote(&helper.to_string_lossy()),
            port,
            shell_quote(&ticket.to_string_lossy())
        );
        let mut cmd = Command::new("/usr/bin/osascript");
        cmd.arg("-e").arg(apple_script(&script));
        Ok(cmd)
    }
    #[cfg(windows)]
    {
        let mut cmd = Command::new(helper);
        cmd.arg("--request")
            .arg(port)
            .arg(ticket)
            .creation_flags(0x08000000);
        Ok(cmd)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (helper, port, ticket);
        Err("TUN authorization is not supported on this OS.".into())
    }
}
