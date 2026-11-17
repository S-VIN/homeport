//! Starts the core supervisor with administrator rights for one tunnel
//! session: Polkit on Linux, the administrator dialog on macOS, UAC on
//! Windows. Nothing is installed and no file keeps the rights; the returned
//! lease connection is the supervisor's lifetime (see `supervisor`).
use crate::supervisor;
use std::{ffi::OsString, fs, path::Path, time::Duration};
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
    time::timeout,
};

const DECLINED: &str = "Tunnel access was not granted. Turn on TUN to try again.";

/// Run `core` (the core's command line) under an administrator-owned
/// supervisor. `dir` is the private session folder; the ticket written there
/// lets only the process started here open the lease.
pub async fn start(core: &[OsString], dir: &Path) -> Result<std::net::TcpStream, String> {
    // Embedders and tests opt into OS dialogs by configuring the supervisor.
    let exe =
        supervisor::executable().ok_or("Tunnel authorization is unavailable in this host.")?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let ticket = dir.join("lease");
    fs::write(&ticket, &token).map_err(|e| e.to_string())?;
    let mut args: Vec<OsString> = vec![
        supervisor::ARG.into(),
        supervisor::LEASE.into(),
        port.to_string().into(),
        ticket.into(),
    ];
    args.extend_from_slice(core);

    let lease = timeout(
        Duration::from_secs(180),
        accept(&listener, token.as_bytes()),
    );
    let expired = |_| "Timed out waiting for tunnel authorization. Turn on TUN to try again.";
    #[cfg(windows)]
    let lease = {
        let exe = exe.clone();
        tokio::task::spawn_blocking(move || run_as_administrator(&exe, &args))
            .await
            .map_err(|e| e.to_string())??;
        lease.await.map_err(expired)??
    };
    #[cfg(unix)]
    let lease = {
        // Dropped while the dialog is open (failure, timeout, cancellation): close it.
        let mut prompt = prompt(exe, &args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Cannot open the system authorization dialog: {e}"))?;
        tokio::select! {
            lease = lease => {
                // Authorized: the lease owns the supervisor now; only reap the prompt.
                tokio::spawn(async move { prompt.wait().await });
                lease.map_err(expired)??
            }
            status = prompt.wait() => {
                let mut detail = String::new();
                if let Some(mut stderr) = prompt.stderr.take() {
                    let _ = stderr.read_to_string(&mut detail).await;
                }
                // pkexec: 126 = dismissed; osascript: -128 = cancelled.
                let dismissed = status.is_ok_and(|s| s.code() == Some(126)) || detail.contains("-128");
                return Err(if dismissed {
                    DECLINED.into()
                } else {
                    format!("Could not obtain tunnel access. {}", detail.trim())
                });
            }
        }
    };
    lease.into_std().map_err(|e| e.to_string())
}

/// The first connection that presents the ticket.
async fn accept(listener: &TcpListener, token: &[u8]) -> Result<TcpStream, String> {
    loop {
        let (mut lease, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut received = vec![0; token.len()];
        let read = timeout(Duration::from_secs(2), lease.read_exact(&mut received)).await;
        if matches!(read, Ok(Ok(_))) && received == token {
            return Ok(lease);
        }
    }
}

/// Root on the host sees neither a Flatpak sandbox nor another user's
/// AppImage mount: name this executable by a path that root can start.
#[cfg(target_os = "linux")]
fn prompt(exe: &Path, args: &[OsString]) -> tokio::process::Command {
    let flatpak = fs::read_to_string("/.flatpak-info").ok().and_then(|info| {
        let files = info.lines().find_map(|l| l.strip_prefix("app-path="))?;
        Some(Path::new(files).join(exe.strip_prefix("/app").ok()?))
    });
    let appimage = std::env::var_os("APPIMAGE")
        .filter(|_| std::env::var_os("APPDIR").is_some_and(|mount| exe.starts_with(mount)));
    let mut cmd = tokio::process::Command::new(if flatpak.is_some() {
        "flatpak-spawn"
    } else {
        "pkexec"
    });
    if flatpak.is_some() {
        cmd.args(["--host", "--watch-bus", "pkexec"]);
    }
    cmd.arg("--disable-internal-agent")
        .arg(flatpak.map_or(appimage.unwrap_or(exe.into()), Into::into))
        .args(args);
    cmd
}

#[cfg(target_os = "macos")]
fn prompt(exe: &Path, args: &[OsString]) -> tokio::process::Command {
    let shell = std::iter::once(exe.as_os_str())
        .chain(args.iter().map(OsString::as_os_str))
        .map(|a| format!(" '{}'", a.to_string_lossy().replace('\'', "'\\''")))
        .collect::<String>();
    // AppleScript string literal: escape backslashes and quotes.
    let script = format!(
        "do shell script \"exec{}\" with administrator privileges",
        shell.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let mut cmd = tokio::process::Command::new("/usr/bin/osascript");
    cmd.arg("-e").arg(script);
    cmd
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn prompt(exe: &Path, args: &[OsString]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("pkexec");
    cmd.arg(exe).args(args);
    cmd
}

/// UAC: blocks until the user answers the prompt.
#[cfg(windows)]
fn run_as_administrator(exe: &Path, args: &[OsString]) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let wide = |s: &std::ffi::OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let parameters = args
        .iter()
        .map(|a| windows_quote(&a.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let (verb, file, parameters) = (
        wide("runas".as_ref()),
        wide(exe.as_os_str()),
        wide(parameters.as_ref()),
    );
    let code = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            0, // SW_HIDE
        )
    } as usize;
    // Values above 32 mean success; 5 (access denied) is a dismissed prompt.
    match code {
        33.. => Ok(()),
        5 => Err(DECLINED.into()),
        code => Err(format!("Could not obtain tunnel access (error {code}).")),
    }
}

/// One argument in the quoting that `CommandLineToArgvW` (and Rust) parses.
#[cfg(any(windows, test))]
fn windows_quote(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        // Backslashes are literal unless a quote follows them.
        let escapes = if c == '"' { slashes * 2 + 1 } else { slashes };
        out.extend(std::iter::repeat_n('\\', escapes));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_arguments_keep_spaces_quotes_and_trailing_backslashes() {
        assert_eq!(windows_quote("C:\\a b\\"), "\"C:\\a b\\\\\"");
        assert_eq!(windows_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(windows_quote("a\\\"b"), "\"a\\\\\\\"b\"");
    }
}
