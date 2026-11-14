//! One desktop owner per user (web instances remain scoped to their data directory). A second launch sends a bounded, authenticated
//! loopback request and exits; it never opens the database or starts a core.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

type Open = Box<dyn Fn() + Send + Sync>;
struct State {
    open: Mutex<Option<Open>>,
    pending: AtomicBool,
    activated: Arc<AtomicBool>,
    stopped: AtomicBool,
}
pub struct Instance {
    _lock: File,
    state: Arc<State>,
}
/// Desktop identity is independent of package location and --data-dir.
/// Flatpak is granted only this shared runtime subdirectory by its manifest.
pub fn desktop_directory() -> io::Result<std::path::PathBuf> {
    use std::path::PathBuf;
    #[cfg(target_os = "linux")]
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base =
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support"));
    base.map(|p| p.join("rproxy").join("desktop-instance"))
        .ok_or_else(|| io::Error::other("Missing user runtime directory"))
}
impl Instance {
    /// None means the existing owner accepted the activation request.
    pub fn acquire(dir: &Path) -> io::Result<Option<Self>> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(dir)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(dir.join("launch.lock"))?;
        let endpoint = dir.join("launch.endpoint");
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
                Err(std::fs::TryLockError::WouldBlock) => {
                    // The first process may still be publishing its endpoint.
                    if activate(&endpoint).is_ok() {
                        return Ok(None);
                    }
                    if Instant::now() >= deadline {
                        return Err(io::Error::other(
                            "rproxy is already running but is not responding. It may still be starting or shutting down.",
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(80));
                }
            }
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let token = uuid::Uuid::new_v4().to_string();
        // Windows mandatory file locks also block readers. Keep the endpoint
        // outside the locked file; partial startup writes are retried by clients.
        options.truncate(true);
        let mut published = options.open(&endpoint)?;
        write!(published, "{}\n{}\n", listener.local_addr()?.port(), token)?;
        published.sync_all()?;
        drop(published);
        let state = Arc::new(State {
            open: Mutex::new(None),
            pending: AtomicBool::new(false),
            activated: Arc::new(AtomicBool::new(false)),
            stopped: AtomicBool::new(false),
        });
        let worker = state.clone();
        std::thread::Builder::new()
            .name("instance-activation".into())
            .spawn(move || {
                let expected = format!("{token}\n");
                while !worker.stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                            let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
                            let mut bytes = vec![0; expected.len()];
                            if stream.read_exact(&mut bytes).is_ok()
                                && bytes == expected.as_bytes()
                                && !worker.stopped.load(Ordering::Acquire)
                            {
                                worker.activated.store(true, Ordering::Release);
                                let handler = worker.open.lock().unwrap();
                                if let Some(open) = handler.as_ref() {
                                    open();
                                } else {
                                    worker.pending.store(true, Ordering::Release);
                                }
                                let _ = stream.write_all(b"OK");
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(30))
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Some(Self { _lock: lock, state }))
    }
    pub fn stop_accepting(&self) {
        self.state.stopped.store(true, Ordering::Release);
    }
    #[cfg(any(feature = "desktop", test))]
    pub fn on_open(&self, open: impl Fn() + Send + Sync + 'static) {
        let mut handler = self.state.open.lock().unwrap();
        if self.state.pending.swap(false, Ordering::AcqRel) {
            open();
        }
        *handler = Some(Box::new(open));
    }
    #[cfg(all(feature = "desktop", any(target_os = "windows", target_os = "macos")))]
    pub fn activation_flag(&self) -> Arc<AtomicBool> {
        self.state.activated.clone()
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        self.state.stopped.store(true, Ordering::Release);
    }
}
fn activate(endpoint: &Path) -> io::Result<()> {
    let file = File::open(endpoint)?;
    let mut text = String::new();
    file.take(128).read_to_string(&mut text)?;
    let mut lines = text.lines();
    let invalid = || io::Error::other("Instance endpoint is not ready");
    let port = lines
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(invalid)?;
    let token = lines.next().filter(|s| s.len() == 36).ok_or_else(invalid)?;
    let timeout = Duration::from_millis(300);
    let mut stream = TcpStream::connect_timeout(
        &std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        timeout,
    )?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    writeln!(stream, "{token}")?;
    let mut ack = [0; 2];
    stream.read_exact(&mut ack)?;
    if ack == *b"OK" {
        Ok(())
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn second_launch_activates_owner_and_stale_endpoint_is_reclaimed() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Instance::acquire(dir.path()).unwrap().unwrap();
        // Activation during UI startup is remembered.
        assert!(Instance::acquire(dir.path()).unwrap().is_none());
        let opened = Arc::new(AtomicBool::new(false));
        let flag = opened.clone();
        owner.on_open(move || {
            flag.store(true, Ordering::Release);
        });
        assert!(opened.load(Ordering::Acquire));
        opened.store(false, Ordering::Release);
        assert!(Instance::acquire(dir.path()).unwrap().is_none());
        assert!(opened.load(Ordering::Acquire));
        drop(owner);
        assert!(Instance::acquire(dir.path()).unwrap().is_some());
    }
}
