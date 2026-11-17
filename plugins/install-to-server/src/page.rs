use crate::{DEPLOYMENTS, Deployment, Install, STATUS, Status, deploy::Options};
use rproxy_kernel::{
    prelude::*,
    ui::{Column, ListEditor, Page},
};

const FIELD: f32 = 190.;

#[derive(Default)]
pub struct DeployPage {
    options: Options,
    installing: bool,
}

impl Page for DeployPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let servers = cx.get(&DEPLOYMENTS).len();
        let counter = kit::count(servers, "server", "servers");
        kit::page_header(ui, "Install to server", &counter, |_| {});
        kit::page_scroll(ui, "install-to-server", |ui| {
            self.form(ui, cx);
            ui.add_space(kit::theme::SECTION_GAP);
            kit::section(ui, "Installed servers", None);
            servers_list(ui, cx, self.installing);
        });
    }
}

impl DeployPage {
    fn form(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let installing = cx.job_for(&Install(self.options.clone())).is_some();
        // Secrets leave the form once a server is installed, not after a failure.
        if self.installing && !installing && *cx.live(&STATUS) == Status::Idle {
            self.options.password.clear();
            self.options.panel_password.clear();
        }
        self.installing = installing;
        let o = &mut self.options;
        ui.add_enabled_ui(!installing, |ui| {
            kit::settings_card(ui, |ui| {
                text(ui, "Server address", "IP address or host name of your Linux server", &mut o.host, "203.0.113.10");
                text(ui, "SSH login", "root, or a user who may use sudo", &mut o.login, "root");
                secret(ui, "SSH password", "Used for this installation only; never saved", &mut o.password, "");
                text(ui, "Panel login", "For the server's web panel", &mut o.panel_login, "admin");
                secret(ui, "Panel password", "", &mut o.panel_password, "Generated if empty");
                kit::settings_row(
                    ui,
                    "Turn off IPv6",
                    "Traffic leaves the server over IPv4 only. Leave IPv6 on if the server has no IPv4 address.",
                    |ui| drop(kit::toggle(ui, &mut o.disable_ipv6, "")),
                );
            });
            kit::disclosure(ui, "install-to-server-more", "More settings", |ui| {
                kit::settings_card(ui, |ui| {
                    text(ui, "Domain", "panel.<domain> and auth.<domain> must point to the server. Empty: an sslip.io name of the server's address, no setup needed.", &mut o.domain, "Automatic");
                    text(ui, "Certificate email", "Optional; Let's Encrypt writes here before a certificate expires", &mut o.email, "you@example.com");
                    port(ui, cx, "SSH port", &mut o.ssh_port);
                    port(ui, cx, "Panel port", &mut o.https_port);
                    text(ui, "Folder", "A new folder on the server for the installation", &mut o.directory, "/srv/rproxy");
                    text(ui, "Container image", "", &mut o.image, "");
                });
            });
        });
        ui.add_space(kit::theme::SPACE_M);
        let problem = self.options.problem();
        ui.horizontal(|ui| {
            let options = &self.options;
            let r = cx.job_button(ui, Icon::Globe, "Install", problem.is_none(), || {
                Install(options.clone())
            });
            if let Some(problem) = problem {
                r.on_disabled_hover_text(problem);
            }
            // Nothing to fix is shown for a form that was not touched yet.
            match &*cx.live(&STATUS) {
                Status::Working(step) if installing => kit::tone_text(ui, step, kit::Tone::Neutral),
                _ if installing => kit::tone_text(ui, "Starting…", kit::Tone::Neutral),
                Status::Failed(why) => kit::tone_text(ui, why, kit::Tone::Danger),
                _ if *options != Options::default() => {
                    kit::tone_text(ui, problem.unwrap_or_default(), kit::Tone::Neutral)
                }
                _ => return,
            };
        });
    }
}

fn text(ui: &mut egui::Ui, label: &str, help: &str, value: &mut String, placeholder: &str) {
    row(ui, label, help, FIELD, |ui| {
        kit::input(ui, label, value, placeholder, FIELD, false);
    });
}
fn secret(ui: &mut egui::Ui, label: &str, help: &str, value: &mut String, placeholder: &str) {
    row(ui, label, help, FIELD, |ui| {
        kit::input(ui, label, value, placeholder, FIELD, true);
    });
}
fn port(ui: &mut egui::Ui, cx: &mut UiCx, label: &str, value: &mut u16) {
    row(ui, label, "", 70., |ui| {
        if let Some(text) = kit::field(ui, label, &value.to_string(), "", 70.) {
            match text.trim().parse() {
                Ok(port) if port > 0 => *value = port,
                _ => cx.notify_error("Enter a port from 1 to 65535."),
            }
        }
    });
}
/// A settings row whose input has its full width from the first frame (a
/// text input alone only grows into the room the row gave it last time).
fn row(ui: &mut egui::Ui, label: &str, help: &str, width: f32, input: impl FnOnce(&mut egui::Ui)) {
    kit::settings_row(ui, label, help, |ui| {
        ui.set_min_width(width);
        input(ui);
    });
}

fn servers_list(ui: &mut egui::Ui, cx: &mut UiCx, installing: bool) {
    ListEditor::new(&DEPLOYMENTS)
        .unordered()
        .no_headers()
        .column(Column::text("Server", |d: &Deployment| d.host.clone()))
        .column(Column::custom("", 2. * 30., actions).right())
        .subtitle(move |all, i| {
            let d = &all[i];
            let panel = d.panel.trim_start_matches("https://");
            Some(if d.ready {
                (format!("{panel} · login {}", d.login), kit::Tone::Neutral)
            } else if installing {
                ("Installing…".to_owned(), kit::Tone::Neutral)
            } else {
                (
                    "The installation was interrupted. Open the panel to check the server, or remove it here and install again.".to_owned(),
                    kit::Tone::Warning,
                )
            })
        })
        .empty(
            Icon::Globe,
            "Nothing installed yet",
            "Enter your server's SSH access above and press Install.",
        )
        .show(ui, cx);
}

fn actions(ui: &mut egui::Ui, cx: &mut UiCx, d: &Deployment) {
    if kit::icon_button(ui, Icon::Key, "Copy panel password", true).clicked() {
        ui.ctx().copy_text(d.password.clone());
        cx.notify("Password copied");
    }
    if kit::icon_button(ui, Icon::Open, "Open panel", true).clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&d.panel));
    }
}
