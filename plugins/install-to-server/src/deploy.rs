//! One installation: connect over SSH, upload the Compose stack with a fresh
//! server profile, run `install.sh` as root and follow its progress.
use crate::{DEPLOYMENTS, Deployment, STATUS, Status};
use rproxy_kernel::{configuration::Profile, prelude::*};
use std::{net::IpAddr, path::PathBuf, time::Duration};

/// What the user chooses. Everything but the SSH access has a working default.
#[derive(Clone, PartialEq)]
pub struct Options {
    pub host: String,
    pub ssh_port: u16,
    pub login: String,
    pub password: String,
    pub panel_login: String,
    /// Empty: a random one is generated.
    pub panel_password: String,
    pub disable_ipv6: bool,
    /// Empty: `<server address>.sslip.io`, which needs no DNS setup.
    pub domain: String,
    pub https_port: u16,
    pub directory: String,
    pub image: String,
    /// Optional Let's Encrypt contact.
    pub email: String,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            host: String::new(),
            ssh_port: 22,
            login: "root".into(),
            password: String::new(),
            panel_login: "admin".into(),
            panel_password: String::new(),
            disable_ipv6: true,
            domain: String::new(),
            https_port: 8443,
            directory: "/srv/rproxy".into(),
            image: "ghcr.io/s-vin/rproxy:latest".into(),
            email: String::new(),
        }
    }
}
impl Options {
    /// The first thing to fix before an installation can start.
    pub fn problem(&self) -> Option<&'static str> {
        let word = |s: &str, extra: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        // Values go into a single-quoted `.env` line as they are.
        let plain = |s: &str| !s.contains(['\'', '\\']) && !s.contains(char::is_control);
        let ports = [80, self.ssh_port, self.https_port];
        Some(if self.host.trim().is_empty() {
            "Enter the server address."
        } else if self.login.trim().is_empty() || self.password.is_empty() {
            "Enter the SSH login and password."
        } else if self.password.contains(['\n', '\r']) {
            "The SSH password cannot contain line breaks."
        } else if !word(&self.panel_login, "_-") {
            "The panel login may contain letters, digits, _ and -."
        } else if !plain(&self.panel_password) || !plain(&self.email) {
            "The panel password and email cannot contain ' or \\."
        } else if !self.domain.is_empty() && !valid_domain(&self.domain) {
            "Enter a domain like example.com, or leave it empty."
        } else if ports.contains(&0) || (1..3).any(|i| ports[..i].contains(&ports[i])) {
            "The SSH and panel ports must differ; port 80 is used for certificates."
        } else if !self.directory.starts_with('/')
            || self.directory.split('/').any(|part| part == "..")
            || !word(self.directory.trim_matches('/'), "/_-.")
        {
            "The folder must be an absolute path of letters, digits, /, _, - and ."
        } else if !word(&self.image, "/._:@-") {
            "Enter a container image like ghcr.io/owner/rproxy:latest."
        } else {
            return None;
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

struct Server {
    session: russh::client::Handle<Trust>,
    ip: IpAddr,
    /// The password line `sudo -S` reads; `None` when the login is root.
    sudo: Option<String>,
}
impl Server {
    async fn connect(o: &Options, known_hosts: PathBuf) -> Result<Self> {
        let host = o.host.trim();
        let unreachable =
            || err("The server does not answer over SSH. Check the address and the SSH port.");
        let connect = tokio::net::TcpStream::connect((host, o.ssh_port));
        let socket = tokio::time::timeout(Duration::from_secs(15), connect)
            .await
            .map_err(|_| unreachable())?
            .map_err(|_| unreachable())?;
        let ip = socket.peer_addr().map_err(err)?.ip();
        let trust = Trust {
            host: host.into(),
            port: o.ssh_port,
            known_hosts,
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
            let login = session.authenticate_password(o.login.trim(), &o.password);
            if !login.await.map_err(err)?.success() {
                bail!("The server refused the SSH login or password.");
            }
            Ok(session)
        };
        let session = tokio::time::timeout(Duration::from_secs(45), handshake)
            .await
            .map_err(|_| err("The SSH connection timed out."))??;
        let mut server = Self {
            session,
            ip,
            sudo: None,
        };
        if server.sh("id -u").await?.trim() != "0" {
            server.sudo = Some(format!("{}\n", o.password));
        }
        Ok(server)
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
                        line(&text[seen..seen + end]);
                        seen += end + 1;
                    }
                }
                russh::ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                _ => {}
            }
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
    async fn upload(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let command = format!("umask 077; cat > {}", quote(path));
        self.send(&command, bytes).await.map(drop)
    }
}

/// The new server's own configuration: an exit, so traffic of the clients
/// it will serve leaves from it directly. Nothing of this device's
/// configuration goes there.
fn profile() -> Result<Vec<u8>> {
    let direct = plugin_routing::Rule {
        id: Id::random(),
        pattern: "*".into(),
        action: plugin_routing::Route::Direct,
    };
    Profile::new()?
        .list(&plugin_routing::RULES, &[direct])?
        .finish()
}

/// `<address>.sslip.io` resolves to the address itself: a certificate
/// without owning a domain.
fn domain(o: &Options, ip: IpAddr) -> Result<String> {
    if !o.domain.is_empty() {
        return Ok(o.domain.clone());
    }
    match ip {
        IpAddr::V4(v4) if !(v4.is_private() || v4.is_loopback() || v4.is_link_local()) => {
            Ok(format!("{}.sslip.io", v4.to_string().replace('.', "-")))
        }
        _ => bail!("Without a domain the server needs a public IPv4 address. Enter your domain."),
    }
}

const STEPS: u64 = 8;

pub async fn install(cx: &Cx, o: &Options) -> Result<()> {
    let steps = std::sync::atomic::AtomicU64::new(0);
    let step = |text: &str| {
        let done = steps.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        cx.progress(done.min(STEPS - 1), Some(STEPS));
        cx.set_live(&STATUS, Status::Working(format!("{text}…")));
    };
    if let Some(problem) = o.problem() {
        bail!("{problem}");
    }
    step("Connecting");
    let server = Server::connect(o, cx.data_dir().join("known_hosts")).await?;
    if o.disable_ipv6 && server.ip.is_ipv6() {
        bail!("This connection uses IPv6. Enter the server's IPv4 address, or leave IPv6 on.");
    }
    let domain = domain(o, server.ip)?;
    let (panel, auth) = (format!("panel.{domain}"), format!("auth.{domain}"));
    let profile = profile()?;
    let mut record = Deployment {
        id: Id::random(),
        host: o.host.trim().into(),
        directory: o.directory.clone(),
        panel: format!("https://{panel}:{}", o.https_port),
        login: o.panel_login.clone(),
        password: match o.panel_password.as_str() {
            "" => format!("{}{}", Id::random(), Id::random()),
            chosen => chosen.into(),
        },
        ready: false,
    };
    let env: String = [
        ("RPROXY_PANEL_DOMAIN", panel.as_str()),
        ("RPROXY_AUTH_DOMAIN", auth.as_str()),
        ("DATA_DIR", &format!("{}/data", o.directory)),
        ("TINYAUTH_USERNAME", &record.login),
        ("TINYAUTH_PASSWORD", &record.password),
        ("HTTPS_PORT", &o.https_port.to_string()),
        ("RPROXY_IMAGE", &o.image),
        ("ACME_EMAIL", &o.email),
    ]
    .iter()
    .map(|(key, value)| format!("{key}='{value}'\n"))
    .collect();

    step("Uploading");
    let stage = server
        .sh("umask 077; mktemp -d /tmp/rproxy-install.XXXXXX")
        .await?;
    let stage = stage.trim();
    let script = format!(
        "DEST={} STAGE={} PANEL={} AUTH={} HTTPS_PORT={} DISABLE_IPV6={}\n{}",
        quote(&o.directory),
        quote(stage),
        quote(&record.panel),
        quote(&format!("https://{auth}:{}", o.https_port)),
        o.https_port,
        u8::from(o.disable_ipv6),
        include_str!("install.sh"),
    );
    let files: [(&str, &[u8]); 5] = [
        (
            "compose.yaml",
            include_bytes!("../../../docker/compose.yaml"),
        ),
        ("Caddyfile", include_bytes!("../../../docker/Caddyfile")),
        (".env", env.as_bytes()),
        ("profile.sqlite3", &profile),
        ("install.sh", script.as_bytes()),
    ];
    let result = async {
        for (name, bytes) in files {
            server.upload(&format!("{stage}/{name}"), bytes).await?;
        }
        cx.checkpoint()?;
        // Saved before the server changes: its password is never lost.
        save(cx, record.clone()).await?;
        let mut reason = None;
        let (command, password) = match &server.sudo {
            Some(password) => (
                format!("sudo -S -p '' sh {stage}/install.sh"),
                password.as_bytes(),
            ),
            None => (format!("sh {stage}/install.sh"), &b""[..]),
        };
        let run = server.run(&command, password, |line| {
            if let Some(text) = line.strip_prefix("::step::") {
                step(text);
            } else if let Some(text) = line.strip_prefix("::error::") {
                reason = Some(text.to_owned());
            }
        });
        let run = tokio::time::timeout(Duration::from_secs(1800), cx.or_cancel(run));
        match run.await {
            Ok(Ok(Ok(Output { code: Some(0), .. }))) => {
                record.ready = true;
                save(cx, record.clone()).await
            }
            // The script removed what it had installed.
            Ok(Ok(Ok(Output {
                code: Some(_),
                text,
            }))) => {
                let id = record.id;
                cx.write(move |w| {
                    w.update(&DEPLOYMENTS, |all| all.retain(|d| d.id != id));
                    Ok(())
                })
                .await?;
                Err(err(reason.unwrap_or_else(|| last_lines(&text, &o.password))))
            }
            // No exit code: the outcome on the server is unknown; the record stays.
            Ok(Err(cancelled)) => Err(cancelled),
            _ => bail!("The connection to the server was lost during the installation."),
        }
    }
    .await;
    let _ = server.sh(&format!("rm -rf -- {}", quote(stage))).await;
    result
}

async fn save(cx: &Cx, record: Deployment) -> Result<()> {
    cx.write(move |w| {
        w.update(&DEPLOYMENTS, |all| {
            match all.iter_mut().find(|d| d.id == record.id) {
                Some(saved) => *saved = record,
                None => all.push(record),
            }
        });
        Ok(())
    })
    .await
}

/// The end of the server's output for a failure the script did not explain.
fn last_lines(text: &str, secret: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("::"))
        .collect();
    let tail = lines[lines.len().saturating_sub(8)..].join("\n");
    format!(
        "The installation failed on the server:\n{}",
        tail.replace(secret, "•••")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> Options {
        Options {
            host: "203.0.113.1".into(),
            password: "password".into(),
            ..Options::default()
        }
    }
    #[test]
    fn unsafe_settings_and_port_collisions_are_rejected() {
        assert_eq!(options().problem(), None);
        let rejected = |change: fn(&mut Options)| {
            let mut o = options();
            change(&mut o);
            o.problem().is_some()
        };
        for directory in ["/", "/srv/../etc", "/srv/a;id", "relative", "/srv/a b"] {
            let mut o = options();
            o.directory = directory.into();
            assert!(o.problem().is_some(), "{directory}");
        }
        assert!(rejected(|o| o.https_port = o.ssh_port));
        assert!(rejected(|o| o.https_port = 80));
        assert!(rejected(|o| o.domain = "example.com;id".into()));
        assert!(rejected(|o| o.panel_password = "a'b".into()));
        assert!(rejected(
            |o| o.panel_password = "a\nRPROXY_IMAGE=evil".into()
        ));
        assert!(rejected(|o| o.panel_login = "a b".into()));
        assert!(rejected(|o| o.image = "image; rm".into()));
        assert!(!rejected(|o| o.panel_password = "p@$$ w0rd \"!".into()));
    }
    #[test]
    fn the_automatic_domain_needs_a_public_ipv4_address() {
        let o = options();
        assert_eq!(
            domain(&o, "203.0.113.7".parse().unwrap()).unwrap(),
            "203-0-113-7.sslip.io"
        );
        assert!(domain(&o, "192.168.1.5".parse().unwrap()).is_err());
        assert!(domain(&o, "2001:db8::1".parse().unwrap()).is_err());
        let own = Options {
            domain: "example.com".into(),
            ..o
        };
        assert_eq!(
            domain(&own, "2001:db8::1".parse().unwrap()).unwrap(),
            "example.com"
        );
    }
    #[test]
    fn the_profile_is_a_new_server_with_a_direct_route() {
        use rproxy_kernel::store::Store;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile");
        std::fs::write(&path, profile().unwrap()).unwrap();
        let db = Store::open(path).unwrap();
        let rules = db
            .transaction(|tx| tx.all::<plugin_routing::Rule>())
            .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].action, plugin_routing::Route::Direct);
    }
    #[test]
    fn a_failure_shows_the_end_of_the_output_without_the_password() {
        let text = "::step::Checking\nok\n\nE: password broke apt\n";
        assert_eq!(
            last_lines(text, "password"),
            "The installation failed on the server:\nok\nE: ••• broke apt"
        );
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
