//! The Settings page: General, a section per plugin, configuration files and About.
use super::*;

pub(super) fn settings(ui: &mut egui::Ui, cx: &mut UiCx) {
    kit::section(ui, "General", None);
    let mut theme_choice = *cx.get(&THEME);
    kit::settings_card(ui, |ui| {
        kit::settings_row(ui, "Theme", "", |ui| {
            if kit::segmented(
                ui,
                &mut theme_choice,
                &[
                    (Theme::System, "System"),
                    (Theme::Light, "Light"),
                    (Theme::Dark, "Dark"),
                ],
            ) {
                let k = cx.k.clone();
                tokio::spawn(async move {
                    let _ = k
                        .write(None, |w| {
                            w.set(&THEME, theme_choice);
                            Ok(())
                        })
                        .await;
                });
            }
        });
        if cx.s.can_exit {
            kit::settings_row(
                ui,
                "Application",
                "Closing the window keeps rproxy running in the background",
                |ui| {
                    if kit::Button::secondary("Quit rproxy").show(ui).clicked() {
                        cx.s.exit_requested = true;
                    }
                },
            );
        }
    });
    // Each plugin's settings under its own name, in plugin order.
    let list: Vec<_> = cx.k.reg.contributions(&SETTINGS_SECTIONS).to_vec();
    let mut plugins: Vec<usize> = Vec::new();
    for c in &list {
        if !plugins.contains(&c.plugin) {
            plugins.push(c.plugin);
        }
    }
    for plugin in plugins {
        kit::section(ui, cx.k.reg.plugins[plugin].name, None);
        kit::settings_card(ui, |ui| {
            for c in list.iter().filter(|c| c.plugin == plugin) {
                ui.push_id(&c.id, |ui| {
                    cx.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, &()))
                });
            }
        });
    }
    kit::section(ui, "About", None);
    let status = cx.core_status();
    let p = theme::palette(ui);
    kit::card(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.;
        kit::property(
            ui,
            "Version",
            &format!("rproxy {}", env!("CARGO_PKG_VERSION")),
        );
        core_row(ui, cx, &status);
        let dir = cx.k.data_dir.display().to_string();
        kit::property_with(ui, "Data folder", |ui| {
            let w = ui.available_width() - theme::CONTROL - theme::HORIZONTAL_GAP;
            ui.allocate_ui_with_layout(vec2(w, 20.), Layout::left_to_right(Align::Center), |ui| {
                kit::middle(ui, &dir, p.foreground);
            });
            if kit::icon_button(ui, Icon::Copy, "Copy path", true).clicked() {
                ui.ctx().copy_text(dir.clone());
                cx.notify("Path copied");
            }
        });
    });
}

/// Explicit portable configuration import and export; no hidden backup schedule.
pub(super) fn configuration(ui: &mut egui::Ui, cx: &mut UiCx) {
    use crate::configuration::FileAction;
    kit::page_header(ui, "Settings", "", |ui| {
        if kit::Button::secondary("Import…")
            .icon(Icon::Up)
            .enabled(!cx.s.file_busy)
            .show(ui)
            .clicked()
        {
            cx.s.file_action = Some(FileAction::Import);
            cx.s.file_busy = true;
        }
        if kit::Button::secondary("Export…")
            .icon(Icon::Down)
            .enabled(!cx.s.file_busy)
            .show(ui)
            .on_hover_text("Includes subscription links and credentials.")
            .clicked()
        {
            cx.s.file_action = Some(FileAction::Export);
            cx.s.file_busy = true;
        }
    });

    if cx.s.checking.is_some() {
        kit::hint(ui, "Checking the configuration…");
    } else if cx.s.file_busy {
        kit::hint(ui, "File dialog open");
    }
    if let Some(config) = cx.s.incoming.clone() {
        kit::separator(ui);
        kit::tone_text(
            ui,
            "Replace current settings? Traffic will pause while the configuration is applied.",
            Tone::Warning,
        );
        kit::truncated(
            ui,
            format!("{} · {}", config.name, kit::bytes(config.size as u64)),
            None,
        );
        ui.horizontal_wrapped(|ui| {
            if kit::Button::ghost("Cancel").show(ui).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                cx.s.incoming = None;
            }
            if kit::Button::danger("Replace settings").show(ui).clicked() {
                cx.s.incoming = None;
                (cx.k.restore)(config);
            }
        });
    }
}

/// The core version with update actions: check, update, or go back to the bundled core.
pub(super) fn core_row(ui: &mut egui::Ui, cx: &mut UiCx, status: &crate::core::CoreStatus) {
    let k = cx.k.clone();
    let running = k
        .board()
        .runs
        .iter()
        .find(|r| r.key == "kernel/core-update" && r.phase.active())
        .cloned();
    let updates = k.core_updates();
    let installed = updates.installed;
    let current = status.version.clone().unwrap_or_default();
    let version = crate::updates::version_of(&current)
        .map(|v| v.iter().map(u32::to_string).collect::<Vec<_>>().join("."));
    let label = match &version {
        Some(v) if installed => format!("mihomo {v} · updated"),
        Some(v) => format!("mihomo {v}"),
        None => "Offline".to_owned(),
    };
    let available = updates
        .latest
        .as_ref()
        .filter(|r| crate::updates::newer(&r.version, &current))
        .cloned();
    // The version on the left, its actions at the right end of the same row.
    kit::property_with(ui, "Core", |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let result = if let Some(run) = &running {
                let text = match run.total {
                    Some(total) if total > 0 => format!("{}%", run.done * 100 / total),
                    _ => "Working…".to_owned(),
                };
                kit::spinner(ui);
                kit::caption(ui, text);
                Ok(())
            } else if let Some(r) = &available {
                if kit::Button::secondary(&format!("Update to {}", r.version))
                    .show(ui)
                    .clicked()
                {
                    k.install_core_update()
                } else {
                    Ok(())
                }
            } else {
                let mut result = Ok(());
                if kit::Button::secondary("Check for update")
                    .show(ui)
                    .clicked()
                {
                    result = k.check_core_update();
                }
                if installed
                    && kit::Button::ghost("Use bundled")
                        .show(ui)
                        .on_hover_text("Remove the update and run the core shipped with rproxy")
                        .clicked()
                {
                    result = k.remove_core_update();
                }
                result
            };
            if let Err(e) = result {
                k.notify_error("Core update", &e.message);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let ink = kit::colors(ui).foreground;
                kit::truncated(ui, RichText::new(&label).color(ink), None);
            });
        });
    });
    if let (Some(at), None, None) = (updates.checked, &available, &running) {
        kit::property(ui, "", &format!("Up to date · checked {}", kit::age(at)));
    }
}
