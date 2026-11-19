//! # vless-inbound
//! Lets other devices connect to this machine: a VLESS listener built for
//! stealth, following current practice against DPI:
//! - REALITY with "self steal": the TLS handshake is the one of a real website
//!   on the user's own domain, served by this plugin on a loopback port with a
//!   Let's Encrypt certificate (see [`site`]); probes without a key get that site;
//! - XHTTP transport (plain HTTP/2 requests; raw TCP is throttled after ~16 KB);
//! - randomized ranges instead of fixed values (padding, upload stream lifetime);
//!   share links ask clients for a browser TLS fingerprint and explicit,
//!   randomized connection reuse (XMUX) so they never open a burst of handshakes.
//!
//! The plugin does not decide where client traffic goes: Routing does. An exit
//! node is this plugin plus the rule `*` → Direct (the page offers it); a relay
//! node sends client traffic via its own selected server.
use homeport_kernel::{
    mihomo::{Listener, ListenerKind, Reality, VlessListener, Xhttp},
    prelude::*,
};
use std::net::IpAddr;

pub mod site;
mod ui;

pub static PLUGIN: Plugin = Plugin {
    id: "vless-inbound",
    name: "VLESS inbound",
    description: "Let other devices connect to this device over VLESS with REALITY and XHTTP, disguised as your own website.",
    requires: &[&plugin_servers::PLUGIN, &plugin_routing::PLUGIN],
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

choices!(pub enum Website {
    Builtin => "Built-in site",
    Own => "My own site",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub enabled: bool,
    /// The user's domain; its A/AAAA record points to this machine.
    pub domain: String,
    pub port: u16,
    pub website: Website,
    /// `host:port` of the user's own TLS site (for [`Website::Own`]).
    pub own_site: String,
    /// Optional Let's Encrypt contact.
    pub email: String,
    pub redirect_http: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            domain: String::new(),
            port: 443,
            website: Website::Builtin,
            own_site: String::new(),
            email: String::new(),
            redirect_http: true,
        }
    }
}

/// Generated once and kept: changing them would invalidate every client link.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Keys {
    pub private_key: String,
    pub public_key: String,
    pub short_id: String,
    pub path: String,
    /// Loopback port of the built-in site.
    pub site_port: u16,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "vless_inbound_clients")]
pub struct Client {
    /// Also the client's VLESS UUID.
    pub id: Id,
    pub name: String,
}

pub static CONFIG: State<Settings> = State::new("vless-inbound.settings");
pub static KEYS: State<Keys> = State::new("vless-inbound.keys");
pub static CLIENTS: State<Vec<Client>> = State::new("vless-inbound.clients");
pub static CERTIFICATE: State<site::Certificate> = State::new("vless-inbound.certificate");

/// Turn the server on or off. Turning it on needs a domain and at least one client.
pub struct SetEnabled(pub bool);
impl Op for SetEnabled {
    type Reply = ();
    const SAVES: bool = true;
}

pub fn generate_keys() -> Keys {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let secret = x25519_dalek::StaticSecret::from(random_bytes::<32>());
    let public = x25519_dalek::PublicKey::from(&secret);
    let site_port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_or(18443, |a| a.port());
    Keys {
        private_key: URL_SAFE_NO_PAD.encode(secret.to_bytes()),
        public_key: URL_SAFE_NO_PAD.encode(public.as_bytes()),
        short_id: hex(&random_bytes::<8>()),
        path: format!("/{}", hex(&random_bytes::<6>())),
        site_port,
    }
}

/// A hostname that can be an SNI (not an IP, at least one dot).
pub fn valid_domain(d: &str) -> bool {
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
/// Names that never get a public certificate (tests, LAN).
fn private_name(d: &str) -> bool {
    [
        ".test",
        ".local",
        ".localhost",
        ".invalid",
        ".example",
        ".lan",
        ".internal",
        ".home.arpa",
    ]
    .iter()
    .any(|s| d.ends_with(s))
}

/// What is missing before the server can run.
pub fn problem(snap: &Snapshot) -> Option<&'static str> {
    if snap.get(&CLIENTS).is_empty() {
        return Some("Add a client to connect with.");
    }
    settings_problem(&snap.get(&CONFIG), &snap.get(&KEYS))
}
/// What blocks the server apart from clients.
fn settings_problem(c: &Settings, keys: &Keys) -> Option<&'static str> {
    if !valid_domain(&c.domain) {
        return Some("Enter your domain. Its DNS record must point to this device.");
    }
    if c.website == Website::Own
        && c.own_site
            .trim()
            .rsplit_once(':')
            .is_none_or(|(h, p)| h.is_empty() || p.parse::<u16>().is_err())
    {
        return Some("Enter your site as host:port, for example 127.0.0.1:8443.");
    }
    if c.port == 0 {
        return Some("Choose a port, usually 443.");
    }
    if keys.private_key.is_empty() {
        return Some("Keys are not generated yet.");
    }
    None
}

/// Share link of one client (VLESS + REALITY + XHTTP).
pub fn link(snap: &Snapshot, client: &Client) -> String {
    let (c, k) = (snap.get(&CONFIG), snap.get(&KEYS));
    // Ranged connection reuse and padding for xray-based clients (homeport applies the same).
    let extra = r#"{"xPaddingBytes":"100-1000","xmux":{"maxConcurrency":"16-32","hMaxRequestTimes":"600-900","hMaxReusableSecs":"1800-3000"}}"#;
    let mut u = url::Url::parse("vless://placeholder").expect("static URL");
    let _ = u.set_username(&client.id.to_string());
    let _ = u.set_host(Some(&c.domain));
    let _ = u.set_port(Some(c.port));
    u.query_pairs_mut()
        .append_pair("encryption", "none")
        .append_pair("security", "reality")
        .append_pair("sni", &c.domain)
        .append_pair("fp", "chrome")
        .append_pair("pbk", &k.public_key)
        .append_pair("sid", &k.short_id)
        .append_pair("type", "xhttp")
        .append_pair("path", &k.path)
        .append_pair("mode", "auto")
        .append_pair("extra", extra);
    u.set_fragment(Some(&client.name));
    u.to_string()
}

fn listener(snap: &Snapshot) -> Option<Listener> {
    let c = snap.get(&CONFIG);
    if !c.enabled || problem(snap).is_some() {
        return None;
    }
    let k = snap.get(&KEYS);
    let dest = match c.website {
        Website::Builtin => format!("127.0.0.1:{}", k.site_port),
        Website::Own => c.own_site.trim().to_owned(),
    };
    Some(Listener {
        name: "vless".into(),
        kind: ListenerKind::Vless(Box::new(VlessListener {
            users: snap
                .get(&CLIENTS)
                .iter()
                .map(|u| (u.name.clone(), u.id.to_string()))
                .collect(),
            reality: Reality {
                dest,
                private_key: k.private_key.clone(),
                short_ids: vec![k.short_id.clone()],
                server_names: vec![c.domain.clone()],
            },
            xhttp: Some(Xhttp {
                path: k.path.clone(),
                padding_bytes: "100-1000".into(),
                stream_up_server_secs: "20-80".into(),
            }),
        })),
        listen: IpAddr::from([0, 0, 0, 0]),
        port: c.port,
        udp: true,
        users: vec![],
        optional: true,
    })
}

fn setup(p: &mut Setup) -> Result<()> {
    p.editable_setting(&CONFIG);
    p.setting(&KEYS);
    p.live(&CERTIFICATE, || site::Certificate::Starting);
    p.list(&CLIENTS)
        .validate(|_, c, _, others| validate_client(c, others));
    p.on(first_start);
    p.handle(set_enabled);
    p.core(fragment);
    let acme = p.data_dir().join("acme");
    p.service("site", move |snap| site_spec(snap, &acme), run_site);
    ui::register(p);
    Ok(())
}

fn validate_client(c: &mut Client, others: &[Client]) -> Result<()> {
    c.name = c.name.trim().to_owned();
    if c.name.is_empty() {
        bail!("Enter a subscription link name.");
    }
    if others.iter().any(|o| o.name.eq_ignore_ascii_case(&c.name)) {
        bail!("A subscription link with this name already exists.");
    }
    Ok(())
}

/// Keys on first start.
async fn first_start(cx: Cx, _: Started) -> Result<()> {
    cx.write(|w| {
        if w.get(&KEYS).private_key.is_empty() {
            w.set(&KEYS, generate_keys());
        }
        Ok(())
    })
    .await
}

/// Turn the inbound on (with a first client) or off.
async fn set_enabled(cx: Cx, SetEnabled(on): SetEnabled) -> Result<()> {
    cx.write(move |w| {
        if on && w.get(&CLIENTS).is_empty() {
            w.update(&CLIENTS, |l| {
                l.push(Client {
                    id: Id::random(),
                    name: "My device".into(),
                })
            });
        }
        // A missing client was just added above.
        if let Some(why) = settings_problem(&w.get(&CONFIG), &w.get(&KEYS)).filter(|_| on) {
            bail!("{why}");
        }
        w.update(&CONFIG, |c| c.enabled = on);
        Ok(())
    })
    .await?;
    info!("VLESS inbound turned {}", if on { "on" } else { "off" });
    Ok(())
}

fn fragment(snap: &Snapshot) -> Fragment {
    let Some(l) = listener(snap) else {
        return Fragment::default();
    };
    // Domain rules (e.g. on a relay node) must match clients that send IPs.
    Fragment {
        labels: vec![(l.name.clone(), "VLESS clients".into())],
        listeners: vec![l],
        sniff: true,
        ..Default::default()
    }
}

/// The built-in website, while it is on and the settings work.
fn site_spec(snap: &Snapshot, acme: &std::path::Path) -> Option<site::Spec> {
    let c = snap.get(&CONFIG);
    (c.enabled && c.website == Website::Builtin && problem(snap).is_none()).then(|| site::Spec {
        domain: c.domain.clone(),
        port: snap.get(&KEYS).site_port,
        acme: !private_name(&c.domain),
        contact: Some(c.email.trim().to_owned()).filter(|e| e.contains('@')),
        cache: acme.to_owned(),
        redirect_http: c.redirect_http,
    })
}

/// Serve the website; each new certificate state is logged once.
async fn run_site(cx: Cx, spec: site::Spec) -> Result<()> {
    site::run(spec, move |state| {
        if *cx.live(&CERTIFICATE) != state {
            match &state {
                site::Certificate::Starting => {}
                site::Certificate::SelfSigned(why) => info!("Self-signed certificate: {why}"),
                site::Certificate::Issued => info!("Let's Encrypt certificate in use"),
                site::Certificate::Failed(e) => warn!("Certificate: {e}"),
            }
        }
        cx.set_live(&CERTIFICATE, state)
    })
    .await
    .map_err(Error::msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_have_the_lengths_mihomo_expects() {
        let k = generate_keys();
        assert_eq!(k.private_key.len(), 43);
        assert_eq!(k.public_key.len(), 43);
        assert_eq!(k.short_id.len(), 16);
        assert!(k.path.starts_with('/') && k.site_port > 0);
        assert_ne!(generate_keys().private_key, k.private_key);
    }
    #[test]
    fn domains() {
        assert!(valid_domain("node.example.com"));
        assert!(!valid_domain("1.2.3.4") && !valid_domain("localhost") && !valid_domain("-a.com"));
        assert!(private_name("node.test") && !private_name("example.com"));
    }
}
