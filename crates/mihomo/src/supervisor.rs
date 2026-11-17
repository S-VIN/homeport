//! Internal mode of the same rproxy executable. It owns the core through a
//! lifetime lease held by the application: when the lease closes — also after
//! a crash — the core is stopped and reaped.
//!
//! * ordinary core: the lease is the supervisor's stdin pipe;
//! * tunnel core: the supervisor is started by the system's administrator
//!   prompt (see `elevation`), which passes no pipes, so the lease is a
//!   loopback connection opened with `--lease <port> <ticket file>`.
use std::{
    ffi::OsString,
    io::{self, Read, Write},
    net::{Ipv4Addr, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{OnceLock, mpsc},
    time::Duration,
};

pub const ARG: &str = "--core-supervisor";
pub(crate) const LEASE: &str = "--lease";
/// A small Go runtime: two OS threads and a soft memory limit keep the core
/// near 20–60 MB instead of growing with the CPU count. Values already in the
/// environment win.
pub(crate) const GO_ENV: [(&str, &str); 3] =
    [("GOMAXPROCS", "2"), ("GOGC", "50"), ("GOMEMLIMIT", "64MiB")];
static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

/// Applications embedding this crate must dispatch [`ARG`] to [`run`] before
/// creating any runtime, then call this before starting a kernel. Standalone
/// library tests retain the direct child path (kill-on-drop only on macOS)
/// and cannot ask for administrator rights.
pub fn configure(executable: PathBuf) {
    let _ = EXECUTABLE.set(executable);
}
pub(crate) fn executable() -> Option<&'static PathBuf> {
    EXECUTABLE.get()
}

struct OwnedCore(Child);
impl Drop for OwnedCore {
    fn drop(&mut self) {
        // SIGTERM first: mihomo removes the tunnel's routes and rules itself.
        // The child is not reaped yet, so its PID cannot be a stale one.
        #[cfg(unix)]
        if matches!(self.0.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            }
            for _ in 0..50 {
                if !matches!(self.0.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Open the lease of a tunnel core: prove with the ticket that this is the
/// process the application asked the system to start.
fn connect(args: &mut impl Iterator<Item = OsString>) -> io::Result<TcpStream> {
    let port = args
        .next()
        .and_then(|s| s.to_str()?.parse::<u16>().ok())
        .ok_or_else(|| io::Error::other("Missing lease port"))?;
    let ticket = args
        .next()
        .ok_or_else(|| io::Error::other("Missing lease ticket"))?;
    let mut lease = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
    lease.write_all(&std::fs::read(ticket)?)?;
    Ok(lease)
}

/// Root wrote caches into the user's core home (`-d`); hand them back so the
/// ordinary core can update them.
#[cfg(unix)]
fn restore_owner(args: &[OsString]) {
    use std::os::unix::fs::{MetadataExt, lchown};
    fn walk(dir: &std::path::Path, uid: u32, gid: u32) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let _ = lchown(entry.path(), Some(uid), Some(gid));
            // `file_type` does not follow symlinks: never leave the home.
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                walk(&entry.path(), uid, gid);
            }
        }
    }
    let home = args.iter().position(|a| a == "-d").map(|i| &args[i + 1..]);
    if let Some([home, ..]) = home
        && let Ok(owner) = std::fs::metadata(home)
    {
        walk(home.as_ref(), owner.uid(), owner.gid());
    }
}

pub fn run(mut args: impl Iterator<Item = OsString>) -> io::Result<()> {
    let missing = || io::Error::other("Missing core executable");
    let mut executable = PathBuf::from(args.next().ok_or_else(missing)?);
    let mut lease = None;
    if executable.as_os_str() == LEASE {
        lease = Some(connect(&mut args)?);
        executable = PathBuf::from(args.next().ok_or_else(missing)?);
        // The bundled core is named, not located: root sees this executable
        // at another path than a Flatpak sandbox or an AppImage mount does.
        if executable.parent() == Some("".as_ref()) {
            executable = std::env::current_exe()?.with_file_name(executable);
        }
    }
    let args: Vec<OsString> = args.collect();
    #[cfg(windows)]
    let _job = crate::windows_job::Job::attach(std::process::id())?;
    // A terminal may signal the entire process group. The application handles
    // graceful shutdown; this small guard must survive long enough to reap core.
    #[cfg(unix)]
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            libc::signal(signal, libc::SIG_IGN);
        }
    }
    let mut command = Command::new(executable);
    command.args(&args).stdin(Stdio::null());
    for (key, value) in GO_ENV {
        if std::env::var_os(key).is_none() {
            command.env(key, value);
        }
    }
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
                {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if libc::getppid() as u32 != parent {
                        return Err(io::Error::other("Supervisor exited"));
                    }
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
    let mut owner: Box<dyn Read + Send> = match &lease {
        Some(lease) => Box::new(lease.try_clone()?),
        None => Box::new(io::stdin()),
    };
    let mut core = OwnedCore(command.spawn()?);
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("core-owner".into())
        .spawn(move || {
            let mut byte = [0];
            loop {
                match owner.read(&mut byte) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = tx.send(());
        })?;
    let result = loop {
        match core.0.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) if status.success() => break Ok(()),
            Ok(Some(status)) => break Err(io::Error::other(format!("Core exited: {status}"))),
            Err(e) => break Err(e),
        }
        if !matches!(
            rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            break Ok(());
        }
    };
    drop(core); // Before the Windows Job, and before the lease reports the stop.
    #[cfg(unix)]
    if lease.is_some() {
        restore_owner(&args);
    }
    result
}
