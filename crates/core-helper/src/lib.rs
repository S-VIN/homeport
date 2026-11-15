//! Small, GUI-independent privileged worker. No installation, service or setuid files.
use std::{
    io::{self, BufRead, Read, Write},
    net::{Ipv4Addr, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

#[cfg(windows)]
pub mod windows_job;

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Launch {
    pub core: PathBuf,
    pub config: PathBuf,
}

pub const BINARY: &str = if cfg!(windows) {
    "rproxy-core-helper.exe"
} else {
    "rproxy-core-helper"
};

/// Quote exactly one shell argument (including apostrophes and newlines).
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn apple_script(command: &str) -> String {
    format!(
        "do shell script \"{}\" with administrator privileges",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// Win32 CommandLineToArgvW quoting, also used by Rust's argument parser.
pub fn windows_quote(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            '\\',
            if c == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        out.push(c);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

struct OwnedCore(Child);
impl Drop for OwnedCore {
    fn drop(&mut self) {
        // Give Unix mihomo time to restore routes/DNS before the hard fallback.
        #[cfg(unix)]
        if self.0.try_wait().ok().flatten().is_none() {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            for _ in 0..50 {
                if self.0.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The authenticated connection is a lifetime lease. Closing it (including an
/// application crash) kills and reaps the privileged core.
pub fn serve(port: u16, ticket: PathBuf) -> io::Result<()> {
    let token = std::fs::read(&ticket)?;
    if token.len() != 32 {
        return Err(io::Error::other("Invalid tunnel ticket"));
    }
    let mut lease = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
    lease.set_read_timeout(Some(Duration::from_secs(10)))?;
    lease.write_all(&token)?;
    // Bounded, newline-delimited request. The parent only sends it after authentication.
    let mut line = String::new();
    let mut input = io::BufReader::new(lease.try_clone()?).take(64 * 1024);
    input.read_line(&mut line)?;
    let spec: Launch = serde_json::from_str(&line)?;
    let session = spec
        .config
        .parent()
        .ok_or_else(|| io::Error::other("Missing session folder"))?;
    // Cleanup is limited to the private, disposable session beside the ticket;
    // never accept a persistent core home or an arbitrary config directory.
    if !session.is_absolute()
        || !session
            .file_name()
            .is_some_and(|s| s.to_string_lossy().starts_with("rproxy-"))
        || spec.config != ticket.with_file_name("config.json")
    {
        return Err(io::Error::other("Invalid tunnel session folder"));
    }
    let home = session.join("core-data");
    std::fs::create_dir(&home)?;
    let result = supervise(&mut lease, &spec, &home);
    // Clean the session even after the app crashes. Its root-owned cache cannot
    // be removed by the ordinary user; Flatpak also staged executables here.
    let _ = std::fs::remove_dir_all(session);
    result
}

fn supervise(lease: &mut TcpStream, spec: &Launch, home: &std::path::Path) -> io::Result<()> {
    #[cfg(unix)]
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(signal, libc::SIG_IGN);
        }
    }
    let mut command = Command::new(&spec.core);
    command
        .arg("-d")
        .arg(home)
        .arg("-f")
        .arg(&spec.config)
        .env("GOMAXPROCS", "2")
        .env("GOGC", "50")
        .env("GOMEMLIMIT", "64MiB")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        #[cfg(target_os = "linux")]
        let parent = std::process::id();
        unsafe {
            command.pre_exec(move || {
                for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                #[cfg(target_os = "linux")]
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0
                    || libc::getppid() as u32 != parent
                {
                    return Err(io::Error::other("Tunnel supervisor exited"));
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut core = OwnedCore(command.spawn()?);
    // Attach the child, not ourselves: closing a job containing this worker
    // would kill us before cache cleanup and before reporting normal shutdown.
    #[cfg(windows)]
    let _job = windows_job::Job::attach(core.0.id())?;
    let controller = std::fs::read(&spec.config)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    lease.set_read_timeout(None)?;
    let mut reader = lease.try_clone()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut byte = [0];
        while matches!(reader.read(&mut byte), Ok(1..)) {}
        let _ = tx.send(());
    });
    loop {
        if let Some(status) = core.0.try_wait()? {
            return Err(io::Error::other(format!("Tunnel core exited: {status}")));
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => {
                if let Some(controller) = &controller {
                    disable_tun(controller);
                }
                return Ok(());
            }
        }
    }
}

/// Windows has no SIGTERM. Ask mihomo to undo its TUN routes, DNS and firewall
/// rules while it is still alive, including after the owner's process dies.
/// Only the private loopback controller is permitted; every I/O has a deadline.
fn disable_tun(config: &serde_json::Value) {
    let Some(address) = config["external-controller"]
        .as_str()
        .and_then(|s| s.parse::<std::net::SocketAddr>().ok())
        .filter(|a| a.ip().is_loopback())
    else {
        return;
    };
    let Some(secret) = config["secret"]
        .as_str()
        .filter(|s| !s.contains(['\r', '\n']))
    else {
        return;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_secs(1)) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let body = r#"{"tun":{"enable":false}}"#;
    let request = format!(
        "PATCH /configs HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {secret}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_ok() {
        let _ = stream.take(4096).read_to_end(&mut Vec::new());
    }
}

#[cfg(windows)]
pub fn request(port: u16, ticket: PathBuf) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, INFINITE, WaitForSingleObject},
        UI::Shell::{
            SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
        },
    };
    let file: Vec<u16> = std::env::current_exe()?
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let args: Vec<u16> = format!(
        "--serve {port} {}\0",
        windows_quote(&ticket.to_string_lossy())
    )
    .encode_utf16()
    .collect();
    unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of_val(&info) as u32;
        info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = args.as_ptr();
        if ShellExecuteExW(&mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        if info.hProcess.is_null() {
            return Err(io::Error::other("No tunnel process handle"));
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if code != 0 {
            return Err(io::Error::other(format!("Tunnel helper exited: {code}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quotes_preserve_paths_and_cannot_inject_commands() {
        assert_eq!(shell_quote("a'b $HOME `id`"), "'a'\\''b $HOME `id`'");
        assert_eq!(windows_quote("C:\\a b\\"), "\"C:\\a b\\\\\"");
        assert_eq!(windows_quote("a\"b"), "\"a\\\"b\"");
        assert!(apple_script("echo \"a\\b\"").contains("echo \\\"a\\\\b\\\""));
    }
}
