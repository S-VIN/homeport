//! Owns one mihomo child. Control uses a private Unix socket or authenticated loopback TCP on Windows.
use crate::config::{self, Config, Listener, System, Target, overlaps};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    process::{Child, Command},
    time::{sleep, timeout},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub type Result<T> = std::result::Result<T, String>;

/// Number of isolated check lanes (`checks-N` listeners routed to `test-N` groups).
pub const CHECK_LANES: usize = 6;

/// Outcome of one delay test run by mihomo through a proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delay {
    /// The response arrived (with an accepted status) after this many ms.
    Passed(u32),
    /// No response within the timeout.
    Timeout,
    /// A response arrived, but its status was not accepted.
    WrongStatus,
    /// The connection through the proxy failed.
    Failed,
}
impl Delay {
    pub fn ms(self) -> Option<u32> {
        match self {
            Self::Passed(ms) => Some(ms),
            _ => None,
        }
    }
}

/// Soft failures of one apply: listeners that could not start.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApplyReport {
    pub listener_errors: BTreeMap<String, String>,
}

/// Cheap, cloneable access to the controller API (no process lock needed).
#[derive(Clone)]
pub struct Controller {
    http: Client,
    base: String,
}
impl Controller {
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let r = req
            .send()
            .await
            .map_err(|_| "Mihomo control socket is unavailable.".to_string())?;
        if !r.status().is_success() {
            let status = r.status().as_u16();
            let body = r.text().await.unwrap_or_default();
            return Err(format!(
                "Mihomo rejected the request (HTTP {status}). {}",
                body.trim()
            ));
        }
        Ok(r)
    }
    pub async fn version(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct V {
            version: String,
        }
        let v: V = self
            .send(self.http.get(self.url("version")))
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())?;
        Ok(v.version)
    }
    pub async fn select(&self, group: &str, target: Option<&Target>) -> Result<()> {
        let name = target.map(Target::wire).unwrap_or_else(|| "REJECT".into());
        self.send(
            self.http
                .put(self.url(&format!("proxies/{group}")))
                .json(&json!({ "name": name })),
        )
        .await
        .map(drop)
    }
    /// Delay test (HEAD request, redirects not followed) run by mihomo through
    /// `proxy`. `expected` is mihomo's status syntax, e.g. `204` or `200-299/301`.
    /// Mihomo answers a status mismatch with a delay and only marks its history
    /// entry with zero, so the history decides it.
    pub async fn delay(
        &self,
        proxy: &str,
        test_url: &str,
        timeout_ms: u32,
        expected: Option<&str>,
    ) -> Result<Delay> {
        #[derive(Deserialize)]
        struct D {
            delay: u32,
        }
        let timeout_s = timeout_ms.to_string();
        let mut query = vec![("url", test_url), ("timeout", timeout_s.as_str())];
        if let Some(e) = expected {
            query.push(("expected", e));
        }
        let r = self
            .http
            .get(self.url(&format!("proxies/{proxy}/delay")))
            .query(&query)
            .timeout(Duration::from_millis(timeout_ms as u64 + 2000))
            .send()
            .await
            .map_err(|_| "Mihomo control socket is unavailable.".to_string())?;
        match r.status().as_u16() {
            504 => return Ok(Delay::Timeout),
            503 => return Ok(Delay::Failed),
            200 => {}
            status => {
                let body = r.text().await.unwrap_or_default();
                return Err(format!(
                    "Mihomo rejected the request (HTTP {status}). {}",
                    body.trim()
                ));
            }
        }
        let d: D = r.json().await.map_err(|e| e.to_string())?;
        if expected.is_some() {
            let info = self.proxy(proxy).await?;
            let last = info["extra"][test_url]["history"]
                .as_array()
                .and_then(|h| h.last())
                .and_then(|e| e["delay"].as_u64());
            if last == Some(0) {
                return Ok(Delay::WrongStatus);
            }
        }
        Ok(Delay::Passed(d.delay.max(1)))
    }
    /// `GET /proxies/{name}` as returned by mihomo.
    pub async fn proxy(&self, name: &str) -> Result<Value> {
        self.send(self.http.get(self.url(&format!("proxies/{name}"))))
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
    /// The member a group currently uses (`now`).
    pub async fn group_now(&self, group: &str) -> Result<Option<String>> {
        Ok(self.proxy(group).await?["now"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_owned))
    }
    pub async fn connections(&self) -> Result<Value> {
        self.send(self.http.get(self.url("connections")))
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
    /// The core's live log, one JSON object per line (`{"type":"info","payload":"…"}`);
    /// the response streams until the core stops.
    pub async fn logs(&self, level: &str) -> Result<reqwest::Response> {
        let url = self.url(&format!("logs?level={level}"));
        // The client's short timeout is for requests; a stream stays open while
        // the core runs (a day here, then the reader opens a new one).
        self.send(self.http.get(url).timeout(Duration::from_secs(86_400)))
            .await
    }
    pub async fn close_connection(&self, id: &str) -> Result<()> {
        self.send(self.http.delete(self.url(&format!("connections/{id}"))))
            .await
            .map(drop)
    }
    pub async fn close_all(&self) -> Result<()> {
        self.send(self.http.delete(self.url("connections")))
            .await
            .map(drop)
    }
    /// `GET /providers/rules`: `{providers: {name: {ruleCount, updatedAt, …}}}`.
    pub async fn rule_providers(&self) -> Result<Value> {
        self.send(self.http.get(self.url("providers/rules")))
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
    /// Download a rule set again now.
    pub async fn update_rule_provider(&self, name: &str) -> Result<()> {
        self.send(
            self.http
                .put(self.url(&format!("providers/rules/{name}")))
                .timeout(Duration::from_secs(60)),
        )
        .await
        .map(drop)
    }
    /// `GET /configs`: the running general settings (e.g. whether TUN is up).
    pub async fn configs(&self) -> Result<Value> {
        self.send(self.http.get(self.url("configs")))
            .await?
            .json()
            .await
            .map_err(|e| e.to_string())
    }
}
impl Controller {
    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }
}

pub struct Mihomo {
    child: Child,
    supervised: bool,
    #[cfg(windows)]
    _job: Option<crate::windows_job::Job>,
    _dir: TempDir,
    endpoint: Value,
    ctl: Controller,
    check_ports: Vec<u16>,
    routed_port: u16,
    check_password: String,
    applied: Option<(Config, Vec<Listener>)>,
    check_targets: Vec<Option<Target>>,
    force: bool,
    last_errors: BTreeMap<String, String>,
}

/// File name of the core executable on this platform.
pub const BINARY: &str = if cfg!(windows) {
    "mihomo.exe"
} else {
    "mihomo"
};

/// An updated core installed into the home directory (`<home>/bin/mihomo`).
pub fn installed(home: &Path) -> PathBuf {
    home.join("bin").join(BINARY)
}
/// The version of the installed update, written next to it (`<home>/bin/mihomo.version`).
pub fn installed_version_file(home: &Path) -> PathBuf {
    installed(home).with_extension("version")
}
/// The version of the bundled core, e.g. `1.19.32` (from `assets/mihomo/manifest.json`).
pub fn bundled_version() -> &'static str {
    static VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    VERSION.get_or_init(|| {
        let manifest: Value =
            serde_json::from_str(include_str!("../../../assets/mihomo/manifest.json"))
                .unwrap_or_default();
        manifest["version"].as_str().unwrap_or_default().to_owned()
    })
}
/// The installed update, used only while it is newer than the bundled core
/// (an app update can bring a newer core than an old download).
pub fn installed_update(home: &Path) -> Option<PathBuf> {
    let version = fs::read_to_string(installed_version_file(home)).ok()?;
    let path = installed(home);
    (path.is_file() && newer(version.trim(), bundled_version())).then_some(path)
}
/// `1.19.34` from `v1.19.34`, `Mihomo Meta v1.19.34 linux …` and similar.
pub fn version_of(text: &str) -> Option<Vec<u32>> {
    let start = text.find(|c: char| c.is_ascii_digit())?;
    let v: Vec<u32> = text[start..]
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()?
        .split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    (v.len() >= 2).then_some(v)
}
pub fn newer(candidate: &str, current: &str) -> bool {
    matches!((version_of(candidate), version_of(current)), (Some(a), Some(b)) if a > b)
}

/// macOS: a root-owned setuid copy of the core that may create a utun
/// interface (`<home>/bin/mihomo-tun`). See [`grant_tun_script`].
pub fn privileged(home: &Path) -> PathBuf {
    home.join("bin").join("mihomo-tun")
}
/// The setuid copy is used only while it is a copy of the current core:
/// `cp -p` keeps size and modification time, so a core update shows the
/// Grant access button again.
#[cfg(target_os = "macos")]
fn privileged_copy_of(copy: &Path, core: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(c), Ok(o)) = (fs::metadata(copy), fs::metadata(core)) else {
        return false;
    };
    c.uid() == 0 && c.mode() & 0o4000 != 0 && c.len() == o.len() && c.mtime() == o.mtime()
}
/// The shell commands that make [`privileged`] a setuid-root copy of `core`;
/// macOS runs them with `osascript … with administrator privileges`.
pub fn grant_tun_script(core: &Path, target: &Path) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
    let tmp = target.with_extension("tmp");
    let commands = format!(
        "mkdir -p {dir} && rm -f {tmp} && cp -p {core} {tmp} && chown root:wheel {tmp} && chmod 4755 {tmp} && mv -f {tmp} {target}",
        dir = q(target.parent().unwrap_or(Path::new("."))),
        core = q(core),
        tmp = q(&tmp),
        target = q(target),
    );
    // AppleScript string literal: escape backslashes and quotes.
    format!(
        "do shell script \"{}\" with administrator privileges",
        commands.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

/// The core without the macOS setuid copy: an installed update newer than the
/// bundled core, otherwise the bundled one.
pub fn unprivileged(home: &Path) -> PathBuf {
    installed_update(home).unwrap_or_else(bundled)
}
/// The core to run: [`unprivileged`], or on macOS its setuid copy once the user granted it.
pub fn executable(home: &Path) -> PathBuf {
    let core = unprivileged(home);
    #[cfg(target_os = "macos")]
    if privileged_copy_of(&privileged(home), &core) {
        return privileged(home);
    }
    core
}

/// The core shipped with this build.
pub fn bundled() -> PathBuf {
    let binary = BINARY;
    let platform = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    // Packages contain only their own core beside the executable. Development
    // builds use the corresponding checked-in platform directory, never a host fallback.
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/mihomo")
        .join(platform)
        .join(arch)
        .join(binary);
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(binary)))
        .filter(|p| p.is_file())
        .unwrap_or(bundled)
}

pub fn local_address(listen: IpAddr, port: u16) -> SocketAddr {
    let ip = match listen {
        IpAddr::V4(v) if v.is_unspecified() => Ipv4Addr::LOCALHOST.into(),
        IpAddr::V6(v) if v.is_unspecified() => Ipv6Addr::LOCALHOST.into(),
        a => a,
    };
    SocketAddr::new(ip, port)
}

impl Mihomo {
    /// Start the core. `home` keeps files between runs (rule sets, an updated core);
    /// the control socket and generated config live in a private temporary folder.
    /// A core that cannot start is skipped: the macOS setuid copy, then an
    /// installed update, then the bundled core. A broken update is removed so
    /// Settings and the next start agree on which core runs.
    pub async fn start(reserved_ports: &[u16], home: &Path) -> Result<Self> {
        let mut cores = vec![executable(home), unprivileged(home), bundled()];
        cores.dedup();
        let mut error = String::new();
        for core in cores {
            match Self::launch(reserved_ports, home, &core).await {
                Ok(m) => return Ok(m),
                Err(e) => error = e,
            }
            // Only a core that could not run at all is removed, not a slow start.
            let broken = error.starts_with("Cannot start mihomo")
                || error == "Mihomo exited during startup.";
            if broken && core == installed(home) {
                let _ = fs::remove_file(installed_version_file(home));
                let _ = fs::remove_file(&core);
            }
            if core != bundled() {
                eprintln!(
                    "rproxy: {} failed ({error}); trying the next core.",
                    core.display()
                );
            }
        }
        Err(error)
    }
    async fn launch(reserved_ports: &[u16], home: &Path, core: &Path) -> Result<Self> {
        fs::create_dir_all(home).map_err(|e| format!("Cannot create {}: {e}", home.display()))?;
        let mut builder = tempfile::Builder::new();
        builder.prefix("rproxy-");
        #[cfg(unix)]
        builder.permissions(fs::Permissions::from_mode(0o700));
        let dir = builder.tempdir().map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let socket = dir.path().join("control.sock");
        // Keep every probe socket open until all lanes have distinct ports.
        let mut probes = Vec::new();
        let mut check_ports = Vec::new();
        while check_ports.len() < CHECK_LANES + 1 {
            let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
            let port = probe.local_addr().map_err(|e| e.to_string())?.port();
            if !reserved_ports.contains(&port) {
                check_ports.push(port);
            }
            probes.push(probe);
        }
        drop(probes);
        let routed_port = check_ports.pop().expect("one port for the routed listener");
        let http = crate::http().no_proxy().timeout(Duration::from_secs(4));
        #[cfg(unix)]
        let (http, base, endpoint) = (
            http.unix_socket(socket.clone()),
            "http://localhost".to_owned(),
            json!({"external-controller-unix": socket}),
        );
        #[cfg(windows)]
        let (http, base, endpoint) = {
            let listener =
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
            let addr = listener.local_addr().map_err(|e| e.to_string())?;
            let secret = uuid::Uuid::new_v4().to_string();
            let mut headers = reqwest::header::HeaderMap::new();
            let mut auth =
                reqwest::header::HeaderValue::from_str(&format!("Bearer {secret}")).unwrap();
            auth.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, auth);
            (
                http.default_headers(headers),
                format!("http://{addr}"),
                json!({"external-controller": addr.to_string(), "secret": secret}),
            )
        };
        let http = http
            .build()
            .map_err(|_| "Cannot create mihomo client.".to_string())?;
        let path = dir.path().join("config.json");
        let mut initial = json!({"mode": "rule", "rules": ["MATCH,REJECT"],
            "dns": {"enable": false}, "log-level": "silent"});
        initial
            .as_object_mut()
            .unwrap()
            .extend(endpoint.as_object().unwrap().clone());
        fs::write(&path, initial.to_string()).map_err(|e| e.to_string())?;
        let supervised = crate::supervisor::executable().is_some();
        let mut cmd = if let Some(guard) = crate::supervisor::executable() {
            let mut cmd = Command::new(guard);
            cmd.arg(crate::supervisor::ARG).arg(core);
            cmd
        } else {
            Command::new(core)
        };
        cmd.arg("-d")
            .arg(home)
            .arg("-f")
            .arg(path)
            .stdin(if supervised {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .kill_on_drop(!supervised);
        // A small Go runtime: two OS threads and a soft memory limit keep the
        // core near 20–60 MB instead of growing with the CPU count. Values
        // already in the environment win.
        for (key, value) in [("GOMAXPROCS", "2"), ("GOGC", "50"), ("GOMEMLIMIT", "64MiB")] {
            if std::env::var_os(key).is_none() {
                cmd.env(key, value);
            }
        }
        // RPROXY_MIHOMO_LOG=debug|info|warning shows the core's log on stderr.
        if config::log_level() == "silent" {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        } else {
            cmd.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }
        // The kernel stops the core if the application crashes or is killed.
        #[cfg(target_os = "linux")]
        let parent = std::process::id();
        #[cfg(target_os = "linux")]
        if !supervised {
            unsafe {
                cmd.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() as u32 != parent {
                        return Err(std::io::Error::other("Parent exited"));
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let child = cmd
            .spawn()
            .map_err(|e| format!("Cannot start mihomo: {e}"))?;
        #[cfg(windows)]
        let job = if supervised {
            None
        } else {
            Some(
                crate::windows_job::Job::attach(
                    child.id().ok_or("Mihomo exited before job assignment")?,
                )
                .map_err(|e| e.to_string())?,
            )
        };
        let mut core = Self {
            child,
            supervised,
            #[cfg(windows)]
            _job: job,
            _dir: dir,
            endpoint,
            ctl: Controller { http, base },
            check_ports,
            routed_port,
            check_password: format!("{:016x}{:016x}", rand(), rand()),
            applied: None,
            check_targets: vec![None; CHECK_LANES],
            force: false,
            last_errors: BTreeMap::new(),
        };
        for _ in 0..150 {
            if !core.alive() {
                return Err("Mihomo exited during startup.".into());
            }
            if core.version().await.is_ok() {
                return Ok(core);
            }
            sleep(Duration::from_millis(40)).await;
        }
        Err("Mihomo controller did not become ready.".into())
    }
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
    pub async fn stop(&mut self) {
        if self.supervised {
            // Never kill the guard first: closing the pipe asks it to kill+reap.
            drop(self.child.stdin.take());
        } else {
            let _ = self.child.start_kill();
        }
        let _ = self.child.wait().await;
    }
    pub fn check_ports(&self) -> &[u16] {
        &self.check_ports
    }
    pub fn applied(&self) -> Option<&Config> {
        self.applied
            .as_ref()
            .filter(|_| !self.force)
            .map(|(c, _)| c)
    }
    /// Re-send the full configuration on the next apply (retries failed listeners).
    pub fn force_next(&mut self) {
        self.force = true;
    }

    // ---- controller -------------------------------------------------------
    pub fn controller(&self) -> Controller {
        self.ctl.clone()
    }
    pub async fn version(&self) -> Result<String> {
        self.ctl.version().await
    }
    pub async fn select(&self, group: &str, target: Option<&Target>) -> Result<()> {
        self.ctl.select(group, target).await
    }

    /// HTTP client that exits through the isolated check lane `lane`.
    pub fn check_client(&self, lane: usize) -> Result<Client> {
        let port = *self
            .check_ports
            .get(lane)
            .ok_or_else(|| "Unknown check lane.".to_string())?;
        let proxy = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{port}"))
            .map_err(|_| "Invalid test listener.".to_string())?
            .basic_auth("check", &self.check_password);
        crate::http()
            .no_proxy()
            .proxy(proxy)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| "Cannot create test client.".to_string())
    }
    /// HTTP client whose traffic follows the ordinary rules (like the user's).
    pub fn routed_client(&self) -> Result<Client> {
        let proxy = reqwest::Proxy::all(format!("socks5h://127.0.0.1:{}", self.routed_port))
            .map_err(|_| "Invalid routed listener.".to_string())?
            .basic_auth("check", &self.check_password);
        crate::http()
            .no_proxy()
            .proxy(proxy)
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| "Cannot create routed client.".to_string())
    }
    pub async fn route_checks(&mut self, lane: usize, target: Option<Target>) -> Result<()> {
        if lane >= self.check_targets.len() {
            return Err("Unknown check lane.".into());
        }
        self.select(&format!("test-{lane}"), target.as_ref())
            .await?;
        self.check_targets[lane] = target;
        Ok(())
    }

    // ---- configuration ----------------------------------------------------
    /// Apply `cfg`. Optional listeners that cannot bind are reported and left out.
    /// After a failure the next apply re-sends everything (the core may be half-updated).
    pub async fn apply(&mut self, cfg: &Config) -> Result<ApplyReport> {
        let r = self.apply_inner(cfg).await;
        match &r {
            Ok(report) => self.last_errors = report.listener_errors.clone(),
            Err(_) => self.force = true,
        }
        r
    }
    async fn apply_inner(&mut self, cfg: &Config) -> Result<ApplyReport> {
        let force = std::mem::take(&mut self.force);
        if let Some((old, _)) = self.applied.as_ref().filter(|_| !force)
            && old.same_structure(cfg)
        {
            for g in &cfg.groups {
                let before = old
                    .groups
                    .iter()
                    .find(|o| o.name == g.name)
                    .map(|o| &o.selected);
                if g.kind == config::GroupKind::Select && before != Some(&g.selected) {
                    self.select(&g.name, g.selected.as_ref()).await?;
                }
            }
            let listeners = self.applied.as_ref().unwrap().1.clone();
            let mut report = ApplyReport::default();
            for l in cfg
                .listeners
                .iter()
                .filter(|l| !listeners.iter().any(|a| a.name == l.name))
            {
                let e = self
                    .last_errors
                    .get(&l.name)
                    .cloned()
                    .unwrap_or_else(|| "Listener is not running.".into());
                report.listener_errors.insert(l.name.clone(), e);
            }
            self.applied = Some((cfg.clone(), listeners));
            return Ok(report);
        }
        let previous: Vec<Listener> = self
            .applied
            .as_ref()
            .map(|(_, l)| l.clone())
            .unwrap_or_default();
        // Optional listeners may fail softly at startup or when they were already
        // configured (and failing) before; a newly requested listener must work.
        let known = |l: &Listener| {
            self.applied
                .as_ref()
                .is_none_or(|(c, _)| c.listeners.contains(l))
        };
        let mut report = ApplyReport::default();
        let mut active: Vec<Listener> = Vec::new();
        for l in &cfg.listeners {
            let ours = previous
                .iter()
                .any(|p| p.listen == l.listen && p.port == l.port);
            if ours {
                active.push(l.clone());
                continue;
            }
            let addr = SocketAddr::new(l.listen, l.port);
            let free = !self.check_ports.contains(&l.port)
                && l.port != self.routed_port
                && TcpListener::bind(addr).is_ok()
                && UdpSocket::bind(addr).is_ok();
            // A wildcard and a specific address on the same port collide with our own old socket.
            let ours_overlap = previous
                .iter()
                .any(|p| p.port == l.port && overlaps(p.listen, l.listen));
            if free || ours_overlap {
                active.push(l.clone());
            } else if l.optional && known(l) {
                report.listener_errors.insert(
                    l.name.clone(),
                    format!("Cannot listen on {addr}. Check the address and port."),
                );
            } else {
                return Err(format!(
                    "Cannot listen on {addr}. Check the address and port."
                ));
            }
        }
        // Release overlapping old sockets before binding replacements.
        let overlapping: Vec<&Listener> = active
            .iter()
            .filter(|l| {
                previous.iter().any(|p| {
                    p.port == l.port && p.listen != l.listen && overlaps(p.listen, l.listen)
                })
            })
            .collect();
        if !overlapping.is_empty() {
            let interim: Vec<&Listener> = active
                .iter()
                .filter(|l| !overlapping.iter().any(|o| o.name == l.name))
                .collect();
            self.put_config(cfg, &interim).await?;
        }
        let refs: Vec<&Listener> = active.iter().collect();
        self.put_config(cfg, &refs).await?;
        // Handshake every listener; drop optional ones that do not answer.
        let mut failed = Vec::new();
        for l in &active {
            if let Err(e) = handshake(l).await {
                if l.optional && known(l) {
                    failed.push(l.name.clone());
                    report.listener_errors.insert(l.name.clone(), e);
                } else {
                    return Err(e);
                }
            }
        }
        if !failed.is_empty() {
            active.retain(|l| !failed.contains(&l.name));
            let refs: Vec<&Listener> = active.iter().collect();
            self.put_config(cfg, &refs).await?;
        }
        self.applied = Some((cfg.clone(), active));
        Ok(report)
    }
    async fn put_config(&self, cfg: &Config, listeners: &[&Listener]) -> Result<()> {
        let doc = config::render(
            cfg,
            &System {
                controller: &self.endpoint,
                check_ports: &self.check_ports,
                check_password: &self.check_password,
                check_targets: &self.check_targets,
                routed_port: Some(self.routed_port),
            },
            listeners,
        );
        self.ctl
            .send(
                self.ctl
                    .http
                    .put(self.ctl.url("configs?force=false"))
                    .json(&json!({"payload": doc.to_string()})),
            )
            .await?;
        // A reload can keep an old selection by name; acknowledge every select group explicitly.
        for g in &cfg.groups {
            if g.kind == config::GroupKind::Select {
                self.select(&g.name, g.selected.as_ref()).await?;
            }
        }
        Ok(())
    }
}

async fn handshake(l: &Listener) -> Result<()> {
    let addr = local_address(l.listen, l.port);
    let socks = matches!(
        l.kind,
        config::ListenerKind::Mixed | config::ListenerKind::Socks
    );
    timeout(Duration::from_secs(2), async {
        let mut stream = TcpStream::connect(addr).await?;
        if !socks {
            // Nothing to say without a client: an accepted connection is enough.
            return Ok(());
        }
        let method = if l.users.is_empty() { 0 } else { 2 };
        stream.write_all(&[5, 1, method]).await?;
        let mut reply = [0; 2];
        stream.read_exact(&mut reply).await?;
        if reply != [5, method] {
            return Err(std::io::Error::other(
                "SOCKS authentication method mismatch",
            ));
        }
        Ok(())
    })
    .await
    .map_err(|_| format!("Listener {addr} timed out."))?
    .map_err(|_| format!("Listener {addr} failed its SOCKS handshake."))
}

fn rand() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn an_installed_core_is_used_only_while_newer_than_the_bundled_one() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        assert!(
            version_of(bundled_version()).is_some(),
            "manifest version {:?}",
            bundled_version()
        );
        assert_eq!(executable(home), bundled());
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(installed(home), b"core").unwrap();
        // An update from before version files, or an older one, is ignored.
        assert_eq!(installed_update(home), None);
        fs::write(installed_version_file(home), "1.0.0").unwrap();
        assert_eq!(executable(home), bundled());
        fs::write(installed_version_file(home), "999.0.0\n").unwrap();
        assert_eq!(installed_update(home), Some(installed(home)));
        assert_eq!(executable(home), installed(home));
        assert_eq!(unprivileged(home), installed(home));
    }
    #[test]
    fn versions_compare_numerically() {
        assert_eq!(
            version_of("Mihomo Meta v1.19.32 linux amd64"),
            Some(vec![1, 19, 32])
        );
        assert!(newer("1.19.34", "v1.19.32"));
        assert!(newer("1.20.0", "1.19.99"));
        assert!(!newer("1.19.32", "Mihomo Meta v1.19.32"));
        assert!(!newer("garbage", "1.19.32"));
    }
    #[test]
    fn the_grant_script_quotes_paths_for_the_shell_and_applescript() {
        let script = grant_tun_script(
            Path::new("/Applications/r proxy.app/Contents/MacOS/mihomo"),
            Path::new("/Users/o'neil/core/bin/mihomo-tun"),
        );
        assert!(
            script.starts_with("do shell script \"mkdir -p '/Users/o'\\\\''neil/core/bin'"),
            "{script}"
        );
        assert!(script.contains("cp -p '/Applications/r proxy.app/Contents/MacOS/mihomo' "));
        assert!(script.contains("chown root:wheel") && script.contains("chmod 4755"));
        assert!(
            script.ends_with(
                "'/Users/o'\\\\''neil/core/bin/mihomo-tun'\" with administrator privileges"
            ),
            "{script}"
        );
    }
}
