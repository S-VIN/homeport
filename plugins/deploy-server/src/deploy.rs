use crate::Deployment;
use rproxy_kernel::{prelude::*, store::Store};
use russh::{ChannelMsg, client};
use serde::{Deserialize, Serialize};
use std::{
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    time::Duration,
};

#[derive(Clone)]
pub struct Options {
    pub host: String,
    pub login: String,
    pub password: String,
    pub ssh_port: u16,
    pub domain: String,
    pub https_port: u16,
    pub proxy_port: u16,
    pub ui_port: u16,
    pub auth_port: u16,
    pub directory: String,
    pub image: String,
    pub admin: String,
    pub admin_password: String,
    pub email: String,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            host: String::new(),
            login: "root".into(),
            password: String::new(),
            ssh_port: 22,
            domain: "auto".into(),
            https_port: 8443,
            proxy_port: 443,
            ui_port: 20800,
            auth_port: 20801,
            directory: "/srv/rproxy".into(),
            image: "ghcr.io/s-vin/rproxy:latest".into(),
            admin: "admin".into(),
            admin_password: String::new(),
            email: String::new(),
        }
    }
}
fn err(e: impl std::fmt::Display) -> Error {
    Error::msg(e.to_string())
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn env_value(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}
impl Options {
    pub fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() || self.login.trim().is_empty() || self.password.is_empty() {
            bail!("Enter the SSH host, login and password.");
        }
        if self.password.contains(['\n', '\r', '\0']) {
            bail!("SSH password cannot contain line breaks or NUL.");
        }
        let ports = [
            80,
            self.https_port,
            self.proxy_port,
            self.ui_port,
            self.auth_port,
        ];
        if self.ssh_port == 0
            || ports.iter().any(|p| *p == 0 || *p == self.ssh_port)
            || ports
                .iter()
                .enumerate()
                .any(|(i, p)| ports[..i].contains(p))
        {
            bail!("SSH, HTTP (80), HTTPS, VLESS and internal ports must be nonzero and distinct.");
        }
        if !self.directory.starts_with('/')
            || self.directory == "/"
            || self.directory.split('/').any(|s| s == "..")
            || !self
                .directory
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/_-.".contains(&c))
        {
            bail!(
                "Use a new absolute installation directory containing letters, numbers, /, _, - or ."
            );
        }
        if self.domain != "auto" && !plugin_vless_server::valid_domain(&self.domain) {
            bail!("Enter auto or a public DNS domain, without a scheme or port.");
        }
        if self.admin.is_empty()
            || !self
                .admin
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
        {
            bail!("Panel login must contain letters, numbers, _ or -.");
        }
        if self.image.is_empty()
            || !self
                .image
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/._:@-".contains(&c))
        {
            bail!("Invalid container image reference.");
        }
        if [&self.admin_password, &self.email]
            .iter()
            .any(|v| v.contains(['\n', '\r', '\0']))
        {
            bail!("Settings cannot contain line breaks or NUL.");
        }
        Ok(())
    }
}
// First use pins a key; a changed key (including algorithm changes) fails closed.
struct Trust {
    host: String,
    port: u16,
    path: std::path::PathBuf,
}
impl client::Handler for Trust {
    type Error = russh::Error;
    async fn check_server_key(
        &mut self,
        server: &russh::keys::PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let key = server.public_key();
        let known =
            russh::keys::known_hosts::known_host_keys_path(&self.host, self.port, &self.path)?;
        if known.is_empty() {
            russh::keys::known_hosts::learn_known_hosts_path(
                &self.host, self.port, &key, &self.path,
            )?;
            Ok(true)
        } else {
            Ok(known.iter().any(|(_, saved)| *saved == key))
        }
    }
}
struct Session {
    inner: client::Handle<Trust>,
}
fn connect(o: &Options, data: &Path) -> Result<(Session, std::net::IpAddr)> {
    let addresses: Vec<_> = (o.host.as_str(), o.ssh_port)
        .to_socket_addrs()
        .map_err(err)?
        .collect();
    let mut socket = None;
    for address in addresses {
        if let Ok(s) = TcpStream::connect_timeout(&address, Duration::from_secs(15)) {
            socket = Some(s);
            break;
        }
    }
    let socket =
        socket.ok_or_else(|| err("SSH is unreachable. Check the address, port and firewall."))?;
    let ip = socket.peer_addr().map_err(err)?.ip();
    socket.set_nonblocking(true).map_err(err)?;
    std::fs::create_dir_all(data).map_err(err)?;
    let trust = Trust {
        host: o.host.clone(),
        port: o.ssh_port,
        path: data.join("known_hosts"),
    };
    tokio::runtime::Handle::current().block_on(async {
        tokio::time::timeout(Duration::from_secs(45), async {
            let socket = tokio::net::TcpStream::from_std(socket).map_err(err)?;
            let config = client::Config {
                inactivity_timeout: Some(Duration::from_secs(1800)),
                ..Default::default()
            };
            let mut inner = client::connect_stream(std::sync::Arc::new(config), socket, trust)
                .await
                .map_err(|e| err(format!("SSH handshake failed (check host key): {e}")))?;
            if !inner
                .authenticate_password(&o.login, &o.password)
                .await
                .map_err(err)?
                .success()
            {
                bail!("SSH password authentication failed.");
            }
            Ok((Session { inner }, ip))
        })
        .await
        .map_err(err)?
    })
}
fn command(session: &Session, cmd: &str, input: &[u8]) -> Result<String> {
    tokio::runtime::Handle::current().block_on(async {
        tokio::time::timeout(Duration::from_secs(1800), async {
            let mut ch = session.inner.channel_open_session().await.map_err(err)?;
            ch.exec(true, cmd).await.map_err(err)?;
            if !input.is_empty() { ch.data(input).await.map_err(err)?; }
            ch.eof().await.map_err(err)?;
            let mut tail = Vec::new(); let mut code = None;
            while let Some(msg) = ch.wait().await {
                match msg {
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        tail.extend_from_slice(&data); if tail.len() > 32768 { tail.drain(..tail.len()-32768); }
                    },
                    ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                    _ => {},
                }
            }
            let text = String::from_utf8_lossy(&tail).into_owned();
            if code != Some(0) { return Err(err(format!("Remote command failed ({code:?}): {text}"))); }
            Ok(text)
        }).await.map_err(|_| err("SSH operation timed out. Inspect the saved installation directory before retrying."))?
    })
}
fn upload(session: &Session, path: &str, data: &[u8]) -> Result<()> {
    command(session, &format!("umask 077; cat > {}", quote(path)), data).map(|_| ())
}

pub fn install(
    o: Options,
    data: &Path,
    progress: impl Fn(&str),
    save: impl Fn(Deployment) -> Result<()>,
) -> Result<()> {
    o.validate()?;
    progress("Connecting over SSH…");
    let (session, ip) = connect(&o, data)?;
    let domain = if o.domain == "auto" {
        if !ip.is_ipv4() || ip.is_loopback() || ip.is_unspecified() {
            bail!("Auto domain needs a public IPv4 SSH address. Use a DNS domain for IPv6.");
        }
        format!("{}.sslip.io", ip.to_string().replace('.', "-"))
    } else {
        o.domain.clone()
    };
    let panel_domain = format!("panel.{domain}");
    let auth_domain = format!("auth.{domain}");
    let password = if o.admin_password.is_empty() {
        format!("{}{}", Id::random(), Id::random())
    } else {
        o.admin_password.clone()
    };
    let (db, link) = seed(&panel_domain, o.proxy_port, o.https_port, &o.email)?;
    let mut record = Deployment {
        host: o.host.clone(),
        directory: o.directory.clone(),
        panel: format!("https://{panel_domain}:{}", o.https_port),
        connection: link,
        username: o.admin.clone(),
        password: password.clone(),
        status: "Prepared".into(),
    };
    let env = [
        ("RPROXY_PANEL_DOMAIN", panel_domain),
        ("RPROXY_AUTH_DOMAIN", auth_domain),
        ("DATA_DIR", format!("{}/data", o.directory)),
        ("TINYAUTH_USERNAME", o.admin.clone()),
        ("TINYAUTH_PASSWORD", password.clone()),
        ("HTTPS_PORT", o.https_port.to_string()),
        ("RPROXY_UI_PORT", o.ui_port.to_string()),
        ("TINYAUTH_PORT", o.auth_port.to_string()),
        ("RPROXY_IMAGE", o.image.clone()),
        ("ACME_EMAIL", o.email.clone()),
    ]
    .into_iter()
    .map(|(k, v)| format!("{k}={}\n", env_value(&v)))
    .collect::<String>();
    let stage = format!("/tmp/rproxy-install-{}", Id::random());
    command(
        &session,
        &format!("umask 077; mkdir {}", quote(&stage)),
        b"",
    )?;
    let mut saved = false;
    let result = (|| {
        progress("Preparing server and Docker Compose…");
        let script = include_str!("install.sh")
            .replace("@DIRECTORY@", &quote(&o.directory))
            .replace("@STAGE@", &quote(&stage))
            .replace("@PANEL@", &quote(&record.panel))
            .replace("@PROXY_PORT@", &o.proxy_port.to_string())
            .replace(
                "@PORTS@",
                &format!(
                    "80 {} {} {} {}",
                    o.https_port, o.proxy_port, o.ui_port, o.auth_port
                ),
            );
        for (name, bytes) in [
            (
                "compose.yaml",
                include_bytes!("../../../docker/compose.yaml").as_slice(),
            ),
            (
                "Caddyfile",
                include_bytes!("../../../docker/Caddyfile").as_slice(),
            ),
            (".env", env.as_bytes()),
            ("seed.sqlite3", db.as_slice()),
            ("install.sh", script.as_bytes()),
        ] {
            upload(&session, &format!("{stage}/{name}"), bytes)?;
        }
        let uid = command(&session, "id -u", b"")?;
        let preflight = format!("test ! -e {}", quote(&o.directory));
        let check = if uid.trim() == "0" {
            preflight
        } else {
            format!("sudo -S -p '' sh -c {}", quote(&preflight))
        };
        command(&session, &check, format!("{}\n", o.password).as_bytes())
            .map_err(|_| err("Installation directory exists or sudo is unavailable. Choose a new directory and verify sudo access."))?;
        save(record.clone())?;
        saved = true;
        progress("Installing containers and waiting for HTTPS (up to 30 minutes)…");
        let uid = command(&session, "id -u", b"")?;
        let cmd = if uid.trim() == "0" {
            format!("sh {}", quote(&format!("{stage}/install.sh")))
        } else {
            format!("sudo -S -p '' sh {}", quote(&format!("{stage}/install.sh")))
        };
        command(&session, &cmd, format!("{}\n", o.password).as_bytes())?;
        record.status = "Ready".into();
        save(record.clone())?;
        progress("Server ready. Connection and panel links saved.");
        Ok(())
    })();

    let _ = command(&session, &format!("rm -rf -- {}", quote(&stage)), b"");
    result.map_err(|e: Error| {
        let message = e
            .to_string()
            .replace(&o.password, "[redacted]")
            .replace(&password, "[redacted]");
        progress(if saved {
            "Installation failed. Open the error details; recovery links are saved."
        } else {
            "Installation failed. Open the error details."
        });
        if saved {
            record.status = format!("Failed: {message}");
            let _ = save(record);
        }
        err(message)
    })
}

// Seed an independent profile, never export the client's private configuration.
#[derive(Clone, Serialize, Deserialize, Table)]
#[table(name = "kernel_settings")]
struct Setting {
    id: String,
    json: String,
}
#[derive(Clone, Serialize, Deserialize, Table)]
#[table(name = "kernel_plugins")]
struct Enabled {
    id: String,
    enabled: bool,
}
fn seed(domain: &str, port: u16, https_port: u16, email: &str) -> Result<(Vec<u8>, String)> {
    use plugin_vless_server::{Client, Settings, generate_keys};
    let dir = tempfile::tempdir().map_err(err)?;
    let path = dir.path().join("seed.sqlite3");
    let db = Store::open(&path)?;
    db.register::<Setting>()?;
    db.register::<Enabled>()?;
    db.register::<Client>()?;
    db.register::<plugin_routing::Rule>()?;
    let keys = generate_keys();
    let settings = Settings {
        enabled: true,
        domain: domain.into(),
        port,
        email: email.into(),
        redirect_http: false,
        website: plugin_vless_server::Website::Own,
        own_site: format!("127.0.0.1:{https_port}"),
    };
    for (id, json) in [
        ("vless-server.settings", serde_json::to_string(&settings)),
        ("vless-server.keys", serde_json::to_string(&keys)),
    ] {
        let row = Setting {
            id: id.into(),
            json: json.map_err(err)?,
        };
        db.transaction(|tx| tx.put(&row))?;
    }
    db.transaction(|tx| {
        tx.put(&Enabled {
            id: "vless-server".into(),
            enabled: true,
        })
    })?;
    let client = Client {
        id: Id::random(),
        name: "My device".into(),
    };
    db.transaction(|tx| tx.put(&client))?;
    db.transaction(|tx| {
        tx.put(&plugin_routing::Rule {
            id: Id::random(),
            pattern: "*".into(),
            action: plugin_routing::Route::Direct,
        })
    })?;
    let mut link =
        url::Url::parse(&format!("vless://{}@{domain}:{port}", client.id)).map_err(err)?;
    link.query_pairs_mut()
        .append_pair("encryption", "none")
        .append_pair("security", "reality")
        .append_pair("sni", domain)
        .append_pair("fp", "chrome")
        .append_pair("pbk", &keys.public_key)
        .append_pair("sid", &keys.short_id)
        .append_pair("type", "xhttp")
        .append_pair("path", &keys.path)
        .append_pair("mode", "auto");
    link.set_fragment(Some("Deployed server"));
    drop(db);
    Ok((std::fs::read(path).map_err(err)?, link.to_string()))
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
    fn rejects_unsafe_settings_and_port_collisions() {
        let mut o = options();
        assert!(o.validate().is_ok());
        for directory in ["/", "/srv/../etc", "/srv/a;id", "relative"] {
            o.directory = directory.into();
            assert!(o.validate().is_err());
        }
        o = options();
        o.https_port = o.proxy_port;
        assert!(o.validate().is_err());
        o = options();
        o.domain = "example.com;id".into();
        assert!(o.validate().is_err());
        o = options();
        o.admin_password = "abc\nRPROXY_IMAGE=evil".into();
        assert!(o.validate().is_err());
    }
    #[test]
    fn seed_contains_only_new_server_credentials_and_direct_route() {
        let (bytes, link) = seed("node.example.com", 443, 8443, "").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile");
        std::fs::write(&path, bytes).unwrap();
        let db = Store::open(path).unwrap();
        let clients = db
            .transaction(|tx| tx.all::<plugin_vless_server::Client>())
            .unwrap();
        assert_eq!(clients.len(), 1);
        let link = url::Url::parse(&link).unwrap();
        assert_eq!(link.username(), clients[0].id.to_string());
        let settings = db.transaction(|tx| tx.all::<Setting>()).unwrap();
        assert_eq!(settings.len(), 2);
        let config: plugin_vless_server::Settings = serde_json::from_str(
            &settings
                .iter()
                .find(|s| s.id.ends_with("settings"))
                .unwrap()
                .json,
        )
        .unwrap();
        assert!(config.enabled);
        assert!(!config.redirect_http);
        let keys: plugin_vless_server::Keys = serde_json::from_str(
            &settings
                .iter()
                .find(|s| s.id.ends_with("keys"))
                .unwrap()
                .json,
        )
        .unwrap();
        assert!(
            link.query_pairs()
                .any(|(k, v)| k == "pbk" && v == keys.public_key)
        );
        let routes = db
            .transaction(|tx| tx.all::<plugin_routing::Rule>())
            .unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].action, plugin_routing::Route::Direct);
        let (_, second) = seed("node.example.com", 443, 8443, "").unwrap();
        assert_ne!(link.as_str(), second);
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
