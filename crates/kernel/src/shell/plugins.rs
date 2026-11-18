//! The Plugins page: turn features on and off.
use super::*;

/// Where a plugin's own UI lives, as a link for its row in the plugin manager.
pub(super) fn plugin_link(cx: &UiCx, plugin: usize) -> Option<(&'static str, String)> {
    let reg = &cx.k.reg;
    if let Some(page) = reg.pages.iter().find(|e| e.plugin == plugin) {
        return Some((page.id, format!("Open {}", page.spec.title)));
    }
    // A zone on another plugin's page ("servers.toolbar" → the Servers page).
    for (slot, _) in reg.fills.iter().filter(|(_, p)| *p == plugin) {
        let owner = slot.split('.').next().unwrap_or_default();
        if let Some(page) = reg.pages.iter().find(|e| e.id == owner) {
            return Some((page.id, format!("Open {}", page.spec.title)));
        }
    }
    reg.fills
        .iter()
        .any(|(slot, p)| *p == plugin && *slot == SETTINGS_SECTIONS.id)
        .then(|| (SETTINGS_PAGE, "Open Settings".to_owned()))
}

pub(super) fn plugin_manager(ui: &mut egui::Ui, cx: &mut UiCx) {
    let p = theme::palette(ui);
    let saved = cx.k.saved_toggles.lock().unwrap().clone();
    let mut categories: Vec<crate::Category> = Vec::new();
    for pl in &cx.k.catalog {
        if !categories.iter().any(|c| c.id == pl.category.id) {
            categories.push(pl.category);
        }
    }
    let mut navigate = None;
    for cat in categories {
        // Required plugins have no switch: they are part of the app, not listed.
        let members: Vec<_> =
            cx.k.catalog
                .iter()
                .filter(|pl| pl.category.id == cat.id && !pl.required)
                .collect();
        if members.is_empty() {
            continue;
        }
        kit::section_note(ui, cat.name, "", None);
        kit::settings_card(ui, |ui| {
            for pl in members {
                let running = cx.k.reg.plugins.iter().position(|r| r.id == pl.id);
                let mut on = crate::kernel::is_enabled(pl, &saved);
                // Enabled but not running: either a missing dependency (reason) or a pending restart.
                let reason =
                    cx.k.disabled_reasons
                        .iter()
                        .find(|(id, _)| id == pl.id)
                        .map(|(_, why)| why.clone())
                        .filter(|_| on && running.is_none());
                // Not for this system: the plugin's own platforms, no switch.
                let unsupported =
                    (!pl.supported()).then(|| pl.platforms_note().unwrap_or_default());
                let restart = on != running.is_some() && reason.is_none() && unsupported.is_none();
                let link = running.and_then(|i| plugin_link(cx, i));
                let mut changed = false;
                kit::settings_item(
                    ui,
                    pl.id,
                    |ui| {
                        // The name opens the plugin's UI; no extra line per row.
                        let name = ui.add(
                            egui::Label::new(RichText::new(pl.name).color(p.foreground))
                                .sense(egui::Sense::click()),
                        );
                        if let Some((page, text)) = &link {
                            let name = name
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(text);
                            if name.clicked() {
                                navigate = Some(*page);
                            }
                        }
                        if !pl.description.is_empty() {
                            let what = RichText::new(pl.description)
                                .size(theme::SMALL)
                                .color(p.muted);
                            ui.add(egui::Label::new(what).wrap());
                        }
                        if let Some(only) = &unsupported {
                            ui.label(
                                RichText::new(format!("Not available on this system · {only}"))
                                    .size(theme::SMALL)
                                    .color(p.muted),
                            );
                        } else if let Some(why) = &reason {
                            kit::tone_text(ui, format!("Inactive · {why}"), Tone::Warning);
                        }
                    },
                    |ui| {
                        if restart {
                            kit::badge(ui, "Restart required", Tone::Warning);
                        }
                        if unsupported.is_some() {
                            return;
                        }
                        changed = kit::toggle_named(ui, &mut on, "", pl.name).changed();
                    },
                );
                if changed && let Err(e) = cx.k.set_plugin(pl.id, on) {
                    cx.notify_error(&e.message);
                }
            }
        });
    }
    if let Some(page) = navigate {
        cx.s.page = page;
    }
}
