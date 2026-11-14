//! # vless-server
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
use plugin_routing as routing;
use plugin_servers as servers;
use rproxy_kernel::{
    kit::{self, theme},
    mihomo::{Listener, ListenerKind, Reality, VlessListener, Xhttp},
    prelude::*,
    ui::{Column, ListEditor},
};
use std::net::IpAddr;

pub mod site;

pub static PLUGIN: Plugin = Plugin {
    id: "vless-server",
    name: "VLESS server",
    description: "Let other devices connect to this machine over VLESS with REALITY and XHTTP, disguised as your own website.",
    requires: &["servers"],
    default_enabled: false,
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
#[table(name = "vless_clients")]
pub struct Client {
    /// Also the client's VLESS UUID.
    pub id: Id,
    pub name: String,
}

pub static CONFIG: State<Settings> = State::new("vless-server.settings");
pub static KEYS: State<Keys> = State::new("vless-server.keys");
pub static CLIENTS: State<Vec<Client>> = State::new("vless-server.clients");
pub static CERTIFICATE: State<site::Certificate> = State::new("vless-server.certificate");

/// Turn the server on or off. Turning it on needs a domain and at least one client.
pub struct SetEnabled(pub bool);
impl Op for SetEnabled {
    type Reply = ();
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
        return Some("Enter your domain. Its DNS record must point to this machine.");
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
    // Ranged connection reuse and padding for xray-based clients (rproxy applies the same).
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
    p.list(&CLIENTS).validate(|_, c, _, others| {
        c.name = c.name.trim().to_owned();
        if c.name.is_empty() {
            bail!("Enter a client name.");
        }
        if others.iter().any(|o| o.name.eq_ignore_ascii_case(&c.name)) {
            bail!("A client with this name already exists.");
        }
        Ok(())
    });
    // Keys and the first client on first start.
    p.on(|cx: Cx, _: Started| async move {
        cx.write(|w| {
            if w.get(&KEYS).private_key.is_empty() {
                w.set(&KEYS, generate_keys());
            }
            Ok(())
        })
        .await
    });
    p.handle(|cx: Cx, SetEnabled(on): SetEnabled| async move {
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
        .await
    });
    p.core(|snap| {
        let Some(l) = listener(snap) else {
            return Fragment::default();
        };
        // Domain rules (e.g. on a relay node) must match clients that send IPs.
        Fragment {
            listeners: vec![l],
            sniff: true,
            ..Default::default()
        }
    });
    let data = p.data_dir().join("acme");
    // Earlier versions kept certificates in the app folder; reuse them
    // instead of asking Let's Encrypt again (it limits reissues).
    if let Some(app) = data.ancestors().nth(3).filter(|_| !data.exists()) {
        let _ = std::fs::rename(app.join("vless-server").join("acme"), &data);
    }
    p.service(
        "site",
        move |snap| {
            let c = snap.get(&CONFIG);
            (c.enabled && c.website == Website::Builtin && problem(snap).is_none()).then(|| {
                site::Spec {
                    domain: c.domain.clone(),
                    port: snap.get(&KEYS).site_port,
                    acme: !private_name(&c.domain),
                    contact: Some(c.email.trim().to_owned()).filter(|e| e.contains('@')),
                    cache: data.clone(),
                    redirect_http: c.redirect_http,
                }
            })
        },
        |cx: Cx, spec: site::Spec| async move {
            site::run(spec, move |state| cx.set_live(&CERTIFICATE, state))
                .await
                .map_err(Error::msg)
        },
    );
    ui(p);
    Ok(())
}

/// Client traffic leaves this machine directly only with a `*` → Direct rule.
fn exits_here(snap: &Snapshot) -> Option<bool> {
    let rules = snap.try_get(&routing::RULES)?;
    Some(
        rules
            .iter()
            .any(|r| r.pattern == "*" && r.action == routing::Route::Direct),
    )
}

fn status_rows(ui: &mut egui::Ui, cx: &mut UiCx) {
    let snap = cx.snapshot().clone();
    let c = snap.get(&CONFIG);
    let pal = kit::colors(ui);
    let core = cx.core_status();
    let line = |ui: &mut egui::Ui, icon: Icon, color: egui::Color32, label: &str, text: &str| {
        kit::property_with(ui, label, |ui| {
            ui.spacing_mut().item_spacing.x = 6.;
            kit::icon(ui, icon, 14., color);
            ui.add(egui::Label::new(egui::RichText::new(text).color(pal.foreground)).wrap());
        });
    };
    match core.listener_errors.get("vless") {
        Some(e) => line(
            ui,
            Icon::Error,
            pal.danger,
            "Listener",
            &format!(
                "{e}{}",
                if cfg!(target_os = "linux") && c.port < 1024 {
                    " Ports below 1024 need root or CAP_NET_BIND_SERVICE."
                } else {
                    ""
                }
            ),
        ),
        None if listener(&snap).is_some() && core.running() => line(
            ui,
            Icon::Connected,
            pal.success,
            "Listener",
            &format!("Listening on port {}", c.port),
        ),
        None => line(ui, Icon::Info, pal.muted, "Listener", "Off"),
    }
    if c.website == Website::Builtin {
        let (icon, color, text) = match &*snap.live(&CERTIFICATE) {
            _ if !c.enabled => (Icon::Info, pal.muted, "Starts with the server".to_owned()),
            site::Certificate::Starting => (Icon::Info, pal.muted, "Starting…".to_owned()),
            site::Certificate::SelfSigned(why) => (
                Icon::Warning,
                pal.warning,
                format!("Self-signed for now. {why}"),
            ),
            site::Certificate::Issued => (
                Icon::Shield,
                pal.success,
                format!("Let's Encrypt certificate for {}", c.domain),
            ),
            site::Certificate::Failed(e) => (
                Icon::Error,
                pal.danger,
                format!(
                    "Let's Encrypt failed: {e}. Check that {} points to this machine and port {} is reachable.",
                    c.domain, c.port
                ),
            ),
        };
        line(ui, icon, color, "Website", &text);
    }
    match exits_here(&snap) {
        Some(true) => line(
            ui,
            Icon::Exit,
            pal.success,
            "Exit",
            "Client traffic leaves from this machine (rule * → Direct)",
        ),
        Some(false) => {
            kit::property_with(ui, "Exit", |ui| {
                ui.spacing_mut().item_spacing.x = 6.;
                let via = if servers::selected(&snap).is_some() {
                    "goes via the selected server"
                } else {
                    "is blocked: no server is selected"
                };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("Client traffic follows Routing and {via}."))
                            .color(pal.muted),
                    )
                    .wrap(),
                );
                if kit::Button::ghost("Exit here")
                    .tone(kit::Tone::Accent)
                    .show(ui)
                    .on_hover_text("Add the routing rule * → Direct")
                    .clicked()
                {
                    cx.submit(
                        "exit-rule",
                        Add::new(
                            &routing::RULES,
                            routing::Rule {
                                id: Id::random(),
                                pattern: "*".into(),
                                action: routing::Route::Direct,
                            },
                        ),
                    );
                }
            });
        }
        None => line(
            ui,
            Icon::Info,
            pal.muted,
            "Exit",
            "Routing is off: client traffic goes via the selected server.",
        ),
    }
}

fn ui(p: &mut Setup) {
    p.page_fn(
        PageSpec {
            id: "vless-server",
            title: "VLESS server",
            icon: Icon::Shield,
            order: 55,
        },
        |ui, cx| {
            let snap = cx.snapshot().clone();
            let c = (*snap.get(&CONFIG)).clone();
            let clients = snap.get(&CLIENTS);
            let counter = if c.enabled {
                format!("On · {}", kit::plural(clients.len(), "client", "clients"))
            } else {
                "Off".into()
            };
            kit::page_header(ui, "VLESS server", &counter, |ui| {
                let mut on = c.enabled;
                if kit::toggle_named(ui, &mut on, "", "VLESS server").changed() {
                    cx.submit("enabled", SetEnabled(on));
                }
            });

            if let Some(why) = problem(&snap).filter(|_| c.enabled) {
                kit::banner(ui, kit::Tone::Warning, why, None);
            }
            kit::page_scroll(ui, "vless-server", |ui| body(ui, cx, &c));
        },
    );
    status_chip(p);
}

fn body(ui: &mut egui::Ui, cx: &mut UiCx, c: &Settings) {
    kit::settings_card(ui, |ui| {
        kit::settings_row(
            ui,
            "Domain",
            "Your domain; its DNS record points to this machine",
            |ui| {
                if let Some(d) = kit::field(ui, "domain", &c.domain, "example.com", 190.) {
                    let d = d.trim().trim_end_matches('.').to_lowercase();
                    cx.update("domain", &CONFIG, move |s| s.domain = d);
                }
            },
        );
        kit::settings_row(ui, "Port", "", |ui| {
            if let Some(port) = kit::field(ui, "port", &c.port.to_string(), "443", 70.) {
                match port.trim().parse::<u16>() {
                    Ok(p) if p > 0 => cx.update("port", &CONFIG, move |s| s.port = p),
                    _ => cx.notify_error("Enter a port from 1 to 65535."),
                }
            }
        });
        let mut website = c.website;
        kit::settings_row(
            ui,
            "Website",
            "What visitors and probes without a key see",
            |ui| {
                if kit::segmented(
                    ui,
                    &mut website,
                    &[(Website::Builtin, "Built-in"), (Website::Own, "My own")],
                ) {
                    cx.update("website", &CONFIG, move |s| s.website = website);
                }
            },
        );
        if c.website == Website::Own {
            kit::settings_row(
                ui,
                "Site address",
                "Your HTTPS server for this domain, host:port",
                |ui| {
                    if let Some(own) =
                        kit::field(ui, "own-site", &c.own_site, "127.0.0.1:8443", 190.)
                    {
                        cx.update("own-site", &CONFIG, move |s| {
                            s.own_site = own.trim().to_owned()
                        });
                    }
                },
            );
        } else {
            kit::settings_row(
                ui,
                "Certificate email",
                "Optional; Let's Encrypt writes here before a certificate expires",
                |ui| {
                    if let Some(email) = kit::field(ui, "email", &c.email, "you@example.com", 190.)
                    {
                        cx.update("email", &CONFIG, move |s| s.email = email.trim().to_owned());
                    }
                },
            );
            let mut redirect = c.redirect_http;
            kit::settings_row(
                ui,
                "Answer on port 80",
                "Redirect plain HTTP to HTTPS, like a normal site",
                |ui| {
                    if kit::toggle(ui, &mut redirect, "").changed() {
                        cx.update("redirect", &CONFIG, move |s| s.redirect_http = redirect);
                    }
                },
            );
        }
    });
    ui.add_space(theme::SECTION_GAP);
    kit::card(ui, |ui| status_rows(ui, cx));
    ui.add_space(theme::SECTION_GAP);

    kit::section(
        ui,
        "Clients",
        Some("Each client gets its own link. Remove a client to revoke its access."),
    );
    ListEditor::new(&CLIENTS)
        .unordered()
        .add_form(
            "Add client",
            || Client {
                id: Id::random(),
                name: String::new(),
            },
            |ui, c, width| {
                let r = kit::input(
                    ui,
                    "name",
                    &mut c.name,
                    "Client name, e.g. Phone",
                    width,
                    false,
                );
                kit::submitted(ui, &r) && !c.name.trim().is_empty()
            },
        )
        .column(Column::edit("Name", "Client name", |c: &mut Client| {
            &mut c.name
        }))
        .column(
            Column::custom("Link", 64., |ui, cx, client: &Client| {
                let ready = valid_domain(&cx.get(&CONFIG).domain);
                if kit::icon_button(ui, Icon::Copy, "Copy link", ready).clicked() {
                    ui.ctx().copy_text(link(cx.snapshot(), client));
                    cx.notify("Link copied");
                }
                let open = shown_qr(ui) == Some(client.id);
                if kit::icon_button(
                    ui,
                    Icon::QrCode,
                    if open { "Hide QR code" } else { "Show QR code" },
                    ready,
                )
                .clicked()
                {
                    ui.data_mut(|d| d.insert_temp(qr_key(), (!open).then_some(client.id)));
                }
            })
            .right(),
        )
        .empty(Icon::Users, "No clients")
        .show(ui, cx);
    // The QR code of one client, large enough for a phone camera.
    let snap = cx.snapshot().clone();
    let client =
        shown_qr(ui).and_then(|id| snap.get(&CLIENTS).iter().find(|c| c.id == id).cloned());
    if let Some(client) = client.filter(|_| valid_domain(&c.domain)) {
        kit::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = theme::SPACE_XL;
                kit::qr_code(ui, &link(&snap, &client), 220.);
                ui.vertical(|ui| {
                    ui.label(kit::bold(client.name.clone(), theme::BODY));
                    ui.add_space(theme::SPACE_M);
                    if kit::Button::secondary("Copy link")
                        .icon(Icon::Copy)
                        .show(ui)
                        .clicked()
                    {
                        ui.ctx().copy_text(link(&snap, &client));
                        cx.notify("Link copied");
                    }
                });
            });
        });
    }
}

fn qr_key() -> egui::Id {
    egui::Id::unique("vless-server-qr")
}
/// The client whose QR code is shown.
fn shown_qr(ui: &egui::Ui) -> Option<Id> {
    ui.data(|d| d.get_temp::<Option<Id>>(qr_key())).flatten()
}

fn status_chip(p: &mut Setup) {
    p.fill(&STATUS, "vless-server", 30, |ui, cx, _| {
        let snap = cx.snapshot().clone();
        if !snap.get(&CONFIG).enabled {
            return;
        }
        let pal = kit::colors(ui);
        let failed =
            cx.core_status().listener_errors.contains_key("vless") || problem(&snap).is_some();
        let color = if failed { pal.danger } else { pal.success };
        cx.status_chip(
            ui,
            kit::Lead::Icon(Icon::Shield, color),
            "VLESS server",
            "This machine accepts VLESS clients",
        );
    });
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
