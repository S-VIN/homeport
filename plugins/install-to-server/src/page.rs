use crate::{
    DEPLOYMENTS, Deployment, Install, LOG, PROGRESS, REMOVAL, Status, Uninstall, deploy::Options,
};
use homeport_kernel::{
    prelude::*,
    ui::{Column, ListEditor, Page},
};
use std::cell::Cell;

const FIELD: f32 = 190.;

#[derive(Default)]
pub struct DeployPage {
    options: Options,
    installing: bool,
    /// The server being uninstalled, with the SSH access the user enters.
    removal: Option<Removal>,
}

struct Removal {
    id: Id,
    login: String,
    password: String,
    /// Uninstall was pressed: the last outcome is about this server.
    started: bool,
}

impl Page for DeployPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let servers = cx.get(&DEPLOYMENTS).len();
        let counter = kit::count(servers, "server", "servers");
        let uninstalling =
            (self.removal.as_ref()).and_then(|r| cx.job_for(&uninstall(r)).map(|_| r.id));
        cx.page_header(ui, &counter, |_, _| {});
        kit::page_scroll(ui, "install-to-server", |ui| {
            self.form(ui, cx, uninstalling.is_some());
            output(ui, cx);
            kit::section(ui, "Installed servers", None);
            let chosen = Cell::new(None);
            servers_list(ui, cx, self.installing, uninstalling, &chosen);
            if let Some(d) = chosen.take() {
                self.removal = Some(Removal {
                    id: d.id,
                    login: d.ssh_login,
                    password: String::new(),
                    started: false,
                });
            }
            self.removal_card(ui, cx, uninstalling.is_some());
        });
    }
}

fn uninstall(r: &Removal) -> Uninstall {
    Uninstall {
        id: r.id,
        login: r.login.clone(),
        password: r.password.clone(),
    }
}

impl DeployPage {
    fn form(&mut self, ui: &mut egui::Ui, cx: &mut UiCx, uninstalling: bool) {
        let installing = cx.job_for(&Install(self.options.clone())).is_some();
        // Secrets leave the form once a server is installed, not after a failure.
        if self.installing && !installing && *cx.live(&PROGRESS) == Status::Idle {
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
                    text(ui, "Folder", "On the server; ~/ is the home folder of the SSH login. Installing again to it updates the installation and keeps its data.", &mut o.directory, "~/homeport");
                    text(ui, "Container image", "", &mut o.image, "");
                });
            });
        });
        ui.add_space(kit::theme::SPACE_M);
        let problem = self.options.problem();
        // The form's main action on the right, like every page's; its status beside it.
        let row = egui::vec2(ui.available_width(), kit::theme::CONTROL);
        ui.allocate_ui_with_layout(
            row,
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                let options = &self.options;
                // While it runs, the button stops it.
                if installing {
                    cx.job_button(ui, Icon::Terminal, "Install", true, || {
                        Install(options.clone())
                    });
                } else {
                    let r = kit::Button::primary("Install")
                        .icon(Icon::Terminal)
                        .enabled(problem.is_none() && !uninstalling)
                        .show(ui);
                    if r.clicked() {
                        cx.run(Install(options.clone()));
                    }
                    if let Some(problem) = problem {
                        r.on_disabled_hover_text(problem);
                    }
                }
                // Nothing to fix is shown for a form that was not touched yet.
                match &*cx.live(&PROGRESS) {
                    Status::Working(step) if installing => {
                        kit::tone_text(ui, step, kit::Tone::Neutral)
                    }
                    _ if installing => kit::tone_text(ui, "Starting…", kit::Tone::Neutral),
                    Status::Failed(why) => kit::tone_text(ui, why, kit::Tone::Danger),
                    _ if *options != Options::default() => {
                        kit::tone_text(ui, problem.unwrap_or_default(), kit::Tone::Neutral)
                    }
                    _ => return,
                };
            },
        );
    }
}

/// What the installation does and what the server answers, newest at the bottom.
fn output(ui: &mut egui::Ui, cx: &mut UiCx) {
    let lines = cx.live(&LOG);
    if lines.is_empty() {
        return;
    }
    let pal = kit::colors(ui);
    ui.add_space(kit::theme::SPACE_M);
    kit::card(ui, |ui| {
        ui.horizontal(|ui| {
            kit::caption(ui, "Server output");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::icon_button(ui, Icon::Copy, "Copy output", true).clicked() {
                    ui.ctx().copy_text(lines.join("\n"));
                    cx.notify("Output copied");
                }
            });
        });
        egui::ScrollArea::vertical()
            .id_salt("install-output")
            .max_height(200.)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.;
                let font = egui::FontId::monospace(kit::theme::SMALL);
                for line in lines.iter() {
                    // Steps and the outcome stand out from the server's own lines.
                    let color = match line.chars().next() {
                        Some('▸') => pal.foreground,
                        Some('✔') => pal.success,
                        Some('✖') => pal.danger,
                        _ => pal.muted,
                    };
                    let text = egui::RichText::new(line).font(font.clone()).color(color);
                    ui.add(egui::Label::new(text).wrap());
                }
            });
    });
}

fn text(ui: &mut egui::Ui, label: &str, help: &str, value: &mut String, placeholder: &str) {
    row(ui, label, help, FIELD, |ui| {
        kit::input(ui, label, value, placeholder, FIELD);
    });
}
fn secret(ui: &mut egui::Ui, label: &str, help: &str, value: &mut String, placeholder: &str) {
    row(ui, label, help, FIELD, |ui| {
        kit::password(ui, label, value, placeholder, FIELD);
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

impl DeployPage {
    /// What uninstalling does, the SSH access for it, and the confirmation.
    fn removal_card(&mut self, ui: &mut egui::Ui, cx: &mut UiCx, running: bool) {
        let Some(removal) = &mut self.removal else {
            return;
        };
        let Some(host) = (cx.get(&DEPLOYMENTS).iter())
            .find(|d| d.id == removal.id)
            .map(|d| d.host.clone())
        else {
            // Uninstalled, or its row was removed.
            self.removal = None;
            return;
        };
        let mut close = false;
        ui.add_space(kit::theme::SPACE_M);
        kit::card(ui, |ui| {
            ui.label(kit::bold(
                format!("Uninstall from {host}"),
                kit::theme::BODY,
            ));
            kit::tone_text(
                ui,
                "Removes homeport and its data from the server, and what the installer added: Docker and other packages, package sources, the IPv6 setting. Then the server leaves this list.",
                kit::Tone::Warning,
            );
            ui.add_space(kit::theme::SPACE_S);
            ui.add_enabled_ui(!running, |ui| {
                kit::settings_card(ui, |ui| {
                    text(ui, "SSH login", "", &mut removal.login, "root");
                    secret(
                        ui,
                        "SSH password",
                        "Used to uninstall only; never saved",
                        &mut removal.password,
                        "",
                    );
                });
            });
            ui.add_space(kit::theme::SPACE_M);
            let row = egui::vec2(ui.available_width(), kit::theme::CONTROL);
            ui.allocate_ui_with_layout(
                row,
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    if running {
                        cx.job_button(ui, Icon::Uninstall, "Uninstall", true, || {
                            uninstall(removal)
                        });
                    } else {
                        let ready =
                            !removal.login.trim().is_empty() && !removal.password.is_empty();
                        let r = kit::Button::danger("Uninstall")
                            .icon(Icon::Uninstall)
                            .enabled(ready && !self.installing)
                            .show(ui);
                        if r.clicked() {
                            removal.started = true;
                            cx.run(uninstall(removal));
                        }
                        if kit::Button::ghost("Cancel").show(ui).clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape))
                        {
                            close = true;
                        }
                    }
                    match &*cx.live(&REMOVAL) {
                        Status::Working(step) if running => {
                            kit::tone_text(ui, step, kit::Tone::Neutral)
                        }
                        _ if running => kit::tone_text(ui, "Starting…", kit::Tone::Neutral),
                        Status::Failed(why) if removal.started => {
                            kit::tone_text(ui, why, kit::Tone::Danger)
                        }
                        _ => return,
                    };
                },
            );
        });
        if close {
            self.removal = None;
        }
    }
}

fn servers_list(
    ui: &mut egui::Ui,
    cx: &mut UiCx,
    installing: bool,
    uninstalling: Option<Id>,
    chosen: &Cell<Option<Deployment>>,
) {
    let busy = installing || uninstalling.is_some();
    ListEditor::new(&DEPLOYMENTS)
        .unordered()
        .no_headers()
        .column(Column::text("Server", |d: &Deployment| d.host.clone()))
        .column(
            Column::custom("", 3. * 30., move |ui, cx, d| {
                actions(ui, cx, d, busy, chosen)
            })
            .right(),
        )
        .subtitle(move |all, i| {
            let d = &all[i];
            let panel = d.panel.trim_start_matches("https://");
            Some(if uninstalling == Some(d.id) {
                ("Uninstalling…".to_owned(), kit::Tone::Neutral)
            } else if d.ready {
                (format!("{panel} · login {}", d.login), kit::Tone::Neutral)
            } else if installing {
                ("Installing…".to_owned(), kit::Tone::Neutral)
            } else {
                (
                    "Not finished. Press Install with the same settings to continue where it stopped.".to_owned(),
                    kit::Tone::Warning,
                )
            })
        })
        .empty(
            Icon::Terminal,
            "Nothing installed yet",
            "Enter your server's SSH access above and press Install.",
        )
        .show(ui, cx);
}

fn actions(
    ui: &mut egui::Ui,
    cx: &mut UiCx,
    d: &Deployment,
    busy: bool,
    chosen: &Cell<Option<Deployment>>,
) {
    // Far from the row's Delete, which only forgets the server here.
    let label = "Uninstall from the server: removes homeport and what its installer added";
    if kit::icon_button(ui, Icon::Uninstall, label, !busy).clicked() {
        chosen.set(Some(d.clone()));
    }
    if kit::icon_button(ui, Icon::Key, "Copy panel password", true).clicked() {
        ui.ctx().copy_text(d.password.clone());
        cx.notify("Password copied");
    }
    if kit::icon_button(ui, Icon::Open, "Open panel", true).clicked() {
        ui.ctx().open_url(egui::OpenUrl::new_tab(&d.panel));
    }
}
