use crate::{
    ACTIVITY, Check, Clean, Connect, HEALTH, Health, Install, KEY_FILE, LOG, Logs, RELEASES,
    Restart, SERVERS, Server, Status,
    deploy::{Access, Settings},
};
use homeport_kernel::{prelude::*, ui::Page};
use std::collections::HashSet;

const FIELD: f32 = 190.;

#[derive(Default)]
pub struct ServersPage {
    /// The connect form; the address may end with `:port`.
    address: String,
    login: String,
    password: String,
    /// Connect was pressed with something missing: the form says what.
    tried: bool,
    /// The action form open on one server.
    open: Option<(Id, Form)>,
    /// Servers whose panel was checked since the page opened.
    checked: HashSet<Id>,
}

enum Form {
    Install { settings: Settings, sudo: String },
    Restart { sudo: String },
    Logs { sudo: String },
    Clean { sudo: String },
}
impl Form {
    fn sudo(&mut self) -> &mut String {
        match self {
            Self::Install { sudo, .. }
            | Self::Restart { sudo }
            | Self::Logs { sudo }
            | Self::Clean { sudo } => sudo,
        }
    }
}

impl Page for ServersPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let servers = cx.get(&SERVERS);
        let counter = kit::count(servers.len(), "server", "servers");
        let activity = cx.live(&ACTIVITY);
        let busy = matches!(activity.status, Status::Working(_));
        // Every installed server's panel once per visit.
        for s in servers.iter().filter(|s| s.installed() && s.ready) {
            if self.checked.insert(s.id) {
                cx.run(Check(s.id));
            }
        }
        if let Some((id, _)) = &self.open
            && !servers.iter().any(|s| s.id == *id)
        {
            self.open = None;
        }
        cx.page_header(ui, &counter, |_, _| {});
        kit::page_scroll(ui, "install-to-server", |ui| {
            self.connect_form(ui, cx, busy, &activity);
            if servers.is_empty() {
                ui.add_space(kit::theme::SPACE_M);
                kit::card(ui, |ui| {
                    kit::empty(ui, Icon::Terminal, "No servers yet");
                });
            }
            for server in servers.iter() {
                ui.add_space(kit::theme::SPACE_M);
                self.server_card(ui, cx, server, busy, &activity);
            }
            output(ui, cx);
        });
    }
}

impl ServersPage {
    fn access(&self) -> Access {
        let address = self.address.trim();
        // `host:port`, but not a bare IPv6 address.
        let (host, port) = match address.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, port.parse().unwrap_or(0)),
            _ => (address, 22),
        };
        Access {
            host: host.trim_matches(['[', ']']).to_owned(),
            ssh_port: port,
            login: self.login.trim().to_owned(),
            password: self.password.clone(),
        }
    }

    fn connect_form(
        &mut self,
        ui: &mut egui::Ui,
        cx: &mut UiCx,
        busy: bool,
        activity: &crate::Activity,
    ) {
        if self.login.is_empty() && self.address.is_empty() {
            self.login = "root".into();
        }
        let access = self.access();
        let connecting = cx.job_for(&Connect(access.clone())).is_some();
        let problem = access.problem();
        if problem.is_none() {
            self.tried = false;
        }
        let missing = |v: &str| self.tried && v.trim().is_empty();
        let (host, login) = (missing(&self.address), missing(&self.login));
        ui.add_enabled_ui(!connecting, |ui| {
            kit::settings_card(ui, |ui| {
                kit::invalid_scope(ui, host, |ui| {
                    text(
                        ui,
                        "Server address",
                        "IP address or host name; add :port for another SSH port",
                        &mut self.address,
                        "203.0.113.10",
                    )
                });
                kit::invalid_scope(ui, login, |ui| {
                    text(
                        ui,
                        "SSH login",
                        "root, or a user who may use sudo",
                        &mut self.login,
                        "root",
                    )
                });
                secret(
                    ui,
                    "SSH password",
                    "Used once to let Homeport's own SSH key in; never saved",
                    &mut self.password,
                    "",
                );
            });
        });
        ui.add_space(kit::theme::SPACE_M);
        action_row(ui, |ui| {
            if connecting {
                cx.job_button(ui, Icon::Plug, "Connect", true, || Connect(access.clone()));
            } else {
                let r = kit::Button::primary("Connect")
                    .icon(Icon::Plug)
                    .enabled(!busy)
                    .show(ui);
                if r.clicked() {
                    match problem {
                        None => cx.run(Connect(access.clone())),
                        Some(_) => self.tried = true,
                    }
                }
            }
            match &activity.status {
                Status::Working(step) if connecting => {
                    drop(kit::tone_text(ui, step, kit::Tone::Neutral))
                }
                Status::Failed(why) if activity.server.is_none() => {
                    drop(kit::tone_text(ui, why, kit::Tone::Danger))
                }
                _ if self.tried => drop(kit::tone_text(
                    ui,
                    problem.unwrap_or_default(),
                    kit::Tone::Danger,
                )),
                _ => {}
            }
        });
        // The password leaves the form once the server is connected.
        if !connecting
            && !self.password.is_empty()
            && activity.server.is_none()
            && activity.status == Status::Idle
            && cx
                .get(&SERVERS)
                .iter()
                .any(|s| s.host == access.host && s.ssh_login == access.login)
        {
            self.password.clear();
        }
    }

    fn server_card(
        &mut self,
        ui: &mut egui::Ui,
        cx: &mut UiCx,
        s: &Server,
        busy: bool,
        activity: &crate::Activity,
    ) {
        let mine = activity.server == Some(s.id);
        let health = cx.live(&HEALTH).get(&s.id).cloned();
        kit::card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(kit::bold(&s.host, kit::theme::TITLE));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Forgetting only takes the card away; Clean up changes the server.
                    if kit::icon_button(
                        ui,
                        Icon::Delete,
                        "Forget this server (nothing changes on it)",
                        !busy || !mine,
                    )
                    .clicked()
                    {
                        cx.submit(("forget", s.id), Remove::new(&SERVERS, s.id));
                    }
                    if s.installed()
                        && kit::icon_button(ui, Icon::Open, "Open panel", true).clicked()
                    {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&s.panel));
                    }
                    let checking = matches!(health, Some(Health::Checking));
                    if kit::icon_button(ui, Icon::Refresh, "Check again", !checking).clicked() {
                        cx.run(Check(s.id));
                    }
                });
            });
            let ssh = match s.ssh_port {
                22 => format!("{}@{}", s.ssh_login, s.host),
                port => format!("{}@{}:{port}", s.ssh_login, s.host),
            };
            kit::property_with(ui, "SSH", |ui| {
                kit::tone_text(ui, &ssh, kit::Tone::Neutral);
                let tip = "Copy the command that signs in with Homeport's key";
                if kit::icon_button(ui, Icon::Copy, tip, true).clicked() {
                    ui.ctx().copy_text(ssh_command(s, &cx.live(&KEY_FILE)));
                    cx.notify("SSH command copied");
                }
            });
            if !s.system.is_empty() {
                kit::property(ui, "System", &s.system);
            }
            if let Some(release) = cx.live(&RELEASES).get(&s.id).filter(|_| s.installed()) {
                kit::property_with(ui, "Version", |ui| {
                    kit::tone_text(ui, &release.version, kit::Tone::Neutral);
                    if release.update {
                        kit::tone_text(ui, "Update available", kit::Tone::Accent).on_hover_text(
                            "A newer image is published. Press Update to install it.",
                        );
                    }
                });
            }
            if s.installed() {
                kit::property(ui, "Panel", &s.panel);
                kit::property(ui, "Login", &s.login);
                kit::property_with(ui, "Password", |ui| {
                    kit::tone_text(ui, "••••••••", kit::Tone::Neutral);
                    if kit::icon_button(ui, Icon::Copy, "Copy panel password", true).clicked() {
                        ui.ctx().copy_text(s.password.clone());
                        cx.notify("Password copied");
                    }
                });
            }
            status(ui, s, mine, &activity.status, health.as_ref());
            ui.add_space(kit::theme::SPACE_S);
            self.actions(ui, cx, s, busy, mine);
            if let Some((id, form)) = &mut self.open
                && *id == s.id
            {
                let close = action_form(ui, cx, s, form, busy);
                if close {
                    self.open = None;
                }
            }
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui, cx: &mut UiCx, s: &Server, busy: bool, mine: bool) {
        // Which form is open on this server: 0 none, 1 install, 2 restart,
        // 3 clean up, 4 logs.
        let open = match &self.open {
            Some((id, Form::Install { .. })) if *id == s.id => 1,
            Some((id, Form::Restart { .. })) if *id == s.id => 2,
            Some((id, Form::Clean { .. })) if *id == s.id => 3,
            Some((id, Form::Logs { .. })) if *id == s.id => 4,
            _ => 0,
        };
        action_row(ui, |ui| {
            let label = if s.installed() && s.ready {
                "Update"
            } else {
                "Install"
            };
            let selected = open == 1;
            if kit::Button::primary(label)
                .icon(Icon::Terminal)
                .selected(selected)
                .enabled(!busy)
                .show(ui)
                .clicked()
            {
                self.open = (!selected).then(|| {
                    let form = Form::Install {
                        settings: Settings::of(s),
                        sudo: String::new(),
                    };
                    (s.id, form)
                });
            }
            if s.installed() {
                let selected = open == 2;
                let r = kit::Button::secondary("Restart")
                    .icon(Icon::Refresh)
                    .selected(selected)
                    .enabled(!busy)
                    .show(ui);
                if r.clicked() {
                    if s.sudo_password {
                        self.open = (!selected).then(|| {
                            (
                                s.id,
                                Form::Restart {
                                    sudo: String::new(),
                                },
                            )
                        });
                    } else {
                        cx.run(Restart {
                            id: s.id,
                            sudo: String::new(),
                        });
                    }
                }
            }
            if s.installed() {
                let selected = open == 4;
                let r = kit::Button::secondary("Logs")
                    .icon(Icon::Logs)
                    .selected(selected)
                    .enabled(!busy)
                    .show(ui);
                if r.clicked() {
                    let sudo = String::new();
                    match s.sudo_password {
                        true => self.open = (!selected).then_some((s.id, Form::Logs { sudo })),
                        false => cx.run(Logs { id: s.id, sudo }),
                    }
                }
            }
            let selected = open == 3;
            let r = kit::Button::secondary("Clean up")
                .icon(Icon::Broom)
                .selected(selected)
                .enabled(!busy)
                .show(ui);
            if r.clicked() {
                self.open = (!selected).then(|| {
                    (
                        s.id,
                        Form::Clean {
                            sudo: String::new(),
                        },
                    )
                });
            }
            // A running action of this server: its button stops it.
            if mine && busy {
                let running = [
                    cx.job_for(&Restart {
                        id: s.id,
                        sudo: String::new(),
                    }),
                    cx.job_for(&Clean {
                        id: s.id,
                        sudo: String::new(),
                    }),
                    cx.job_for(&Logs {
                        id: s.id,
                        sudo: String::new(),
                    }),
                    cx.job_for(&Install {
                        id: s.id,
                        settings: Settings::default(),
                        sudo: String::new(),
                    }),
                ];
                if let Some(run) = running.into_iter().flatten().next()
                    && kit::icon_button(ui, Icon::Stop, "Stop", true).clicked()
                {
                    cx.cancel(run.id);
                }
            }
        });
    }
}

/// What the server does now, or how it is.
fn status(ui: &mut egui::Ui, s: &Server, mine: bool, now: &Status, health: Option<&Health>) {
    let pal = kit::colors(ui);
    let (icon, color, text) = match (now, health) {
        (Status::Working(step), _) if mine => (Icon::Activity, pal.muted, step.clone()),
        (Status::Failed(why), _) if mine => (Icon::Error, pal.danger, why.clone()),
        _ if !s.installed() => (Icon::Circle, pal.muted, "Homeport is not installed".into()),
        _ if !s.ready => (
            Icon::Warning,
            pal.warning,
            "The installation was not finished. Press Install to continue it.".into(),
        ),
        (_, Some(Health::Works)) => (Icon::Check, pal.success, "The panel works".into()),
        (_, Some(Health::Fails(why))) => (Icon::Error, pal.danger, why.clone()),
        _ => (Icon::Activity, pal.muted, "Checking…".into()),
    };
    kit::property_status(ui, "Status", icon, color, &text);
}

/// The open action's settings and its confirmation; true closes it.
fn action_form(ui: &mut egui::Ui, cx: &mut UiCx, s: &Server, form: &mut Form, busy: bool) -> bool {
    let mut close = false;
    ui.add_space(kit::theme::SPACE_M);
    ui.add_enabled_ui(!busy, |ui| {
        let problem = match form {
            Form::Install { settings: o, sudo } => {
                kit::settings_card(ui, |ui| {
                    text(ui, "Panel login", "", &mut o.panel_login, "admin");
                    let keep = if s.installed() { "Unchanged if empty" } else { "Generated if empty" };
                    secret(ui, "Panel password", "", &mut o.panel_password, keep);
                    kit::settings_row(
                        ui,
                        "Turn off IPv6",
                        "Traffic leaves the server over IPv4 only. Leave IPv6 on if the server has no IPv4 address.",
                        |ui| drop(kit::toggle(ui, &mut o.disable_ipv6, "")),
                    );
                    if s.sudo_password {
                        sudo_row(ui, sudo);
                    }
                });
                kit::disclosure(ui, ("install-to-server-more", s.id), "More settings", |ui| {
                    kit::settings_card(ui, |ui| {
                        text(ui, "Domain", "panel.<domain> and auth.<domain> must point to the server. Empty: an sslip.io name of the server's address, no setup needed.", &mut o.domain, "Automatic");
                        text(ui, "Certificate email", "Optional; Let's Encrypt writes here before a certificate expires", &mut o.email, "you@example.com");
                        port(ui, "Panel port", &mut o.https_port);
                        text(ui, "Folder", "On the server; ~/ is the home folder of the SSH login. Installing again to it updates the installation and keeps its data.", &mut o.directory, "~/homeport");
                        text(ui, "Container image", "", &mut o.image, "");
                    });
                });
                o.problem(s.ssh_port)
            }
            Form::Clean { .. } => {
                kit::tone_text(
                    ui,
                    "Removes Homeport and its data from the server, and what the installer added: Docker and other packages, package sources, the IPv6 setting, the panel file and Homeport's SSH key. Then the server leaves this list.",
                    kit::Tone::Warning,
                );
                if s.sudo_password {
                    ui.add_space(kit::theme::SPACE_S);
                    kit::settings_card(ui, |ui| sudo_row(ui, form.sudo()));
                }
                None
            }
            Form::Restart { .. } | Form::Logs { .. } => {
                kit::settings_card(ui, |ui| sudo_row(ui, form.sudo()));
                None
            }
        };
        let needs_sudo = s.sudo_password && form.sudo().is_empty();
        ui.add_space(kit::theme::SPACE_M);
        action_row(ui, |ui| {
            let button = match form {
                Form::Install { .. } if s.installed() && s.ready => {
                    kit::Button::primary("Update").icon(Icon::Terminal)
                }
                Form::Install { .. } => kit::Button::primary("Install").icon(Icon::Terminal),
                Form::Restart { .. } => kit::Button::primary("Restart").icon(Icon::Refresh),
                Form::Logs { .. } => kit::Button::primary("Show logs").icon(Icon::Logs),
                Form::Clean { .. } => kit::Button::danger("Clean up").icon(Icon::Broom),
            };
            let ready = problem.is_none() && !needs_sudo;
            if button.enabled(ready && !busy).show(ui).clicked() {
                let (id, sudo) = (s.id, form.sudo().clone());
                match form {
                    Form::Install { settings, .. } => cx.run(Install {
                        id,
                        settings: settings.clone(),
                        sudo,
                    }),
                    Form::Restart { .. } => cx.run(Restart { id, sudo }),
                    Form::Logs { .. } => cx.run(Logs { id, sudo }),
                    Form::Clean { .. } => cx.run(Clean { id, sudo }),
                }
                close = true;
            }
            if kit::Button::ghost("Cancel").show(ui).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                close = true;
            }
            if let Some(problem) = problem {
                kit::tone_text(ui, problem, kit::Tone::Danger);
            } else if needs_sudo {
                kit::tone_text(ui, "Enter the password for sudo.", kit::Tone::Neutral);
            }
        });
    });
    close
}

/// `ssh` with homeport's key, to sign in to the server by hand.
fn ssh_command(s: &Server, key: &std::path::Path) -> String {
    let port = match s.ssh_port {
        22 => String::new(),
        port => format!(" -p {port}"),
    };
    format!(
        "ssh -i \"{}\"{port} {}@{}",
        key.display(),
        s.ssh_login,
        s.host
    )
}

fn sudo_row(ui: &mut egui::Ui, sudo: &mut String) {
    secret(
        ui,
        "Password for sudo",
        "Used for this action only; never saved",
        sudo,
        "",
    );
}

/// What the last action does and what the server answers, newest at the bottom.
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
            .max_height(320.)
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

/// A row with the main action on the right, like every page's; its status beside it.
fn action_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let row = egui::vec2(ui.available_width(), kit::theme::CONTROL);
    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), add);
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
fn port(ui: &mut egui::Ui, label: &str, value: &mut u16) {
    let mut port = Ok(None);
    row(ui, label, "", 70., |ui| {
        port = kit::checked_field(ui, label, &value.to_string(), "", 70., kit::port);
    });
    match port {
        Ok(Some(p)) => *value = p,
        Err(e) => drop(kit::field_error(ui, &e, None)),
        Ok(None) => {}
    }
}
/// A settings row whose input has its full width from the first frame (a
/// text input alone only grows into the room the row gave it last time).
fn row(ui: &mut egui::Ui, label: &str, help: &str, width: f32, input: impl FnOnce(&mut egui::Ui)) {
    kit::settings_row(ui, label, help, |ui| {
        ui.set_min_width(width);
        input(ui);
    });
}
