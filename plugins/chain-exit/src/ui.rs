//! The Chain exit page (the route, its settings and the exit list) and its
//! status chip.
use crate::*;
use homeport_kernel::{
    kit::{self, Tone, theme},
    ui::{Field, ListEditor},
};

fn latency_text(snap: &Snapshot) -> (String, Tone) {
    match &*snap.live(&LAST_CHECK) {
        None => ("Not tested yet".into(), Tone::Neutral),
        Some((Ok(ms), at)) => (
            format!("{ms} ms through both hops · {}", kit::age(*at)),
            Tone::Neutral,
        ),
        Some((Err(_), at)) => (
            format!("No answer through both hops · {}", kit::age(*at)),
            Tone::Danger,
        ),
    }
}

/// `This device → entry → exit → Internet` as one line of steps.
fn route_line(ui: &mut egui::Ui, snap: &Snapshot, plan: &Plan) {
    let pal = kit::colors(ui);
    let entry = servers::selected(snap).map(|s| s.name);
    let direct = !through_entry(snap);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4., 4.);
        let step = |ui: &mut egui::Ui, text: &str, color: egui::Color32| {
            ui.label(egui::RichText::new(text).color(color));
        };
        let arrow = |ui: &mut egui::Ui| {
            kit::icon(ui, Icon::ChevronRight, 12., pal.faint);
        };
        step(ui, "This device", pal.muted);
        arrow(ui);
        if !direct {
            match &entry {
                Some(name) => step(ui, name, pal.foreground),
                None => step(ui, "No server selected", pal.danger),
            }
            arrow(ui);
        }
        step(ui, &plan.name(), pal.foreground);
        arrow(ui);
        step(ui, "Internet", pal.muted);
    });
}

pub(crate) fn register(p: &mut Setup) {
    p.page_fn(
        PageSpec {
            title: "Chain exit",
            icon: Icon::SignOut,
            order: 45,
        },
        |ui, cx| {
            let snap = cx.snapshot().clone();
            let exits = snap.get(&EXITS);
            let current = plan(&snap);
            let counter = match &current {
                Some(p) => format!("Using {}", p.name()),
                None if exits.is_empty() && snap.get(&WARP_ACCOUNT).is_none() => String::new(),
                None => "Off".into(),
            };
            cx.page_header(ui, &counter, |ui, cx| {
                if current.is_some() {
                    cx.job_button(ui, Icon::Gauge, "Test", cx.core_status().running(), || {
                        CheckExit
                    });
                }
            });

            kit::page_scroll(ui, "chain-exit", |ui| body(ui, cx, &snap, current.as_ref()));
        },
    );
    status(p);
}

fn body(ui: &mut egui::Ui, cx: &mut UiCx, snap: &Snapshot, current: Option<&Plan>) {
    let settings = (*snap.get(&SETTINGS)).clone();
    if let Some(plan) = current {
        kit::card(ui, |ui| {
            route_line(ui, snap, plan);
            let (text, tone) = latency_text(snap);
            kit::tone_text(ui, text, tone);
        });
    }
    // Like every page: the add bar and list first, then the settings.
    exits(ui, cx);
    route_settings(ui, cx, snap, &settings);
}

fn route_settings(ui: &mut egui::Ui, cx: &mut UiCx, snap: &Snapshot, settings: &Settings) {
    kit::settings_card(ui, |ui| {
        let mut entry = settings.entry;
        kit::settings_row(
            ui,
            "Reach the exit",
            "Through the current server (two hops) or directly from this device. If the exit is unavailable, proxied traffic stops.",
            |ui| {
                if kit::segmented(
                    ui,
                    &mut entry,
                    &[
                        (Entry::Selected, "Via current server"),
                        (Entry::Direct, "Directly"),
                    ],
                ) {
                    cx.update("entry", &SETTINGS, move |s| s.entry = entry);
                }
            },
        );
        match &*snap.get(&WARP_ACCOUNT) {
            None => kit::settings_row(
                ui,
                "Cloudflare WARP",
                "A free exit on a Cloudflare address, or a fallback when the exit server is down",
                |ui| {
                    cx.job_button(ui, Icon::Add, "Set up WARP", true, || SetUpWarp);
                },
            ),
            Some(_) => {
                let mut use_warp = settings.warp;
                kit::settings_row(
                    ui,
                    "Cloudflare WARP",
                    "If exit fails: used when the exit server does not answer. Always: WARP is the exit",
                    |ui| {
                        if kit::Button::ghost("Remove").show(ui).clicked() {
                            cx.submit("remove-warp", RemoveWarp);
                        }
                        if kit::segmented(
                            ui,
                            &mut use_warp,
                            &[
                                (WarpUse::Off, "Off"),
                                (WarpUse::Backup, "If exit fails"),
                                (WarpUse::Only, "Always"),
                            ],
                        ) {
                            cx.update("warp", &SETTINGS, move |s| s.warp = use_warp);
                        }
                    },
                );
            }
        }
    });
}

fn exits(ui: &mut egui::Ui, cx: &mut UiCx) {
    ListEditor::new(&EXITS)
        .unordered()
        .add_form(
            "Add exit server",
            || Exit::new(""),
            |ui, e, width| {
                let r = kit::input(
                    ui,
                    "link",
                    &mut e.link,
                    "Exit server link (vless://, hysteria2://, WireGuard…)",
                    width,
                );
                kit::submitted(ui, &r) && !e.link.trim().is_empty()
            },
        )
        .leading("", 16., |ui, cx, e: &Exit| {
            let pal = kit::colors(ui);
            let on = *cx.get(&ACTIVE) == Some(e.id);
            kit::icon(
                ui,
                if on { Icon::Connected } else { Icon::Circle },
                16.,
                if on { pal.success } else { pal.faint },
            );
        })
        .column(Field::edit("Name", "Exit server name", |e: &mut Exit| {
            &mut e.name
        }))
        .column(Field::text("Address", Exit::address).width(150.))
        .column(
            Field::custom("", 84., |ui, cx, e: &Exit| {
                let on = *cx.get(&ACTIVE) == Some(e.id);
                let (label, next) = if on {
                    ("Turn off", None)
                } else {
                    ("Use", Some(e.id))
                };
                if kit::Button::ghost(label).show(ui).clicked() {
                    cx.submit("use", Use(next));
                }
            })
            .right(),
        )
        .empty(
            Icon::SignOut,
            "No exit server",
            "Paste the link of a server to leave through, or set up WARP.",
        )
        .show(ui, cx);
}

fn status(p: &mut Setup) {
    p.fill(&STATUS, "exit", 8, |ui, cx, _| {
        let Some(e) = plan(cx.snapshot()) else {
            return;
        };
        let pal = kit::colors(ui);
        let failed = matches!(&*cx.live(&LAST_CHECK), Some((Err(_), _)));
        let color = if failed { pal.danger } else { pal.accent };
        let text = format!("Chain · {}", e.name());
        cx.status_chip(
            ui,
            kit::Lead::Icon(Icon::SignOut, color),
            &text,
            "Traffic leaves through the exit server",
        );
    });
    p.fill(&PANELS, "exit", 15, |ui, cx, _| {
        let snap = cx.snapshot().clone();
        let Some(e) = plan(&snap) else { return };
        homeport_kernel::shell::panel_title_page(ui, cx, "Chain exit", &PLUGIN);
        route_line(ui, &snap, &e);
        let (text, tone) = latency_text(&snap);
        kit::tone_text(ui, text, tone);
    });
    p.fill(&servers::CONNECTION, "exit", 40, |ui, cx, _| {
        let Some(e) = plan(cx.snapshot()) else {
            return;
        };
        let pal = kit::colors(ui);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.;
            kit::icon(ui, Icon::SignOut, 14., pal.accent);
            let text = format!("Then via {}", e.name());
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .size(theme::SMALL)
                        .color(pal.muted),
                )
                .truncate(),
            );
        });
        ui.add_space(theme::SPACE_S);
    });
}
