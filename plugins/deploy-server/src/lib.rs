//! Password-authenticated SSH provisioning of the bundled Compose stack.
use rproxy_kernel::{
    kit,
    prelude::*,
    ui::{Page, Request},
};
use serde::{Deserialize, Serialize};
pub mod deploy;

pub static PLUGIN: Plugin = Plugin {
    id: "deploy-server",
    name: "Deploy server",
    description: "Install rproxy on a Linux server over SSH and keep its connection links.",
    requires: &["subscriptions"],
    platforms: Platform::DESKTOP,
    setup,
    ..Plugin::DEFAULT
};
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Deployment {
    pub host: String,
    pub directory: String,
    pub panel: String,
    pub connection: String,
    pub username: String,
    pub password: String,
    pub status: String,
}
pub static DEPLOYMENTS: State<Vec<Deployment>> = State::new("deploy-server.deployments");
pub static PROGRESS: State<String> = State::new("deploy-server.progress");
pub struct Install(pub deploy::Options);
impl Op for Install {
    type Reply = ();
    const SAVES: bool = true;
}
fn setup(p: &mut Setup) -> Result<()> {
    p.setting(&DEPLOYMENTS);
    p.live(&PROGRESS, String::new);
    let directory = p.data_dir();
    // Blocking SSH owns its lifetime: never expose a misleading job cancel button.
    let lock = std::sync::Arc::new(std::sync::Mutex::new(()));
    p.handle(move |cx: Cx, Install(options): Install| {
        let directory = directory.clone();
        let lock = lock.clone();
        async move {
            let worker = cx.clone();
            tokio::task::spawn_blocking(move || {
                let _guard = lock
                    .try_lock()
                    .map_err(|_| Error::msg("An installation is already running."))?;
                deploy::install(
                    options,
                    &directory,
                    |stage| worker.set_live(&PROGRESS, stage.to_owned()),
                    |record| {
                        // Preserve recovery links before starting containers, even if health checks fail.
                        tokio::runtime::Handle::current().block_on(cx.write(move |w| {
                            w.update(&DEPLOYMENTS, |items| {
                                if let Some(existing) = items.iter_mut().find(|r| {
                                    r.host == record.host && r.directory == record.directory
                                }) {
                                    *existing = record;
                                } else {
                                    items.push(record);
                                }
                            });
                            Ok(())
                        }))
                    },
                )
            })
            .await
            .map_err(|e| Error::msg(e.to_string()))??;
            Ok(())
        }
    });
    p.page::<DeployPage>(PageSpec {
        id: "deploy-server",
        title: "Deploy server",
        icon: Icon::Servers,
        order: 85,
    });
    Ok(())
}
#[derive(Default)]
struct DeployPage {
    options: deploy::Options,
    error: Option<String>,
    busy: bool,
}
impl Page for DeployPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        if self.busy
            && let Request::Done(result) = cx.request::<()>("install")
        {
            self.busy = false;
            self.error = result.err();
        }
        kit::page_header(
            ui,
            "Deploy server",
            &kit::plural(cx.get(&DEPLOYMENTS).len(), "installation", "installations"),
            |_| {},
        );
        kit::card(ui, |ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                field(ui, "SSH host", &mut self.options.host, false);
                field(ui, "SSH login", &mut self.options.login, false);
                field(ui, "SSH password", &mut self.options.password, true);
                field(
                    ui,
                    "Domain (auto or your domain)",
                    &mut self.options.domain,
                    false,
                );
                ui.collapsing("Installation settings", |ui| {
                    port(ui, "SSH port", &mut self.options.ssh_port);
                    port(ui, "Panel HTTPS port", &mut self.options.https_port);
                    port(ui, "VLESS port", &mut self.options.proxy_port);
                    port(ui, "Internal panel port", &mut self.options.ui_port);
                    port(ui, "Internal login port", &mut self.options.auth_port);
                    field(ui, "Install directory", &mut self.options.directory, false);
                    field(ui, "Container image", &mut self.options.image, false);
                    field(ui, "Panel login", &mut self.options.admin, false);
                    field(
                        ui,
                        "Panel password (blank generates one)",
                        &mut self.options.admin_password,
                        true,
                    );
                    field(ui, "ACME email (optional)", &mut self.options.email, false);
                });
                if kit::Button::primary("Install server").show(ui).clicked() {
                    cx.submit("install", Install(self.options.clone()));
                    self.options.password.clear();
                    self.options.admin_password.clear();
                    self.busy = true;
                    self.error = None;
                }
            });
            if self.busy {
                ui.spinner();
            }
            let status = cx.snapshot().live(&PROGRESS);
            if !status.is_empty() {
                ui.label(status.as_str());
            }
        });
        for (index, record) in cx.get(&DEPLOYMENTS).iter().enumerate() {
            ui.push_id(index, |ui| {
                kit::card(ui, |ui| {
                    ui.strong(&record.host);
                    kit::property(ui, "Status", &record.status);
                    kit::property(ui, "Directory", &record.directory);
                    kit::property(ui, "Panel", &record.panel);
                    kit::property(ui, "Connection", &record.connection);
                    kit::property(ui, "Panel login", &record.username);
                    ui.horizontal(|ui| {
                        if kit::Button::secondary("Copy panel password")
                            .show(ui)
                            .clicked()
                        {
                            ui.ctx().copy_text(record.password.clone());
                        }
                        if kit::Button::secondary("Add to client").show(ui).clicked() {
                            cx.submit(
                                ("add", index),
                                Add::new(
                                    &plugin_subscriptions::SOURCES,
                                    plugin_subscriptions::Source::new(&record.connection),
                                ),
                            );
                        }
                    });
                })
            });
        }
        if let Some(error) = self.error.clone() {
            let mut open = true;
            egui::Window::new("Installation failed")
                .open(&mut open)
                .collapsible(false)
                .vscroll(true)
                .show(ui.ctx(), |ui| {
                    ui.label(&error);
                    if kit::Button::secondary("Copy error").show(ui).clicked() {
                        ui.ctx().copy_text(error.clone());
                    }
                });
            if !open {
                self.error = None;
            }
        }
    }
}
fn field(ui: &mut egui::Ui, label: &str, value: &mut String, password: bool) {
    kit::property_with(ui, label, |ui| {
        ui.add(egui::TextEdit::singleline(value).password(password));
    });
}
fn port(ui: &mut egui::Ui, label: &str, value: &mut u16) {
    kit::property_with(ui, label, |ui| {
        ui.add(egui::DragValue::new(value).range(1..=65535));
    });
}
