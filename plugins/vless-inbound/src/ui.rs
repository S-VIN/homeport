//! The VLESS inbound page (settings, status, clients and their links) and
//! its status chip.
use crate::*;
use homeport_kernel::{
    kit::{self, theme},
    ui::{Field, ListEditor},
};
use plugin_routing as routing;
use plugin_servers as servers;

/// The routing pattern of this inbound's clients.
const CLIENTS_RULE: &str = "inbound:vless";

fn status_rows(ui: &mut egui::Ui, cx: &mut UiCx) {
    let snap = cx.snapshot().clone();
    let c = snap.get(&CONFIG);
    let pal = kit::colors(ui);
    let core = cx.core_status();
    match core.listener_errors.get("vless") {
        Some(e) => kit::property_status(
            ui,
            "Listener",
            Icon::Error,
            pal.danger,
            &format!(
                "{e}{}",
                if cfg!(target_os = "linux") && c.port < 1024 {
                    " Ports below 1024 need root or CAP_NET_BIND_SERVICE."
                } else {
                    ""
                }
            ),
        ),
        None if listener(&snap).is_some() && core.running() => kit::property_status(
            ui,
            "Listener",
            Icon::Connected,
            pal.success,
            &format!("Listening on port {}", c.port),
        ),
        None => kit::property_status(ui, "Listener", Icon::Info, pal.muted, "Off"),
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
                    "Let's Encrypt failed: {e}. Check that {} points to this device and port {} is reachable.",
                    c.domain, c.port
                ),
            ),
        };
        kit::property_status(ui, "Certificate", icon, color, &text);
    }
    if routing::route_for(&snap, CLIENTS_RULE) == routing::Route::Direct {
        kit::property_status(
            ui,
            "Client traffic",
            Icon::SignOut,
            pal.success,
            "Leaves from this device",
        );
        return;
    }
    // Where the main route sends it: the selected server, or what a plugin
    // put in front of it (e.g. a chain exit), after the routing rules.
    let (tone, icon, via) = servers::route_text(&snap);
    // Nothing to alarm about while nobody can connect.
    let tone = if c.enabled { tone } else { kit::Tone::Neutral };
    let text = if tone == kit::Tone::Neutral && c.enabled {
        format!("Follows Routing; {}", via.to_lowercase())
    } else {
        via.to_owned()
    };
    kit::property_status(ui, "Client traffic", icon, pal.tone(tone), &text);
    // Under the text, at its start: side by side they overflow a phone.
    kit::property_action(ui, |ui| {
        if kit::Button::ghost("Send directly")
            .tone(kit::Tone::Accent)
            .show(ui)
            .on_hover_text("Adds the routing rule VLESS clients → Direct at the top; this device's own traffic keeps its rules")
            .clicked()
        {
            let direct = routing::SetRoute {
                pattern: CLIENTS_RULE.into(),
                action: routing::Route::Direct,
            };
            cx.submit("exit-rule", direct);
        }
    });
}

pub(crate) fn register(p: &mut Setup) {
    p.page::<VlessPage>(PageSpec {
        title: "VLESS inbound",
        icon: Icon::Shield,
        order: 33,
    });
    status_chip(p);
}

/// The client whose QR code is shown, kept per window.
#[derive(Default)]
struct VlessPage {
    qr: Option<Id>,
}
impl Page for VlessPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let snap = cx.snapshot().clone();
        let c = (*snap.get(&CONFIG)).clone();
        let clients = snap.get(&CLIENTS);
        let counter = if c.enabled {
            format!("On · {}", kit::plural(clients.len(), "client", "clients"))
        } else {
            "Off".into()
        };
        cx.page_header(ui, &counter, |_, _| {});

        if let Some(why) = problem(&snap).filter(|_| c.enabled) {
            kit::banner(ui, kit::Tone::Warning, why, None);
        }
        let qr = std::cell::Cell::new(self.qr);
        kit::page_scroll(ui, "vless-inbound", |ui| body(ui, cx, &c, &qr));
        self.qr = qr.get();
    }
}

fn body(ui: &mut egui::Ui, cx: &mut UiCx, c: &Settings, qr: &std::cell::Cell<Option<Id>>) {
    server_settings(ui, cx, c);
    // Off says so once, in the header and the switch.
    if c.enabled {
        kit::card(ui, |ui| status_rows(ui, cx));
    }
    kit::section(
        ui,
        "Clients",
        Some("Each client gets its own link. Remove a client to revoke its access."),
    );
    clients(ui, cx, qr);
    shown_client(ui, cx, c, qr.get());
}

fn server_settings(ui: &mut egui::Ui, cx: &mut UiCx, c: &Settings) {
    kit::settings_card(ui, |ui| {
        let mut on = c.enabled;
        // The page title already names it: the row says what the switch does.
        kit::settings_row(ui, "Enabled", "Accept VLESS clients on this device", |ui| {
            if kit::toggle_named(ui, &mut on, "", "VLESS inbound").changed() {
                cx.submit("enabled", SetEnabled(on));
            }
        });
        field_row(
            ui,
            "Domain",
            "Your domain; its DNS record points to this device",
            true,
            |ui| {
                if let Some(d) = kit::field(ui, "domain", &c.domain, "example.com", 190.) {
                    let d = d.trim().trim_end_matches('.').to_lowercase();
                    cx.update("domain", &CONFIG, move |s| s.domain = d);
                }
            },
        );
        let mut port = Ok(None);
        field_row(ui, "Port", "", true, |ui| {
            port = kit::checked_field(ui, "port", &c.port.to_string(), "443", 70., kit::port);
        });
        match port {
            Ok(Some(p)) => cx.update("port", &CONFIG, move |s| s.port = p),
            Err(e) => drop(kit::field_error(ui, &e, None)),
            Ok(None) => {}
        }
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
            field_row(
                ui,
                "Site address",
                "Your HTTPS server for this domain, host:port",
                true,
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
            field_row(
                ui,
                "Certificate email",
                "Let's Encrypt writes here before a certificate expires",
                false,
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
}

/// A settings row with a text field, marked as required or optional.
fn field_row(
    ui: &mut egui::Ui,
    label: &str,
    help: &str,
    required: bool,
    control: impl FnOnce(&mut egui::Ui),
) {
    let pal = kit::colors(ui);
    kit::settings_item(
        ui,
        label,
        |ui| {
            ui.horizontal(|ui| {
                let r = ui.label(egui::RichText::new(label).color(pal.foreground));
                if !help.is_empty() {
                    r.on_hover_text(help);
                }
                let mark = if required { "Required" } else { "Optional" };
                ui.label(
                    egui::RichText::new(mark)
                        .size(theme::SMALL)
                        .color(pal.faint),
                );
            });
        },
        control,
    );
}

fn clients(ui: &mut egui::Ui, cx: &mut UiCx, qr: &std::cell::Cell<Option<Id>>) {
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
                    "Subscription link name, e.g. Phone",
                    width,
                );
                kit::submitted(ui, &r) && !c.name.trim().is_empty()
            },
        )
        .column(Field::edit(
            "Name",
            "Subscription link name",
            |c: &mut Client| &mut c.name,
        ))
        .column(
            Field::custom("Link", 64., |ui, cx, client: &Client| {
                let ready = valid_domain(&cx.get(&CONFIG).domain);
                const NO_DOMAIN: &str = "Enter the server's domain first";
                if kit::icon_button(ui, Icon::Copy, "Copy link", ready)
                    .on_disabled_hover_text(NO_DOMAIN)
                    .clicked()
                {
                    ui.ctx().copy_text(link(cx.snapshot(), client));
                    cx.notify("Link copied");
                }
                let open = qr.get() == Some(client.id);
                if kit::icon_button(
                    ui,
                    Icon::QrCode,
                    if open { "Hide QR code" } else { "Show QR code" },
                    ready,
                )
                .on_disabled_hover_text(NO_DOMAIN)
                .clicked()
                {
                    qr.set((!open).then_some(client.id));
                }
            })
            .right(),
        )
        .empty(
            Icon::Users,
            "No clients",
            "Add a client to get its link and QR code.",
        )
        .show(ui, cx);
}

/// The QR code of one client, large enough for a phone camera.
fn shown_client(ui: &mut egui::Ui, cx: &mut UiCx, c: &Settings, qr: Option<Id>) {
    let snap = cx.snapshot().clone();
    let client = qr.and_then(|id| snap.get(&CLIENTS).iter().find(|c| c.id == id).cloned());
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

fn status_chip(p: &mut Setup) {
    p.fill(&STATUS, "vless-inbound", 30, |ui, cx, _| {
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
            "VLESS inbound",
            "This device accepts VLESS clients",
        );
    });
}
