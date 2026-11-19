//! # local-proxy
//! One local HTTP + SOCKS5 proxy (mihomo `mixed`) with optional login, the
//! system proxy pointed at it and TUN. A port that cannot bind is reported on
//! the page; the rest keeps working.
use homeport_kernel::{
    kit::{self, theme},
    mihomo::{self, Listener, ListenerKind},
    prelude::*,
};
use std::{fmt, net::IpAddr, str::FromStr, time::Duration};

pub mod system_proxy;
mod ui;

/// The local proxy and how this device uses it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Proxy {
    pub enabled: bool,
    pub listen: Listen,
    pub port: u16,
    pub login: Option<String>,
    pub password: Option<String>,
    /// Point the operating system's proxy settings at this port.
    pub system_proxy: bool,
    /// Capture all traffic of the machine with a virtual interface (TUN).
    pub tun: bool,
}
impl Default for Proxy {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: Listen([127, 0, 0, 1].into()),
            port: 20808,
            login: None,
            password: None,
            system_proxy: false,
            tun: false,
        }
    }
}
impl Proxy {
    pub fn address(&self) -> String {
        std::net::SocketAddr::new(self.listen.0, self.port).to_string()
    }
}
pub static PROXY: State<Proxy> = State::new("local-proxy.settings");
/// Whether the tunnel is up (`None` until checked).
pub static TUN_UP: State<Option<bool>> = State::new("local-proxy.tun-up");
/// Result of the last system proxy change: what was set, or why it failed.
pub static SYSTEM_PROXY: State<Option<std::result::Result<String, String>>> =
    State::new("local-proxy.system-proxy");

/// Save the address and login (checked first; an empty password keeps the saved one).
pub struct SaveProxy(pub Proxy);
impl Op for SaveProxy {
    type Reply = ();
    const SAVES: bool = true;
}

struct ReadTun;
/// Apply the configuration again: the core retries the port.
struct RetryPort;
impl Op for RetryPort {
    type Reply = ();
}
impl Op for ReadTun {
    type Reply = ();
    fn job(&self) -> Option<JobSpec> {
        Some(JobSpec::new("Check the tunnel").quiet())
    }
}

/// The mihomo listener of the local proxy.
const LISTENER: &str = "local-proxy";

/// Where the system proxy points: the port when it is on and has no login
/// (system settings cannot pass credentials to SOCKS5).
pub fn system_proxy_address(snap: &Snapshot) -> Option<std::net::SocketAddr> {
    let p = snap.get(&PROXY);
    (device_control() && p.system_proxy && p.enabled && p.login.is_none())
        .then(|| mihomo::local_address(p.listen.0, p.port))
}

/// System proxy integration is unavailable inside Flatpak and Docker.
fn device_control() -> bool {
    !Platform::is_flatpak() && !Platform::is_docker()
}

/// Flatpak runs its authorized tunnel worker on the host.
fn tun_control() -> bool {
    !Platform::is_docker()
}

pub static PLUGIN: Plugin = Plugin {
    id: "local-proxy",
    name: "Local proxy",
    description: "A local HTTP and SOCKS5 proxy for browsers and apps, the system proxy and TUN.",
    setup,
    ..Plugin::DEFAULT
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Listen(pub IpAddr);
impl fmt::Display for Listen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for Listen {
    type Err = std::net::AddrParseError;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        s.trim().trim_matches(['[', ']']).parse().map(Self)
    }
}

/// Why the port is not running (from the last core apply).
pub fn error(status: &homeport_kernel::core::CoreStatus) -> Option<String> {
    status.listener_errors.get(LISTENER).cloned()
}

fn validate(p: &mut Proxy, previous: &Proxy) -> Result<()> {
    if p.port == 0 {
        bail!("Enter a port from 1 to 65535.");
    }
    match &mut p.login {
        Some(login) => {
            *login = login.trim().to_owned();
            if login.is_empty() || login.contains(':') {
                bail!("Enter a login without a colon.");
            }
            if p.password.as_deref().unwrap_or("").is_empty() {
                p.password = previous
                    .login
                    .is_some()
                    .then(|| previous.password.clone())
                    .flatten();
            }
            if p.password.is_none() {
                bail!("Enter a password.");
            }
        }
        None => p.password = None,
    }
    Ok(())
}

async fn save_proxy(cx: Cx, SaveProxy(mut next): SaveProxy) -> Result<()> {
    let p = cx
        .write(move |w| {
            validate(&mut next, &w.get(&PROXY))?;
            w.set(&PROXY, next.clone());
            Ok(next)
        })
        .await?;
    match p.enabled {
        true => info!("Proxy port {}:{} saved", p.listen.0, p.port),
        false => info!("Proxy port turned off"),
    }
    Ok(())
}

// Keep mutation ownership inside the blocking worker. Cancelling a service
// cannot let on_stop restore before its already queued apply has finished.
async fn apply_system_proxy(
    journal: std::path::PathBuf,
    address: Option<std::net::SocketAddr>,
) -> std::result::Result<String, String> {
    static ORDER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = ORDER.lock().await;
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        system_proxy::apply(&journal, address)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn setup(p: &mut Setup) -> Result<()> {
    p.editable_setting(&PROXY);
    p.handle(save_proxy);
    p.handle(|cx: Cx, _: RetryPort| async move { cx.core().reapply().await });
    p.live(&TUN_UP, || None);
    p.live(&SYSTEM_PROXY, || None);
    p.handle(read_tun);
    p.every(
        "tun",
        |s| (s.get(&PROXY).tun && tun_control()).then_some(Duration::from_secs(3)),
        || ReadTun,
    );
    if device_control() {
        p.on(restore_after_crash);
    }
    p.service("system-proxy", system_proxy_address, system_proxy)
        .on_stop(|cx: Cx, _| async move {
            let result = apply_system_proxy(journal(&cx), None).await;
            if result.is_ok() {
                info!("System proxy restored");
            }
            cx.set_live(&SYSTEM_PROXY, None);
            result.map(drop).map_err(Error::msg)
        });
    p.core(fragment);
    ui::register(p);
    Ok(())
}

/// What homeport changed in the system proxy settings, kept across a crash or logoff.
fn journal(cx: &Cx) -> std::path::PathBuf {
    cx.data_dir().join("system-proxy.json")
}

async fn read_tun(cx: Cx, _: ReadTun) -> Result<()> {
    let up = cx.core().tun_enabled().await.ok();
    if up.is_some() && up != *cx.live(&TUN_UP) {
        info!("TUN is {}", if up == Some(true) { "up" } else { "down" });
    }
    cx.set_live(&TUN_UP, up);
    Ok(())
}

/// Undo a system proxy left by a crash, a logoff or a killed process.
async fn restore_after_crash(cx: Cx, _: Started) -> Result<()> {
    let journal = journal(&cx);
    if journal.exists() {
        apply_system_proxy(journal, None)
            .await
            .map_err(Error::msg)?;
        info!("System proxy left by the last run restored");
    }
    Ok(())
}

/// Point the system proxy at the chosen listener while it runs; the previous
/// settings come back when it stops (`on_stop`) or is not running.
async fn system_proxy(cx: Cx, address: std::net::SocketAddr) -> Result<()> {
    let mut changes = cx.core().changes();
    let mut previous = None;
    loop {
        let status = cx.core().status();
        let ready = status.running() && !status.listener_errors.contains_key(LISTENER);
        if previous != Some(ready) {
            previous = Some(ready);
            let result = apply_system_proxy(journal(&cx), ready.then_some(address)).await.and_then(|label| {
                if ready {
                    Ok(label)
                } else {
                    Err("The proxy port is not running. Previous system proxy settings are restored until it starts.".into())
                }
            });
            match &result {
                Ok(_) => info!("System proxy set to {address}"),
                Err(e) => warn!("System proxy not set: {e}"),
            }
            cx.set_live(&SYSTEM_PROXY, Some(result));
        }
        if changes.changed().await.is_err() {
            return Ok(());
        }
    }
}

/// The listener, and the tunnel when traffic capture is on.
fn fragment(snap: &Snapshot) -> Fragment {
    let p = snap.get(&PROXY);
    let tunnel = (p.tun && tun_control()).then_some(mihomo::Tun {
        strict_route: false,
    });
    let listener = p.enabled.then(|| Listener {
        name: LISTENER.into(),
        kind: ListenerKind::Mixed,
        listen: p.listen.0,
        port: p.port,
        udp: true,
        users: p
            .login
            .clone()
            .zip(p.password.clone())
            .into_iter()
            .collect(),
        optional: true,
    });
    let mut f = Fragment::listeners(listener);
    // Domain rules see names in captured traffic. DNS is the DNS plugin's:
    // without it queries leave through the tunnel to the system's server.
    f.sniff = tunnel.is_some();
    f.tun = tunnel;
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation_keeps_password_and_rejects_bad_logins() {
        let saved = Proxy {
            login: Some("u".into()),
            password: Some("secret".into()),
            ..Proxy::default()
        };
        let mut edit = Proxy {
            password: None,
            ..saved.clone()
        };
        validate(&mut edit, &saved).unwrap();
        assert_eq!(edit.password.as_deref(), Some("secret"));
        let mut colon = Proxy {
            login: Some("a:b".into()),
            password: Some("x".into()),
            ..Proxy::default()
        };
        assert!(validate(&mut colon, &saved).is_err());
        let mut open = Proxy {
            password: Some("x".into()),
            ..Proxy::default()
        };
        validate(&mut open, &saved).unwrap();
        assert!(open.password.is_none());
        let mut fresh = Proxy {
            login: Some("new".into()),
            ..Proxy::default()
        };
        assert!(validate(&mut fresh, &Proxy::default()).is_err());
    }
}
