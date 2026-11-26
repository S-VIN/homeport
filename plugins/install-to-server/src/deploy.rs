//! The work on a server over SSH. Connecting signs in with the password and
//! lets homeport's own key in (`authorized_keys`); every later action signs in
//! with that key. Installing uploads the Compose stack, runs `install.sh` as
//! root and writes the panel file; the image's server preset
//! (presets/server.sql) configures the new homeport. Cleaning up runs
//! `uninstall.sh` the same way and takes the key out again.
use crate::{ACTIVITY, Health, LOG, RELEASES, Release, SERVERS, Server, Status};
use homeport_kernel::prelude::*;
use russh::keys::{
    PrivateKey, PrivateKeyWithHashAlg,
    ssh_key::{
        LineEnding,
        private::{Ed25519Keypair, KeypairData},
    },
};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// The panel's address and sign-in, in the home folder of the SSH login.
/// Any homeport that connects to the server reads it.
pub const PANEL_FILE: &str = "homeport-panel.txt";

/// The SSH access the user enters to connect a server.
#[derive(Clone, Debug, PartialEq)]
pub struct Access {
    pub host: String,
    pub ssh_port: u16,
    pub login: String,
    /// Empty: sign in with homeport's key (a server connected before).
    pub password: String,
}
impl Default for Access {
    fn default() -> Self {
        Self {
            host: String::new(),
            ssh_port: 22,
            login: "root".into(),
            password: String::new(),
        }
    }
}
impl Access {
    pub fn problem(&self) -> Option<&'static str> {
        Some(if self.host.trim().is_empty() {
            "Enter the server address."
        } else if self.login.trim().is_empty() {
            "Enter the SSH login."
        } else if self.password.contains(['\n', '\r']) {
            "The SSH password cannot contain line breaks."
        } else if self.ssh_port == 0 {
            "Enter the SSH port."
        } else {
            return None;
        })
    }
    pub fn secrets(&self) -> [String; 1] {
        [self.password.clone()]
    }
}

/// How homeport is installed. Everything has a working default.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub panel_login: String,
    /// Empty: the current one, or a random one for a new installation.
    pub panel_password: String,
    pub disable_ipv6: bool,
    /// Empty: `<server address>.sslip.io`, which needs no DNS setup.
    pub domain: String,
    pub https_port: u16,
    /// `~/…` is in the home folder of the SSH login.
    pub directory: String,
    pub image: String,
    /// Optional Let's Encrypt contact.
    pub email: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            panel_login: "admin".into(),
            panel_password: String::new(),
            disable_ipv6: true,
            domain: String::new(),
            https_port: 8443,
            directory: "~/homeport".into(),
            image: "ghcr.io/s-vin/homeport:latest".into(),
            email: String::new(),
        }
    }
}
impl Settings {
    /// What an installed server already uses, to update it as it is.
    pub fn of(server: &Server) -> Self {
        let mut settings = Self::default();
        if !server.installed() {
            return settings;
        }
        let address = server.panel.trim_start_matches("https://");
        let (name, port) = address.rsplit_once(':').unwrap_or((address, "443"));
        if let Ok(port) = port.parse() {
            settings.https_port = port;
        }
        let domain = name.strip_prefix("panel.").unwrap_or(name);
        if !domain.ends_with(".sslip.io") {
            settings.domain = domain.into();
        }
        settings.panel_login = server.login.clone();
        settings.directory = server.directory.clone();
        settings
    }
    /// The first thing to fix before an installation can start.
    pub fn problem(&self, ssh_port: u16) -> Option<&'static str> {
        let word = |s: &str, extra: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        // Values go into a single-quoted `.env` line as they are.
        let plain = |s: &str| !s.contains(['\'', '\\']) && !s.contains(char::is_control);
        let ports = [80, ssh_port, self.https_port];
        Some(if !word(&self.panel_login, "_-") {
            "The panel login may contain letters, digits, _ and -."
        } else if !plain(&self.panel_password) || !plain(&self.email) {
            "The panel password and email cannot contain ' or \\."
        } else if !self.domain.is_empty() && !valid_domain(&self.domain) {
            "Enter a domain like example.com, or leave it empty."
        } else if ports.contains(&0) || (1..3).any(|i| ports[..i].contains(&ports[i])) {
            "The SSH and panel ports must differ; port 80 is used for certificates."
        } else if !(self.directory.starts_with('/') || self.directory.starts_with("~/"))
            || self.directory.split('/').any(|part| part == "..")
            || !word(
                self.directory.trim_start_matches('~').trim_matches('/'),
                "/_-.",
            )
        {
            "The folder must start with ~/ or / and contain letters, digits, /, _, - and ."
        } else if !word(&self.image, "/._:@-") {
            "Enter a container image like ghcr.io/owner/homeport:latest."
        } else {
            return None;
        })
    }
}

/// The panel as the panel file tells it.
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub address: String,
    pub login: String,
    pub password: String,
    pub directory: String,
}
impl Panel {
    pub fn to_file(&self) -> String {
        format!(
            "# Homeport web panel. Homeport reads this file when it connects to this server.\n\
             Panel: {}\nLogin: {}\nPassword: {}\nFolder: {}\n",
            self.address, self.login, self.password, self.directory
        )
    }
    /// `None` without an address, login and password.
    pub fn parse(text: &str) -> Option<Self> {
        let value = |key: &str| {
            text.lines()
                .filter_map(|l| l.split_once(':'))
                .find(|(k, _)| k.trim().eq_ignore_ascii_case(key))
                .map(|(_, v)| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        Some(Self {
            address: value("Panel")?,
            login: value("Login")?,
            password: value("Password")?,
            directory: value("Folder").unwrap_or_default(),
        })
    }
}

fn valid_domain(d: &str) -> bool {
    d.len() <= 253
        && d.parse::<IpAddr>().is_err()
        && d.contains('.')
        && d.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        })
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::msg(e.to_string())
}
/// One shell argument.
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Where homeport's own SSH key is kept: in the plugin's data folder.
pub fn key_file(data_dir: &Path) -> PathBuf {
    data_dir.join("id_ed25519")
}
/// Homeport's own SSH key, made at the first connection.
fn key(cx: &Cx) -> Result<PrivateKey> {
    let path = key_file(&cx.data_dir());
    if let Ok(text) = std::fs::read_to_string(&path) {
        return PrivateKey::from_openssh(text).map_err(err);
    }
    let pair = Ed25519Keypair::from_seed(&random_bytes::<32>());
    let key = PrivateKey::new(KeypairData::Ed25519(pair), "homeport").map_err(err)?;
    let text = key.to_openssh(LineEnding::LF).map_err(err)?;
    let _ = std::fs::create_dir_all(cx.data_dir());
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
    std::io::Write::write_all(&mut file.open(&path).map_err(err)?, text.as_bytes()).map_err(err)?;
    Ok(key)
}
/// The key's `authorized_keys` line and the part that identifies it.
fn public_line(key: &PrivateKey) -> Result<(String, String)> {
    let line = key.public_key().to_openssh().map_err(err)?;
    let data = line.split(' ').nth(1).unwrap_or_default().to_owned();
    Ok((line, data))
}

/// The server's key is remembered at the first connection; a different key
/// later (also another algorithm) refuses the connection.
struct Trust {
    host: String,
    port: u16,
    known_hosts: PathBuf,
}
impl russh::client::Handler for Trust {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        server: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        use russh::keys::known_hosts::{known_host_keys_path, learn_known_hosts_path};
        let key = server.public_key();
        let known = known_host_keys_path(&self.host, self.port, &self.known_hosts)?;
        if known.is_empty() {
            learn_known_hosts_path(&self.host, self.port, &key, &self.known_hosts)?;
        }
        Ok(known.is_empty() || known.iter().any(|(_, saved)| *saved == key))
    }
}

/// What a remote command printed, and its exit code unless the connection
/// ended first.
struct Output {
    code: Option<u32>,
    text: String,
}

enum Auth<'a> {
    Password(&'a str),
    Key(&'a PrivateKey),
}

/// How the work that needs root runs.
enum Sudo {
    Root,
    NoPassword,
    Password(String),
    /// sudo asks for a password the user did not give.
    Missing,
}

struct Ssh {
    session: russh::client::Handle<Trust>,
    ip: IpAddr,
    sudo: Sudo,
}
impl Ssh {
    async fn open(cx: &Cx, host: &str, port: u16, login: &str, auth: Auth<'_>) -> Result<Self> {
        let host = host.trim();
        let unreachable =
            || err("The server does not answer over SSH. Check the address and the SSH port.");
        let connect = tokio::net::TcpStream::connect((host, port));
        let socket = tokio::time::timeout(Duration::from_secs(15), connect)
            .await
            .map_err(|_| unreachable())?
            .map_err(|_| unreachable())?;
        let ip = socket.peer_addr().map_err(err)?.ip();
        let trust = Trust {
            host: host.into(),
            port,
            known_hosts: cx.data_dir().join("known_hosts"),
        };
        let config = russh::client::Config {
            inactivity_timeout: Some(Duration::from_secs(1800)),
            ..Default::default()
        };
        let handshake = async {
            let mut session = russh::client::connect_stream(config.into(), socket, trust)
                .await
                .map_err(|e| match e {
                    russh::Error::UnknownKey => err("The server's SSH key has changed since the last connection. If the server was reinstalled, remove its line from known_hosts in the plugin's data folder."),
                    e => err(format!("The SSH connection failed: {e}")),
                })?;
            let login = login.trim();
            let accepted = match auth {
                Auth::Password(password) => session.authenticate_password(login, password).await,
                Auth::Key(key) => {
                    let key = PrivateKeyWithHashAlg::new(Arc::new(key.clone()), None);
                    session.authenticate_publickey(login, key).await
                }
            };
            if !accepted.map_err(err)?.success() {
                match auth {
                    Auth::Password(_) => bail!("The server refused the SSH login or password."),
                    Auth::Key(_) => bail!(
                        "The server no longer accepts Homeport's key. Connect the server again with its SSH password."
                    ),
                }
            }
            Ok(session)
        };
        let session = tokio::time::timeout(Duration::from_secs(45), handshake)
            .await
            .map_err(|_| err("The SSH connection timed out."))??;
        let mut ssh = Self {
            session,
            ip,
            sudo: Sudo::Root,
        };
        if ssh.sh("id -u").await?.trim() != "0" {
            let free = ssh.run("sudo -n true", b"", |_| {}).await?;
            ssh.sudo = if free.code == Some(0) {
                Sudo::NoPassword
            } else {
                Sudo::Missing
            };
        }
        Ok(ssh)
    }
    /// Sign in with homeport's key; `sudo` is the password for sudo, if needed.
    async fn key(cx: &Cx, server: &Server, sudo: &str) -> Result<Self> {
        let key = key(cx)?;
        let mut ssh = Self::open(
            cx,
            &server.host,
            server.ssh_port,
            &server.ssh_login,
            Auth::Key(&key),
        )
        .await?;
        if matches!(ssh.sudo, Sudo::Missing) && !sudo.is_empty() {
            let line = format!("{sudo}\n");
            let tried = ssh.run("sudo -S -p '' true 2>/dev/null", line.as_bytes(), |_| {});
            if tried.await?.code != Some(0) {
                bail!("The server refused the password for sudo.");
            }
            ssh.sudo = Sudo::Password(line);
        }
        Ok(ssh)
    }

    /// `command` run as root without asking; `None` when sudo needs a password.
    fn as_root(&self, command: &str) -> Option<String> {
        match self.sudo {
            Sudo::Root => Some(command.to_owned()),
            Sudo::NoPassword => Some(format!("sudo -n sh -c {}", quote(command))),
            Sudo::Password(_) | Sudo::Missing => None,
        }
    }
    /// Run `command`, feeding it `input`; `line` sees every finished output line.
    async fn run(&self, command: &str, input: &[u8], mut line: impl FnMut(&str)) -> Result<Output> {
        let mut channel = self.session.channel_open_session().await.map_err(err)?;
        channel.exec(true, command).await.map_err(err)?;
        if !input.is_empty() {
            channel.data(input).await.map_err(err)?;
        }
        channel.eof().await.map_err(err)?;
        let (mut text, mut code, mut seen) = (String::new(), None, 0);
        while let Some(message) = channel.wait().await {
            match message {
                russh::ChannelMsg::Data { data } | russh::ChannelMsg::ExtendedData { data, .. } => {
                    text.push_str(&String::from_utf8_lossy(&data));
                    while let Some(end) = text[seen..].find('\n') {
                        // A progress line redrawn with \r: its last state.
                        line(
                            text[seen..seen + end]
                                .rsplit('\r')
                                .next()
                                .unwrap_or_default(),
                        );
                        seen += end + 1;
                    }
                }
                russh::ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
        }
        if seen < text.len() {
            line(text[seen..].rsplit('\r').next().unwrap_or_default());
        }
        Ok(Output { code, text })
    }
    /// Run a short command that must succeed.
    async fn sh(&self, command: &str) -> Result<String> {
        self.send(command, b"").await
    }
    async fn send(&self, command: &str, input: &[u8]) -> Result<String> {
        let run = self.run(command, input, |_| {});
        let out = tokio::time::timeout(Duration::from_secs(60), run)
            .await
            .map_err(|_| err("The server stopped answering."))??;
        if out.code != Some(0) {
            bail!("The server could not run `{command}`: {}", out.text.trim());
        }
        Ok(out.text)
    }
    /// `~/…` in the home folder of the SSH login (not root's when using sudo).
    async fn resolve(&self, directory: &str) -> Result<String> {
        match directory.strip_prefix('~') {
            Some(rest) => Ok(format!("{}{rest}", self.home().await?)),
            None => Ok(directory.to_owned()),
        }
    }
    /// The home folder of the SSH login: what the installer keeps on the
    /// server goes there.
    async fn home(&self) -> Result<String> {
        let home = self.sh("printf '%s' \"$HOME\"").await?;
        let home = home.trim().trim_end_matches('/');
        if !home.starts_with('/') || home.contains(['\'', '\n']) {
            bail!("The server did not tell the home folder of the SSH login.");
        }
        Ok(home.to_owned())
    }
    async fn upload(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let command = format!("umask 077; cat > {}", quote(path));
        self.send(&command, bytes).await.map(drop)
    }
    /// The panel file, if homeport is installed.
    async fn panel(&self) -> Result<Option<Panel>> {
        let command = format!("cat ~/{PANEL_FILE} 2>/dev/null || true");
        Ok(Panel::parse(&self.sh(&command).await?))
    }
    /// A new temporary folder for the uploaded files.
    async fn stage(&self) -> Result<String> {
        let stage = self.sh("umask 077; mktemp -d /tmp/homeport-install.XXXXXX");
        Ok(stage.await?.trim().to_owned())
    }
    /// Run the uploaded `script` as root; `line` sees its output. Without an
    /// exit code (Stop, timeout, lost connection) the outcome on the server
    /// is unknown, and that is an error.
    async fn run_as_root(&self, cx: &Cx, script: &str, line: impl FnMut(&str)) -> Result<Output> {
        let (command, password) = match &self.sudo {
            Sudo::Root => (format!("sh {script}"), &b""[..]),
            Sudo::NoPassword => (format!("sudo -n sh {script}"), &b""[..]),
            Sudo::Password(password) => (format!("sudo -S -p '' sh {script}"), password.as_bytes()),
            Sudo::Missing => bail!("Enter the password for sudo."),
        };
        let run = self.run(&command, password, line);
        match tokio::time::timeout(Duration::from_secs(1800), cx.or_cancel(run)).await {
            Ok(Ok(Ok(out @ Output { code: Some(_), .. }))) => Ok(out),
            Ok(Err(cancelled)) => Err(cancelled),
            _ => bail!("The connection to the server was lost before the work was done."),
        }
    }
    /// Upload `body` as a script, run it as root with the progress shown and
    /// remove it again. A failure explains itself with `what`.
    async fn script(&self, progress: &Progress<'_>, what: &str, body: String) -> Result<()> {
        if matches!(self.sudo, Sudo::Missing) {
            bail!("Enter the password for sudo.");
        }
        let stage = self.stage().await?;
        let path = format!("{stage}/script.sh");
        let result = async {
            let body = format!("STAGE={}\n{body}", quote(&stage));
            self.upload(&path, body.as_bytes()).await?;
            progress.cx.checkpoint()?;
            let mut reason = None;
            let run = self.run_as_root(progress.cx, &path, |l| progress.line(l, &mut reason));
            let out = run.await?;
            if out.code != Some(0) {
                let why = reason.unwrap_or_else(|| last_lines(what, &out.text, progress.secrets));
                bail!("{why}");
            }
            Ok(())
        }
        .await;
        let _ = self.sh(&format!("rm -rf -- {}", quote(&stage))).await;
        result
    }
}

/// An action's progress: the job's steps, the page's status and the log.
struct Progress<'a> {
    cx: &'a Cx,
    host: String,
    secrets: &'a [String],
    steps: u64,
    done: std::sync::atomic::AtomicU64,
}
impl<'a> Progress<'a> {
    fn new(cx: &'a Cx, host: &str, secrets: &'a [String], steps: u64) -> Self {
        Self {
            cx,
            host: host.trim().to_owned(),
            secrets,
            steps,
            done: 0.into(),
        }
    }
    fn step(&self, text: &str) {
        let done = (self.done).fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        self.cx.progress(done.min(self.steps - 1), Some(self.steps));
        self.cx.update_live(&ACTIVITY, |a| {
            a.status = Status::Working(format!("{text}…"))
        });
        log(self.cx, self.secrets, &format!("▸ {text}"));
        info!("{}: {text}", self.host);
    }
    /// A line of a script: `::step::` is progress, `::error::` the reason
    /// it failed, the rest goes to the log.
    fn line(&self, line: &str, reason: &mut Option<String>) {
        if let Some(text) = line.strip_prefix("::step::") {
            self.step(text);
        } else if let Some(text) = line.strip_prefix("::error::") {
            *reason = Some(text.to_owned());
        } else if !line.trim().is_empty() {
            log(self.cx, self.secrets, line);
        }
    }
    fn done(&self, text: &str) {
        info!("{}: {text}", self.host);
        log(self.cx, self.secrets, &format!("✔ {text}"));
    }
}

/// `<address>.sslip.io` resolves to the address itself: a certificate
/// without owning a domain.
fn domain(settings: &Settings, ip: IpAddr) -> Result<String> {
    if !settings.domain.is_empty() {
        return Ok(settings.domain.clone());
    }
    match ip {
        IpAddr::V4(v4) if !(v4.is_private() || v4.is_loopback() || v4.is_link_local()) => {
            Ok(format!("{}.sslip.io", v4.to_string().replace('.', "-")))
        }
        _ => bail!("Without a domain the server needs a public IPv4 address. Enter your domain."),
    }
}

/// Lines kept in the log.
const LOG_LINES: usize = 400;

/// Add a line to the log, with the secrets hidden.
pub fn log(cx: &Cx, secrets: &[String], line: &str) {
    let mut line = line.trim_end().to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            line = line.replace(secret.as_str(), "•••");
        }
    }
    cx.update_live(&LOG, |l| {
        l.push(line);
        let extra = l.len().saturating_sub(LOG_LINES);
        l.drain(..extra);
    });
}

/// Sign in with the password, let homeport's key in, read what the server
/// has and save it as a server (the same one when connected before).
pub async fn connect(cx: &Cx, access: &Access) -> Result<Id> {
    if let Some(problem) = access.problem() {
        bail!("{problem}");
    }
    let secrets = access.secrets();
    let progress = Progress::new(cx, &access.host, &secrets, 4);
    let (host, login) = (access.host.trim(), access.login.trim());
    progress.step(&format!("Connecting to {host}:{}", access.ssh_port));
    let earlier = (cx.get(&SERVERS).iter())
        .find(|s| s.host == host && s.ssh_login == login)
        .cloned();
    if access.password.is_empty() && earlier.is_none() {
        bail!("Enter the SSH password.");
    }
    let key = key(cx)?;
    let auth = match access.password.as_str() {
        "" => Auth::Key(&key),
        password => Auth::Password(password),
    };
    let ssh = Ssh::open(cx, host, access.ssh_port, login, auth).await?;
    log(cx, &secrets, &format!("Signed in as {login}"));
    if !access.password.is_empty() {
        progress.step("Letting Homeport's key in");
        let (line, data) = public_line(&key)?;
        ssh.sh(&format!(
            "umask 077; mkdir -p ~/.ssh; touch ~/.ssh/authorized_keys; \
             grep -qF {} ~/.ssh/authorized_keys || echo {} >> ~/.ssh/authorized_keys",
            quote(&data),
            quote(&line)
        ))
        .await?;
        // Some servers take no keys at all, or not from this file.
        Ssh::open(cx, host, access.ssh_port, login, Auth::Key(&key))
            .await
            .map_err(|_| {
                err("The server does not accept SSH keys for this login. Turn on PubkeyAuthentication in its SSH settings.")
            })?;
    }
    progress.step("Reading the server");
    let system = ssh
        .sh(". /etc/os-release 2>/dev/null; printf '%s on %s' \"${PRETTY_NAME:-$(uname -s)}\" \"$(uname -m)\"")
        .await?;
    let panel = ssh.panel().await?;
    let mut server = Server {
        id: earlier.as_ref().map_or_else(Id::random, |s| s.id),
        host: host.into(),
        ssh_port: access.ssh_port,
        ssh_login: login.into(),
        system: system.trim().into(),
        sudo_password: matches!(ssh.sudo, Sudo::Missing),
        panel: String::new(),
        login: String::new(),
        password: String::new(),
        directory: String::new(),
        ready: false,
    };
    match (panel, earlier) {
        (Some(panel), _) => adopt(&mut server, panel),
        // An installation that was interrupted stays to be finished.
        (None, Some(earlier)) if earlier.installed() && !earlier.ready => {
            server.panel = earlier.panel;
            server.login = earlier.login;
            server.password = earlier.password;
            server.directory = earlier.directory;
        }
        _ => {}
    }
    let id = server.id;
    progress.done(&match server.installed() {
        true => format!(
            "Connected. Homeport is installed; the panel is at {}",
            server.panel
        ),
        false => "Connected. Homeport is not installed yet.".to_owned(),
    });
    save(cx, server).await?;
    Ok(id)
}

fn adopt(server: &mut Server, panel: Panel) {
    server.panel = panel.address;
    server.login = panel.login;
    server.password = panel.password;
    server.directory = panel.directory;
    server.ready = true;
}

/// The panel file again (another homeport may have installed or cleaned up
/// meanwhile), then whether the panel answers and which image it runs.
/// `None`: nothing is installed.
pub async fn check(cx: &Cx, server: &Server) -> Result<Option<(Health, Option<Release>)>> {
    let ssh = Ssh::key(cx, server, "").await?;
    let mut fresh = server.clone();
    match ssh.panel().await? {
        Some(panel) => adopt(&mut fresh, panel),
        // An unfinished installation has no file yet.
        None if !server.ready => {}
        None => {
            fresh.panel.clear();
            fresh.login.clear();
            fresh.password.clear();
            fresh.directory.clear();
            fresh.ready = false;
        }
    }
    if fresh != *server {
        save(cx, fresh.clone()).await?;
    }
    if !fresh.installed() || !fresh.ready {
        return Ok(None);
    }
    let out = ssh
        .run(
            &format!(
                "curl -sS -o /dev/null -w '%{{http_code}}' --connect-timeout 10 --max-time 20 {}",
                quote(&fresh.panel)
            ),
            b"",
            |_| {},
        )
        .await?;
    // The panel turns away visitors who are not signed in.
    let health = match out.text.trim() {
        "401" | "302" | "303" => Health::Works,
        _ if out.code != Some(0) => Health::Fails("The panel does not answer.".into()),
        code => Health::Fails(format!("The panel answers with HTTP {code}.")),
    };
    Ok(Some((health, release(&ssh, &fresh.directory).await)))
}

/// The image the panel runs, and whether its registry has a newer one
/// under the same tag. `None` when that cannot be read: sudo asks for a
/// password, the services do not run.
async fn release(ssh: &Ssh, directory: &str) -> Option<Release> {
    if directory.is_empty() {
        return None;
    }
    let script = format!(
        "cd {} || exit 1\n\
         c=$(docker compose ps -q homeport 2>/dev/null)\n\
         [ -n \"$c\" ] || exit 1\n\
         ref=$(docker inspect -f '{{{{.Config.Image}}}}' \"$c\")\n\
         id=$(docker inspect -f '{{{{.Image}}}}' \"$c\") || exit 1\n\
         echo \"image=$ref\"\n\
         docker image inspect -f 'created={{{{.Created}}}}{{{{range .RepoDigests}}}} digest={{{{.}}}}{{{{end}}}}' \"$id\" || exit 1\n\
         echo \"version=$(docker exec \"$c\" /app/homeport --version 2>/dev/null)\"\n",
        quote(directory)
    );
    // A stuck container must not hold the check.
    let command = ssh.as_root(&script)?;
    let run = ssh.run(&command, b"", |_| {});
    let out = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .ok()?
        .ok()?;
    if out.code != Some(0) {
        return None;
    }
    let running = Running::parse(&out.text)?;
    // A locally built image has no digest to compare.
    let update = match running.digests.is_empty() {
        true => false,
        false => published(ssh, &running.image)
            .await
            .is_some_and(|d| !running.digests.contains(&d)),
    };
    Some(Release {
        version: running.version(),
        update,
    })
}

/// What `release` reads from Docker on the server.
#[derive(Debug, Default, PartialEq)]
struct Running {
    image: String,
    /// "2026-11-20T12:00:00.123Z".
    created: String,
    /// The digests the image was pulled at.
    digests: Vec<String>,
    /// Empty when the image's homeport does not tell it.
    version: String,
}
impl Running {
    fn parse(text: &str) -> Option<Self> {
        let mut running = Self::default();
        for line in text.lines() {
            if let Some(image) = line.strip_prefix("image=") {
                running.image = image.trim().into();
            } else if let Some(version) = line.strip_prefix("version=") {
                let version = version.trim().strip_prefix("homeport ").unwrap_or_default();
                running.version = version.into();
            } else {
                for word in line.split_whitespace() {
                    if let Some(created) = word.strip_prefix("created=") {
                        running.created = created.into();
                    } else if let Some(digest) = word.strip_prefix("digest=") {
                        running
                            .digests
                            .push(digest.rsplit('@').next().unwrap_or_default().into());
                    }
                }
            }
        }
        (!running.image.is_empty()).then_some(running)
    }
    /// "0.2.0 · built 2026-11-20".
    fn version(&self) -> String {
        let built = self.created.get(..10).unwrap_or_default();
        match (self.version.as_str(), built) {
            (version, "") => version.into(),
            ("", built) => format!("Built {built}"),
            (version, built) => format!("{version} · built {built}"),
        }
    }
}

/// The manifest types a multi-platform image is published with.
const MANIFESTS: &str = "application/vnd.oci.image.index.v1+json, \
    application/vnd.docker.distribution.manifest.list.v2+json, \
    application/vnd.oci.image.manifest.v1+json, \
    application/vnd.docker.distribution.manifest.v2+json";

/// The digest the registry publishes `image` at now, asked from the server
/// (it is the one that downloads updates).
async fn published(ssh: &Ssh, image: &str) -> Option<String> {
    let (host, repository, tag) = reference(image)?;
    let url = format!("https://{host}/v2/{repository}/manifests/{tag}");
    let curl = "curl -sS --connect-timeout 10 --max-time 20";
    let head = |token: Option<&str>| {
        let auth = token.map_or_else(String::new, |t| {
            format!(" -H {}", quote(&format!("Authorization: Bearer {t}")))
        });
        format!(
            "{curl} -I -H {}{auth} {}",
            quote(&format!("Accept: {MANIFESTS}")),
            quote(&url)
        )
    };
    let mut headers = ssh.sh(&head(None)).await.ok()?;
    // Public images too want an anonymous token first.
    if status(&headers) == Some(401) {
        let challenge = header(&headers, "www-authenticate")?;
        let scope = param(challenge, "scope")
            .map_or_else(|| format!("repository:{repository}:pull"), str::to_owned);
        let mut command = format!("{curl} --get {}", quote(param(challenge, "realm")?));
        command += &format!(" --data-urlencode {}", quote(&format!("scope={scope}")));
        if let Some(service) = param(challenge, "service") {
            command += &format!(" --data-urlencode {}", quote(&format!("service={service}")));
        }
        let token = token(&ssh.sh(&command).await.ok()?)?;
        headers = ssh.sh(&head(Some(&token))).await.ok()?;
    }
    (status(&headers) == Some(200))
        .then(|| header(&headers, "docker-content-digest").map(str::to_owned))
        .flatten()
}

/// The registry, repository and tag of an image reference; `None` for one
/// pinned to a digest, which has no updates.
fn reference(image: &str) -> Option<(String, String, String)> {
    if image.contains('@') {
        return None;
    }
    let (name, tag) = match image.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name, tag),
        _ => (image, "latest"),
    };
    let (host, repository) = match name.split_once('/') {
        Some(("docker.io", repository)) => ("registry-1.docker.io", repository.to_owned()),
        Some((host, repository)) if host.contains(['.', ':']) || host == "localhost" => {
            (host, repository.to_owned())
        }
        Some(_) => ("registry-1.docker.io", name.to_owned()),
        None => ("registry-1.docker.io", format!("library/{name}")),
    };
    // Docker Hub's official images also live under library/.
    let repository = match (host, repository.contains('/')) {
        ("registry-1.docker.io", false) => format!("library/{repository}"),
        _ => repository,
    };
    Some((host.into(), repository, tag.into()))
}

/// The status code of the last response in curl's headers.
fn status(headers: &str) -> Option<u16> {
    let line = headers.lines().rfind(|l| l.starts_with("HTTP/"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}
fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}
/// `key="value"` of a `WWW-Authenticate: Bearer …` challenge.
fn param<'a>(challenge: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("{key}=\"");
    let start = challenge
        .match_indices(&pattern)
        .find(|(i, _)| *i == 0 || challenge[..*i].ends_with([' ', ',']))?
        .0
        + pattern.len();
    let len = challenge[start..].find('"')?;
    Some(&challenge[start..start + len])
}
/// The `"token"` of a registry's token answer.
fn token(json: &str) -> Option<String> {
    let rest = &json[json.find("\"token\"")? + 7..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

pub async fn install(cx: &Cx, server: &Server, settings: &Settings, sudo: &str) -> Result<()> {
    if let Some(problem) = settings.problem(server.ssh_port) {
        bail!("{problem}");
    }
    let secrets = [sudo.to_owned(), settings.panel_password.clone()];
    let progress = Progress::new(cx, &server.host, &secrets, 8);
    progress.step(&format!("Connecting to {}", server.host));
    let ssh = Ssh::key(cx, server, sudo).await?;
    if matches!(ssh.sudo, Sudo::Missing) {
        bail!("Enter the password for sudo.");
    }
    if settings.disable_ipv6 && ssh.ip.is_ipv6() {
        bail!(
            "This connection uses IPv6. Connect the server by its IPv4 address, or leave IPv6 on."
        );
    }
    let home = ssh.home().await?;
    let directory = ssh.resolve(&settings.directory).await?;
    let domain = domain(settings, ssh.ip)?;
    let (panel, auth) = (format!("panel.{domain}"), format!("auth.{domain}"));
    // Installing again updates the installation, with the same panel
    // password unless a new one is given.
    let mut record = Server {
        panel: format!("https://{panel}:{}", settings.https_port),
        login: settings.panel_login.clone(),
        password: match settings.panel_password.as_str() {
            "" if server.installed() => server.password.clone(),
            "" => format!("{}{}", Id::random(), Id::random()),
            chosen => chosen.into(),
        },
        directory: directory.clone(),
        ready: false,
        ..server.clone()
    };
    let env: String = [
        ("HOMEPORT_PANEL_DOMAIN", panel.as_str()),
        ("HOMEPORT_AUTH_DOMAIN", auth.as_str()),
        ("DATA_DIR", &format!("{directory}/data")),
        ("TINYAUTH_USERNAME", &record.login),
        ("TINYAUTH_PASSWORD", &record.password),
        ("HTTPS_PORT", &settings.https_port.to_string()),
        ("HOMEPORT_IMAGE", &settings.image),
        ("ACME_EMAIL", &settings.email),
    ]
    .iter()
    .map(|(key, value)| format!("{key}='{value}'\n"))
    .collect();

    progress.step(&format!("Uploading the installation to {directory}"));
    let stage = ssh.stage().await?;
    let stage = stage.as_str();
    let script = format!(
        "DEST={} STAGE={} HOME_DIR={} PANEL={} AUTH={} HTTPS_PORT={} DISABLE_IPV6={}\n{}",
        quote(&directory),
        quote(stage),
        quote(&home),
        quote(&record.panel),
        quote(&format!("https://{auth}:{}", settings.https_port)),
        settings.https_port,
        u8::from(settings.disable_ipv6),
        include_str!("install.sh"),
    );
    let files: [(&str, &[u8]); 4] = [
        (
            "compose.yaml",
            include_bytes!("../../../docker/compose.yaml"),
        ),
        ("Caddyfile", include_bytes!("../../../docker/Caddyfile")),
        (".env", env.as_bytes()),
        ("install.sh", script.as_bytes()),
    ];
    let result = async {
        for (name, bytes) in files {
            ssh.upload(&format!("{stage}/{name}"), bytes).await?;
        }
        cx.checkpoint()?;
        // Saved before the server changes: its password is never lost.
        save(cx, record.clone()).await?;
        let (mut reason, mut created) = (None, false);
        let script = format!("{stage}/install.sh");
        // An unknown outcome on the server keeps the record as it is.
        let outcome = ssh.run_as_root(cx, &script, |line| match line {
            "::created::" => created = true,
            line => progress.line(line, &mut reason),
        });
        match outcome.await? {
            Output { code: Some(0), .. } => {
                let file = Panel {
                    address: record.panel.clone(),
                    login: record.login.clone(),
                    password: record.password.clone(),
                    directory: directory.clone(),
                };
                ssh.upload(&format!("{home}/{PANEL_FILE}"), file.to_file().as_bytes())
                    .await?;
                progress.done(&format!("Installed. The panel is at {}", record.panel));
                record.ready = true;
                save(cx, record.clone()).await
            }
            // The script stopped with a reason; what it set up stays for the next run.
            Output { text, .. } => {
                if !created {
                    // Nothing of this installation is on the server: as before it.
                    save(cx, server.clone()).await?;
                }
                Err(err(reason.unwrap_or_else(|| {
                    last_lines("The installation", &text, &secrets)
                })))
            }
        }
    }
    .await;
    let _ = ssh.sh(&format!("rm -rf -- {}", quote(stage))).await;
    result
}

pub async fn restart(cx: &Cx, server: &Server, sudo: &str) -> Result<()> {
    if !server.installed() {
        bail!("Homeport is not installed on this server.");
    }
    let secrets = [sudo.to_owned()];
    let progress = Progress::new(cx, &server.host, &secrets, 3);
    progress.step(&format!("Connecting to {}", server.host));
    let ssh = Ssh::key(cx, server, sudo).await?;
    let body = format!(
        "set -eu\ncd {}\necho '::step::Restarting the services'\ndocker compose restart 2>&1\n",
        quote(&server.directory)
    );
    ssh.script(&progress, "Restarting", body).await?;
    progress.done("Restarted");
    Ok(())
}

/// The last lines of homeport's services, into the log.
pub async fn logs(cx: &Cx, server: &Server, sudo: &str) -> Result<()> {
    if !server.installed() || server.directory.is_empty() {
        bail!("Homeport is not installed on this server.");
    }
    let secrets = [sudo.to_owned(), server.password.clone()];
    let progress = Progress::new(cx, &server.host, &secrets, 3);
    progress.step(&format!("Connecting to {}", server.host));
    let ssh = Ssh::key(cx, server, sudo).await?;
    let body = format!(
        "set -eu\ncd {}\necho '::step::Reading the logs'\n\
         docker compose logs --no-color --tail {LOG_TAIL} 2>&1\n",
        quote(&server.directory)
    );
    ssh.script(&progress, "Reading the logs", body).await?;
    progress.done(&format!("The last {LOG_TAIL} lines of each service"));
    Ok(())
}
/// Lines of each service in the logs; the three of them fit in the log.
const LOG_TAIL: usize = 100;

/// Remove homeport and what its installer added, the panel file and
/// homeport's key; then the server leaves the list.
pub async fn clean(cx: &Cx, server: &Server, sudo: &str) -> Result<()> {
    let secrets = [sudo.to_owned()];
    let progress = Progress::new(cx, &server.host, &secrets, 8);
    progress.step(&format!("Connecting to {}", server.host));
    let ssh = Ssh::key(cx, server, sudo).await?;
    let home = ssh.home().await?;
    let directory = match server.directory.as_str() {
        "" => ssh.resolve(&Settings::default().directory).await?,
        directory => directory.to_owned(),
    };
    let body = format!(
        "DEST={} HOME_DIR={}\n{}",
        quote(&directory),
        quote(&home),
        include_str!("uninstall.sh"),
    );
    ssh.script(&progress, "Cleaning up", body).await?;
    progress.step("Removing Homeport's key");
    let (_, data) = public_line(&key(cx)?)?;
    ssh.sh(&format!(
        "f=~/.ssh/authorized_keys; if [ -f \"$f\" ]; then grep -vF {} \"$f\" > \"$f.homeport\" || true; \
         cat \"$f.homeport\" > \"$f\"; rm -f \"$f.homeport\"; fi",
        quote(&data)
    ))
    .await?;
    progress.done(&format!("Cleaned up {}", server.host));
    let id = server.id;
    cx.update_live(&crate::HEALTH, |h| drop(h.remove(&id)));
    cx.update_live(&RELEASES, |r| drop(r.remove(&id)));
    cx.write(move |w| {
        w.update(&SERVERS, |all| all.retain(|s| s.id != id));
        Ok(())
    })
    .await
}

async fn save(cx: &Cx, record: Server) -> Result<()> {
    cx.write(move |w| {
        w.update(&SERVERS, |all| {
            match all.iter_mut().find(|s| s.id == record.id) {
                Some(saved) => *saved = record,
                None => all.push(record),
            }
        });
        Ok(())
    })
    .await
}

/// The end of the server's output for a failure the script did not explain.
fn last_lines(what: &str, text: &str, secrets: &[String]) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("::"))
        .collect();
    let mut tail = lines[lines.len().saturating_sub(8)..].join("\n");
    for secret in secrets.iter().filter(|s| !s.is_empty()) {
        tail = tail.replace(secret.as_str(), "•••");
    }
    format!("{what} failed on the server:\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsafe_settings_and_port_collisions_are_rejected() {
        let ok = Settings::default();
        assert_eq!(ok.problem(22), None);
        let rejected = |change: fn(&mut Settings)| {
            let mut s = Settings::default();
            change(&mut s);
            s.problem(22).is_some()
        };
        for directory in [
            "/",
            "~",
            "~/",
            "/srv/../etc",
            "/srv/a;id",
            "relative",
            "~user/x",
            "/srv/a b",
        ] {
            let s = Settings {
                directory: directory.into(),
                ..Settings::default()
            };
            assert!(s.problem(22).is_some(), "{directory}");
        }
        assert_eq!(
            ok.problem(8443),
            Some("The SSH and panel ports must differ; port 80 is used for certificates.")
        );
        assert!(rejected(|s| s.https_port = 80));
        assert!(rejected(|s| s.domain = "example.com;id".into()));
        assert!(rejected(|s| s.panel_password = "a'b".into()));
        assert!(rejected(
            |s| s.panel_password = "a\nHOMEPORT_IMAGE=evil".into()
        ));
        assert!(rejected(|s| s.panel_login = "a b".into()));
        assert!(rejected(|s| s.image = "image; rm".into()));
        assert!(!rejected(|s| s.panel_password = "p@$$ w0rd \"!".into()));
        assert!(!rejected(|s| s.directory = "/opt/homeport".into()));
        assert!(Access::default().problem().is_some());
        let access = Access {
            host: "203.0.113.1".into(),
            ..Access::default()
        };
        assert_eq!(access.problem(), None, "a key needs no password");
    }
    #[test]
    fn the_panel_file_reads_back_what_was_written() {
        let panel = Panel {
            address: "https://panel.203-0-113-7.sslip.io:8443".into(),
            login: "admin".into(),
            password: "p@ss: word".into(),
            directory: "/root/homeport".into(),
        };
        assert_eq!(Panel::parse(&panel.to_file()), Some(panel));
        assert_eq!(Panel::parse(""), None);
        assert_eq!(Panel::parse("Panel: https://x\nLogin: admin\n"), None);
    }
    #[test]
    fn an_installed_server_is_updated_with_its_own_settings() {
        let server = Server {
            id: Id::random(),
            host: "203.0.113.7".into(),
            ssh_port: 22,
            ssh_login: "root".into(),
            system: String::new(),
            sudo_password: false,
            panel: "https://panel.example.com:9443".into(),
            login: "me".into(),
            password: "secret".into(),
            directory: "/srv/homeport".into(),
            ready: true,
        };
        let s = Settings::of(&server);
        assert_eq!(
            (s.domain.as_str(), s.https_port, s.panel_login.as_str()),
            ("example.com", 9443, "me")
        );
        assert_eq!(s.directory, "/srv/homeport");
        let sslip = Server {
            panel: "https://panel.203-0-113-7.sslip.io:8443".into(),
            ..server
        };
        assert_eq!(Settings::of(&sslip).domain, "");
    }
    #[test]
    fn the_automatic_domain_needs_a_public_ipv4_address() {
        let s = Settings::default();
        assert_eq!(
            domain(&s, "203.0.113.7".parse().unwrap()).unwrap(),
            "203-0-113-7.sslip.io"
        );
        assert!(domain(&s, "192.168.1.5".parse().unwrap()).is_err());
        assert!(domain(&s, "2001:db8::1".parse().unwrap()).is_err());
        let own = Settings {
            domain: "example.com".into(),
            ..s
        };
        assert_eq!(
            domain(&own, "2001:db8::1".parse().unwrap()).unwrap(),
            "example.com"
        );
    }
    #[test]
    fn a_failure_shows_the_end_of_the_output_without_the_password() {
        let text = "::step::Checking\nok\n\nE: password broke apt\n";
        assert_eq!(
            last_lines("The installation", text, &["password".into()]),
            "The installation failed on the server:\nok\nE: ••• broke apt"
        );
    }
    #[test]
    fn image_references_name_their_registry() {
        let r = |image: &str| reference(image).map(|(h, r, t)| format!("{h} {r} {t}"));
        assert_eq!(
            r("ghcr.io/s-vin/homeport:latest").as_deref(),
            Some("ghcr.io s-vin/homeport latest")
        );
        assert_eq!(
            r("nginx").as_deref(),
            Some("registry-1.docker.io library/nginx latest")
        );
        assert_eq!(
            r("docker.io/nginx:1.27").as_deref(),
            Some("registry-1.docker.io library/nginx 1.27")
        );
        assert_eq!(
            r("owner/app:v2").as_deref(),
            Some("registry-1.docker.io owner/app v2")
        );
        assert_eq!(
            r("localhost:5000/app").as_deref(),
            Some("localhost:5000 app latest")
        );
        assert_eq!(r("ghcr.io/a/b@sha256:00"), None);
    }
    #[test]
    fn the_registry_answers_are_read() {
        let first = "HTTP/1.1 200 Connection established\r\n\r\nHTTP/2 401 \r\ncontent-type: application/json\r\n\
            www-authenticate: Bearer realm=\"https://ghcr.io/token\",service=\"ghcr.io\",scope=\"repository:s-vin/homeport:pull\"\r\n";
        assert_eq!(status(first), Some(401));
        let challenge = header(first, "WWW-Authenticate").unwrap();
        assert_eq!(param(challenge, "realm"), Some("https://ghcr.io/token"));
        assert_eq!(param(challenge, "service"), Some("ghcr.io"));
        assert_eq!(
            param(challenge, "scope"),
            Some("repository:s-vin/homeport:pull")
        );
        assert_eq!(
            token(r#"{"access_token": "x", "token" : "abc.def"}"#).as_deref(),
            Some("abc.def")
        );
        let ok = "HTTP/1.1 200 OK\r\nDocker-Content-Digest: sha256:12ab\r\n";
        assert_eq!(status(ok), Some(200));
        assert_eq!(header(ok, "docker-content-digest"), Some("sha256:12ab"));
    }
    #[test]
    fn the_running_image_is_read_from_docker() {
        let text = "image=ghcr.io/s-vin/homeport:latest\n\
            created=2026-11-20T12:00:00.5Z digest=ghcr.io/s-vin/homeport@sha256:aa\n\
            version=homeport 0.2.0\n";
        let running = Running::parse(text).unwrap();
        assert_eq!(running.digests, ["sha256:aa"]);
        assert_eq!(running.version(), "0.2.0 · built 2026-11-20");
        let old = Running::parse("image=x\ncreated=2026-11-20T12:00:00Z\nversion=\n").unwrap();
        assert_eq!(old.version(), "Built 2026-11-20");
        assert_eq!(Running::parse("version=\n"), None);
    }
    #[test]
    #[cfg(unix)]
    fn shell_quoting_preserves_metacharacters() {
        let secret = "a' $(touch /tmp/not-created) $HOME \\ b";
        let output = std::process::Command::new("sh")
            .args(["-c", &format!("printf %s {}", quote(secret))])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8(output.stdout).unwrap(), secret);
    }
}
