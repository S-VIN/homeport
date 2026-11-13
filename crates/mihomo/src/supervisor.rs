//! Internal mode of the same rproxy executable. The owning application's stdin
//! pipe is its lifetime lease: EOF stops and reaps the core, even after a crash.
use std::{io::{self, Read}, path::PathBuf, process::{Child, Command, Stdio}, sync::{mpsc, OnceLock}, time::Duration};

pub const ARG: &str = "--core-supervisor";
static EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

/// Applications embedding this crate must dispatch [`ARG`] to [`run`] before
/// creating any runtime, then call this before starting a kernel. Standalone
/// library tests retain the direct child path (kill-on-drop only on macOS).
pub fn configure(executable: PathBuf) { let _ = EXECUTABLE.set(executable); }
pub(crate) fn executable() -> Option<&'static PathBuf> { EXECUTABLE.get() }

struct OwnedCore(Child);
impl Drop for OwnedCore {
    fn drop(&mut self) {
        // Child owns the OS handle / unreaped PID; never reopen or kill a stale PID.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run(mut args: impl Iterator<Item = std::ffi::OsString>) -> io::Result<()> {
    let executable = args.next().ok_or_else(|| io::Error::other("Missing core executable"))?;
    #[cfg(windows)]
    let _job = crate::windows_job::Job::attach(std::process::id())?;
    // A terminal may signal the entire process group. The application handles
    // graceful shutdown; this small guard must survive long enough to reap core.
    #[cfg(unix)]
    unsafe {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] { libc::signal(signal, libc::SIG_IGN); }
    }
    let mut command = Command::new(executable);
    command.args(args).stdin(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        #[cfg(target_os = "linux")]
        let parent = std::process::id();
        unsafe { command.pre_exec(move || {
            for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] { libc::signal(signal, libc::SIG_DFL); }
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 { return Err(io::Error::last_os_error()); }
                if libc::getppid() as u32 != parent { return Err(io::Error::other("Supervisor exited")); }
            }
            Ok(())
        }); }
    }
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(0x08000000); }
    let mut core = OwnedCore(command.spawn()?);
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("core-owner".into()).spawn(move || {
        let mut byte = [0];
        let mut input = io::stdin().lock();
        loop {
            match input.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {},
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = tx.send(());
    })?;
    loop {
        if let Some(status) = core.0.try_wait()? {
            return if status.success() { Ok(()) } else { Err(io::Error::other(format!("Core exited: {status}"))) };
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {},
            _ => return Ok(()), // OwnedCore drops before the Windows Job.
        }
    }
}
