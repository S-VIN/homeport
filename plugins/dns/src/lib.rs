//! # dns
//! The core's own DNS: which server answers, Fake IP and fixed answers
//! (Hosts). It resolves names for the rules and direct connections and, with
//! TUN, answers the apps (port 53 is captured only while this plugin is on).
use homeport_kernel::{
    kit,
    prelude::*,
    ui::{Column, ListEditor},
};
use std::net::IpAddr;

pub static PLUGIN: Plugin = Plugin {
    id: "dns",
    name: "DNS",
    description: "Choose the DNS server, turn on Fake IP and add your own hosts.",
    migrations: homeport_kernel::migrations!(),
    setup,
    ..Plugin::DEFAULT
};

choices!(pub enum Server {
    Cloudflare => "Cloudflare",
    Google => "Google",
    Quad9 => "Quad9",
    AdGuard => "AdGuard · blocks ads",
    System => "System",
    Custom => "Custom",
});
impl Server {
    /// The address mihomo asks (DNS over HTTPS by IP: no lookup first).
    fn address(self) -> Option<&'static str> {
        Some(match self {
            Self::Cloudflare => "https://1.1.1.1/dns-query",
            Self::Google => "https://8.8.8.8/dns-query",
            Self::Quad9 => "https://9.9.9.9/dns-query",
            Self::AdGuard => "https://94.140.14.14/dns-query",
            Self::System => "system",
            Self::Custom => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub server: Server,
    /// The address of the custom server.
    pub custom: String,
    pub fake_ip: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            server: Server::Cloudflare,
            custom: String::new(),
            fake_ip: false,
        }
    }
}
pub static SETTINGS: State<Settings> = State::new("dns.settings");

/// A fixed answer: this domain always gets this address.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, Table)]
#[table(name = "dns_hosts")]
pub struct Host {
    pub id: Id,
    pub domain: String,
    pub ip: String,
}
pub static HOSTS: State<Vec<Host>> = State::new("dns.hosts");

/// A server address mihomo understands: `https://`, `tls://`, `quic://`,
/// `udp://` or `tcp://` with a host, or a plain IP (port optional).
pub fn check_address(text: &str) -> std::result::Result<String, &'static str> {
    const WRONG: &str =
        "Enter a DNS server: an IP address, https://…/dns-query, tls://… or quic://…";
    let text = text.trim();
    if text.parse::<IpAddr>().is_ok() || text.parse::<std::net::SocketAddr>().is_ok() {
        return Ok(text.to_owned());
    }
    let url = url::Url::parse(text).map_err(|_| WRONG)?;
    let schemes = ["https", "tls", "quic", "udp", "tcp"];
    if !schemes.contains(&url.scheme()) || url.host_str().is_none_or(str::is_empty) {
        return Err(WRONG);
    }
    Ok(text.to_owned())
}

fn setup(p: &mut Setup) -> Result<()> {
    p.editable_setting(&SETTINGS);
    p.list(&HOSTS).validate(|_, h, _, others| {
        h.domain = h.domain.trim().trim_end_matches('.').to_lowercase();
        h.ip = h.ip.trim().to_owned();
        if h.domain.is_empty() || h.domain.contains(char::is_whitespace) {
            bail!("Enter a domain, e.g. example.com or *.example.com.");
        }
        if h.ip.parse::<IpAddr>().is_err() {
            bail!("Enter an IPv4 or IPv6 address.");
        }
        if others.iter().any(|o| o.domain == h.domain) {
            bail!("This domain is already in Hosts.");
        }
        Ok(())
    });
    p.core(fragment);
    p.page_fn(
        PageSpec {
            title: "DNS",
            icon: Icon::Globe,
            order: 55,
        },
        page,
    );
    Ok(())
}

fn fragment(snap: &Snapshot) -> Fragment {
    let s = snap.get(&SETTINGS);
    // A custom server not entered yet: the system's.
    let server = match s.server.address() {
        Some(a) => a.to_owned(),
        None if s.custom.is_empty() => "system".to_owned(),
        None => s.custom.clone(),
    };
    Fragment {
        dns: Some(mihomo::Dns {
            enable: true,
            nameservers: vec![server],
            fake_ip: s.fake_ip,
            hosts: (snap.get(&HOSTS).iter())
                .map(|h| (h.domain.clone(), h.ip.clone()))
                .collect(),
        }),
        ..Default::default()
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    let s = (*cx.get(&SETTINGS)).clone();
    cx.page_header(ui, "", |_, _| {});
    kit::settings_card(ui, |ui| {
        let mut server = s.server;
        kit::settings_row(
            ui,
            "Server",
            "Answers name lookups of the core and, with TUN, of all apps",
            |ui| {
                if kit::choice(ui, "server", &mut server, Server::ALL, 190., "") {
                    cx.update("server", &SETTINGS, move |s| s.server = server);
                }
            },
        );
        if s.server == Server::Custom {
            kit::settings_row(ui, "Address", "", |ui| {
                if let Some(text) = kit::field(
                    ui,
                    "custom",
                    &s.custom,
                    "https://dns.example/dns-query",
                    260.,
                ) {
                    match check_address(&text) {
                        Ok(a) => cx.update("custom", &SETTINGS, move |s| s.custom = a),
                        Err(e) => cx.notify_error(e),
                    }
                }
            });
        }
        let mut fake_ip = s.fake_ip;
        kit::settings_row(
            ui,
            "Fake IP",
            "Faster with TUN: apps get stand-in addresses at once and the real name goes to the server. Some apps and games may not work with it.",
            |ui| {
                if kit::toggle_named(ui, &mut fake_ip, "", "Fake IP").changed() {
                    cx.update("fake-ip", &SETTINGS, move |s| s.fake_ip = fake_ip);
                }
            },
        );
    });
    kit::section(ui, "Hosts", Some("These domains always get your address"));
    ListEditor::new(&HOSTS)
        .unordered()
        .add_form(
            "Add host",
            || Host {
                id: Id::random(),
                domain: String::new(),
                ip: String::new(),
            },
            |ui, h, width| {
                kit::Fields::new(width)
                    .input("domain", &mut h.domain, "example.com")
                    .help("*.example.com matches subdomains")
                    .input("ip", &mut h.ip, "192.168.1.10")
                    .show(ui)
                    && !h.domain.trim().is_empty()
                    && !h.ip.trim().is_empty()
            },
        )
        .column(Column::edit("Domain", "example.com", |h: &mut Host| {
            &mut h.domain
        }))
        .column(Column::edit("IP address", "192.168.1.10", |h: &mut Host| &mut h.ip).width(160.))
        .empty(
            Icon::Globe,
            "No hosts",
            "Add a domain and its address above.",
        )
        .show(ui, cx);
}

#[cfg(test)]
mod tests {
    use super::check_address;
    #[test]
    fn server_addresses() {
        for ok in [
            "1.1.1.1",
            "8.8.8.8:53",
            "https://dns.google/dns-query",
            "tls://dns.quad9.net",
            "quic://dns.adguard.com",
        ] {
            assert!(check_address(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "dns.google",
            "http://x/dns-query",
            "ftp://x",
            "https://",
        ] {
            assert!(check_address(bad).is_err(), "{bad}");
        }
    }
}
