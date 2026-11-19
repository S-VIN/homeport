//! The Local proxy page (address, login, this device) and its status chip
//! and panel.
use crate::*;

pub(crate) fn register(p: &mut Setup) {
    p.page::<ProxyPage>(PageSpec {
        title: "Local proxy",
        icon: Icon::Plug,
        order: 32,
    });
    p.fill(&STATUS, "listeners", 20, listeners_chip);
    p.fill(&PANELS, "listeners", 20, listeners_panel);
}

fn listeners_chip(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let p = cx.get(&PROXY);
    let pal = kit::colors(ui);
    let (color, text) = if !p.enabled {
        (pal.faint, "Proxy off".to_owned())
    } else if error(&cx.core_status()).is_some() {
        (pal.danger, "Port failed".to_owned())
    } else {
        (pal.muted, p.address())
    };
    cx.status_chip(
        ui,
        kit::Lead::Icon(Icon::Plug, color),
        &text,
        "Local proxy (HTTP and SOCKS5)",
    );
}

fn listeners_panel(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let pal = kit::colors(ui);
    homeport_kernel::shell::panel_title_page(ui, cx, "Local proxy", &PLUGIN);
    let p = cx.get(&PROXY);
    if !p.enabled {
        kit::hint(ui, "Off");
        return;
    }
    let failed = error(&cx.core_status());
    kit::row_line(ui, theme::CONTROL, |ui| {
        ui.spacing_mut().item_spacing.x = 6.;
        kit::dot(
            ui,
            if failed.is_some() {
                pal.danger
            } else {
                pal.success
            },
        );
        ui.label(p.address());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            copy_button(ui, cx, &p);
            auth_label(ui, &p);
        });
    });
    if let Some(e) = failed {
        kit::tone_text(ui, e, kit::Tone::Danger);
    }
}

fn copy_button(ui: &mut egui::Ui, cx: &mut UiCx, p: &Proxy) {
    if kit::icon_button(ui, Icon::Copy, "Copy address", p.enabled).clicked() {
        ui.ctx()
            .copy_text(mihomo::local_address(p.listen.0, p.port).to_string());
        cx.notify("Address copied");
    }
}

/// Lock icon and "Login" / "No login".
fn auth_label(ui: &mut egui::Ui, p: &Proxy) {
    let pal = kit::colors(ui);
    let (icon, text) = if p.login.is_some() {
        (Icon::Lock, "Login")
    } else {
        (Icon::Unlock, "No login")
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.;
        kit::icon(ui, icon, 14., pal.muted);
        ui.label(
            egui::RichText::new(text)
                .size(theme::SMALL)
                .color(pal.muted),
        );
    });
}

/// The address and login being edited (the password is only kept here, never shown).
#[derive(Clone, PartialEq)]
struct Draft {
    ip: String,
    port: String,
    auth: bool,
    login: String,
    password: String,
}
impl Draft {
    fn of(p: &Proxy) -> Self {
        Self {
            ip: p.listen.to_string(),
            port: p.port.to_string(),
            auth: p.login.is_some(),
            login: p.login.clone().unwrap_or_default(),
            password: String::new(),
        }
    }
    /// The proxy with these edits, or the wrong field and why.
    fn apply(&self, base: &Proxy) -> std::result::Result<Proxy, Wrong> {
        let listen = (self.ip.parse::<Listen>())
            .map_err(|_| (Some(Field::Ip), "Enter an IPv4 or IPv6 address.".to_owned()))?;
        let port = kit::port(&self.port).map_err(|e| (Some(Field::Port), e))?;
        Ok(Proxy {
            listen,
            port,
            login: self.auth.then(|| self.login.clone()),
            password: (self.auth && !self.password.is_empty()).then(|| self.password.clone()),
            ..base.clone()
        })
    }
}

#[derive(Default)]
struct ProxyPage {
    /// Edits not saved yet; `None` shows the saved values.
    draft: Option<Draft>,
    error: Option<Wrong>,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Ip,
    Port,
}
/// A field that is wrong (none: the saved address as a whole) and why.
type Wrong = (Option<Field>, String);

impl Page for ProxyPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let saved = (*cx.get(&PROXY)).clone();
        cx.page_header(ui, "", |_, _| {});
        kit::page_scroll(ui, "local-proxy", |ui| {
            self.proxy_card(ui, cx, &saved);
            device_card(ui, cx, &saved);
        });
    }
}

/// Widths of the address fields; a single field spans both.
const IP: f32 = 140.;
const PORT: f32 = 72.;
const FIELD: f32 = IP + theme::HORIZONTAL_GAP + PORT;

impl ProxyPage {
    fn proxy_card(&mut self, ui: &mut egui::Ui, cx: &mut UiCx, saved: &Proxy) {
        match cx.outcome("save") {
            Some(Ok(())) => {
                self.draft = None;
                self.error = None;
            }
            Some(Err(e)) => self.error = Some((None, e)),
            None => {}
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.draft = None;
            self.error = None;
        }
        let mut draft = self.draft.clone().unwrap_or_else(|| Draft::of(saved));
        let mut enter = false;
        let busy = cx.pending("save");
        kit::settings_card(ui, |ui| {
            let mut on = saved.enabled;
            // Named like every page's main switch: the page title says what.
            kit::settings_row(ui, "Enabled", "HTTP and SOCKS5 on one port", |ui| {
                if kit::toggle_named(ui, &mut on, "", "Local proxy").changed() {
                    cx.update("enabled", &PROXY, move |p| p.enabled = on);
                }
            });
            ui.add_enabled_ui(!busy, |ui| {
                kit::settings_row(
                    ui,
                    "Address",
                    "0.0.0.0 accepts other devices on your network",
                    |ui| {
                        // Copy after the value, as everywhere in Settings.
                        let wrong = self.error.as_ref().and_then(|e| e.0);
                        let r = kit::input(ui, "ip", &mut draft.ip, "127.0.0.1", IP);
                        enter |= kit::submitted(ui, &r);
                        if wrong == Some(Field::Ip) {
                            kit::invalid(ui, &r);
                        }
                        let r = kit::input(ui, "port", &mut draft.port, "Port", PORT);
                        enter |= kit::submitted(ui, &r);
                        if wrong == Some(Field::Port) {
                            kit::invalid(ui, &r);
                        }
                        copy_button(ui, cx, saved);
                    },
                );
                // Under the address: an edit that can't be saved, or why the
                // core could not open the port (retried once it is free).
                if let Some((_, e)) = &self.error {
                    kit::field_error(ui, e, None);
                } else if saved.enabled
                    && let Some(e) = error(&cx.core_status())
                    && kit::field_error(ui, &e, Some("Retry"))
                {
                    cx.run(RetryPort);
                }
                kit::settings_row(ui, "Require login", "", |ui| {
                    kit::toggle_named(ui, &mut draft.auth, "", "Require login");
                });
                if draft.auth {
                    kit::settings_row(ui, "Login", "", |ui| {
                        let r = kit::input(ui, "login", &mut draft.login, "user", FIELD);
                        enter |= kit::submitted(ui, &r);
                    });
                    let has = saved.login.is_some();
                    let help = if has {
                        "Leave empty to keep the saved password"
                    } else {
                        ""
                    };
                    kit::settings_row(ui, "Password", help, |ui| {
                        let hint = if has { "Unchanged" } else { "Password" };
                        let r = kit::password(ui, "password", &mut draft.password, hint, FIELD);
                        enter |= kit::submitted(ui, &r);
                    });
                }
            });
            let dirty = draft != Draft::of(saved);
            if dirty {
                ui.add_space(theme::SPACE_M);
                // On the right, under the controls they apply.
                let (save, cancel) = ui
                    .allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), theme::CONTROL),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let save = kit::Button::primary("Save")
                                .enabled(!busy)
                                .show(ui)
                                .clicked();
                            (save, kit::Button::ghost("Cancel").show(ui).clicked())
                        },
                    )
                    .inner;
                ui.add_space(theme::SPACE_M);
                if cancel {
                    self.draft = None;
                    self.error = None;
                    return;
                }
                if save || enter {
                    match draft.apply(saved) {
                        Ok(next) => {
                            self.error = None;
                            cx.submit("save", SaveProxy(next));
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            }
            self.draft = dirty.then_some(draft);
        });
    }
}

/// System proxy and TUN: how this computer itself uses homeport.
fn device_card(ui: &mut egui::Ui, cx: &mut UiCx, p: &Proxy) {
    if Platform::is_docker() {
        kit::tone_text(
            ui,
            "System proxy and TUN are not used in Docker",
            kit::Tone::Neutral,
        )
        .on_hover_text("Devices and apps connect to the proxy port directly.");
        return;
    }
    kit::settings_card(ui, |ui| {
        if !Platform::is_flatpak() {
            let mut on = p.system_proxy;
            kit::settings_row(
                ui,
                "System proxy",
                "Points the system proxy settings at this port; the previous settings come back when turned off. Works without a login only.",
                |ui| {
                    if kit::toggle_named(ui, &mut on, "", "System proxy").changed() {
                        cx.update("system-proxy", &PROXY, move |p| p.system_proxy = on);
                    }
                },
            );
            if p.system_proxy {
                let status = match (
                    &*cx.live(&SYSTEM_PROXY),
                    system_proxy_address(cx.snapshot()),
                ) {
                    (_, None) if !p.enabled => ("The proxy is off".into(), kit::Tone::Warning),
                    (_, None) => (
                        "Not set: the proxy requires a login".into(),
                        kit::Tone::Warning,
                    ),
                    (Some(Err(e)), _) => (e.clone(), kit::Tone::Danger),
                    (Some(Ok(what)), _) => (format!("Active · {what}"), kit::Tone::Neutral),
                    (None, _) => ("Applying…".into(), kit::Tone::Neutral),
                };
                kit::tone_text(ui, status.0, status.1);
            }
        }
        let mut tun = p.tun;
        kit::settings_row(
            ui,
            "TUN mode",
            "Route every app through Homeport with a virtual network interface (TUN)",
            |ui| {
                ui.add_enabled_ui(!cx.pending("tun"), |ui| {
                    if kit::toggle_named(ui, &mut tun, "", "TUN mode").changed() {
                        cx.update("tun", &PROXY, move |p| p.tun = tun);
                    }
                });
            },
        );
        if cx.pending("tun") {
            kit::tone_text(
                ui,
                "Applying TUN mode… Confirm the system authorization dialog if prompted.",
                kit::Tone::Neutral,
            );
        } else if let homeport_kernel::ui::Request::Done(Err(e)) =
            cx.request::<Update<Proxy>>("tun")
        {
            kit::tone_text(ui, e, kit::Tone::Danger);
        } else if p.tun {
            match (cx.core_status().tun_error, *cx.live(&TUN_UP)) {
                (Some(refused), _) => kit::tone_text(ui, refused, kit::Tone::Danger),
                (None, Some(true)) => kit::tone_text(ui, "Tunnel active", kit::Tone::Neutral),
                (None, Some(false)) => kit::tone_text(
                    ui,
                    "The tunnel could not start. Check the core log, then turn TUN off and on to retry.",
                    kit::Tone::Danger,
                ),
                (None, None) => kit::tone_text(ui, "Starting the tunnel…", kit::Tone::Neutral),
            };
        }
    });
}
