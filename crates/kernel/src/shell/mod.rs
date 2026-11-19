//! The window shell and its plugin zones.
//!
//! ```text
//! narrow                                       wide (≥ 960 pt)
//! ┌ STATUS chips ──────── Saved · Activity ┐   ┌ sidebar ┬ page ──────────┬ PANELS ┐
//! │        [ toasts float here ]           │   │ pages   │ header      [ toasts ]│
//! │ page: header + content                 │   │ …       │ content       │ cards  │
//! ├ navigation: pages … More ─── Settings ┤   │ Activity│               │        │
//! └────────────────────────────────────────┘   └ Settings┴───────────────┴────────┘
//! ```
//! A status chip opens its plugin's panel in a popover anchored to the chip; in
//! a wide window the panels are docked and the chips are not needed.
//! Settings contains General, a section per plugin and About; plugin
//! management has its own page.
use crate::{
    Kernel, Phase, Slot,
    kernel::{THEME, Theme},
    kit::{self, Icon, Lead, Tone, theme},
    ui::{SessionState, Toast, UiCx},
};
use egui::{Align, Align2, Layout, Rect, RichText, Stroke, pos2, vec2};
use std::sync::Arc;

mod activity;
mod nav;
mod plugins;
mod settings;
mod status;
mod toasts;
use activity::*;
use nav::*;
use plugins::*;
use settings::*;
use status::*;
pub use status::{panel_title, panel_title_page};
use toasts::*;

/// Status bar chips (top, always visible). Keep one short chip per concern;
/// draw it with [`UiCx::status_chip`], which opens the plugin's panel or page.
pub static STATUS: Slot<()> = Slot::new("shell.status");
/// Detail cards for what the chips summarize. Docked on the right in a wide
/// window, a popover under the status bar otherwise. Start with [`panel_title`].
pub static PANELS: Slot<()> = Slot::new("shell.panels");
/// A plugin's rows in Settings ([`kit::settings_row`]): each plugin gets a
/// section under its own name; the label names the setting.
pub static SETTINGS_SECTIONS: Slot<()> = Slot::new("shell.settings");

const SETTINGS_PAGE: &str = "settings";
const PLUGINS_PAGE: &str = "plugins";
/// Width of the navigation sidebar in a wide window.
const SIDEBAR: f32 = 184.;
/// The widest a page gets; very wide windows center the page and panels.
const MAX_PAGE: f32 = 1040.;

pub(crate) fn frame(root: &mut egui::Ui, k: &Arc<Kernel>, s: &mut SessionState) {
    let ctx = &root.ctx().clone();
    s.now = ctx.input(|i| i.time);
    s.wide = ctx.content_rect().width() >= theme::UTILITY_BREAKPOINT;
    apply_theme(ctx, k, s);
    for n in k.notices_after(s.last_notice) {
        s.last_notice = s.last_notice.max(n.id);
        s.toasts.push(Toast {
            notice: n,
            at: s.now,
        });
    }
    let p = if ctx.global_style().visuals.dark_mode {
        kit::palette::DARK
    } else {
        kit::palette::LIGHT
    };
    let mut cx = UiCx::new(k, s);
    let wide = cx.s.wide && cx.slot_len(&PANELS) > 0;
    // Focus order follows drawing order: status chips, then the page, then the
    // navigation. Side regions are reserved first and drawn after the page.
    let mut status_rect = Rect::NOTHING;
    if !wide {
        status_rect = egui::Panel::top("status-bar")
            .exact_size(theme::STATUS_BAR)
            .frame(
                egui::Frame::NONE
                    .fill(p.surface)
                    .inner_margin(egui::Margin::symmetric(6, 5)),
            )
            .show_separator_line(true)
            .show(root, |ui| status_bar(ui, &mut cx))
            .response
            .rect;
    }
    let navigation_rect = if wide {
        egui::Panel::left("sidebar")
            .resizable(false)
            .exact_size(SIDEBAR)
            .frame(egui::Frame::NONE.fill(p.surface))
            .show_separator_line(true)
            .show(root, |ui| ui.max_rect())
            .inner
    } else {
        egui::Panel::bottom("navigation")
            .exact_size(theme::NAVIGATION)
            .frame(egui::Frame::NONE.fill(p.surface))
            .show_separator_line(true)
            .show(root, |ui| ui.max_rect())
            .inner
    };
    // Very wide windows: the page (at most `MAX_PAGE` wide) and the panels stay
    // together as one centered block; half of the spare room goes right of the
    // panels, half left of the page.
    let spare = (root.available_width() - theme::UTILITY_WIDTH - 2. * theme::PADDING - MAX_PAGE)
        .max(0.)
        / 2.;
    let panels_rect = wide.then(|| {
        egui::Panel::right("panels")
            .resizable(false)
            .exact_size(theme::UTILITY_WIDTH + spare)
            .frame(egui::Frame::NONE.fill(p.background))
            .show_separator_line(false)
            .show(root, |ui| ui.max_rect())
            .inner
    });
    expire_toasts(&mut cx);
    egui::CentralPanel::default()
        .frame(
            egui::Frame::NONE
                .fill(p.background)
                .inner_margin(egui::Margin {
                    left: theme::PADDING as i8,
                    right: theme::PADDING as i8,
                    top: 8,
                    bottom: 4,
                }),
        )
        .show(root, |ui| {
            // Keep rows scannable instead of stretching them.
            let mut rect = ui.max_rect();
            rect.min.x += (rect.width() - MAX_PAGE).max(0.);
            if !wide {
                rect = ui
                    .max_rect()
                    .shrink2(vec2(((ui.max_rect().width() - MAX_PAGE) / 2.).max(0.), 0.));
            }
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                page(ui, &mut cx)
            });
        });
    if let Some(mut rect) = panels_rect {
        rect.max.x = rect.min.x + theme::UTILITY_WIDTH;
        let mut ui = root.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::top_down(Align::Min)),
        );
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 0,
                right: theme::PADDING as i8,
                top: 10,
                bottom: 6,
            })
            .show(&mut ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("panels-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| panels(ui, &mut cx, true, None));
            });
    }
    let activity_anchor = if wide {
        sidebar(root, &mut cx, &p, navigation_rect)
    } else {
        navigation(root, &mut cx, &p, navigation_rect)
    };
    if !wide && cx.s.panels_open {
        panels_popover(ctx, &mut cx, &p, status_rect);
    } else if wide {
        cx.s.panels_open = false;
    }
    toasts(ctx, &mut cx, &p, status_rect, panels_rect);
    if cx.s.activity_open {
        let anchor = if wide {
            activity_anchor
        } else {
            cx.s.activity_chip.unwrap_or(status_rect)
        };
        activity(ctx, &mut cx, &p, anchor, wide);
    }
    let animating = cx.s.pending_since().is_some()
        || cx.s.saved_at().is_some_and(|t| cx.s.now - t < 2.)
        || !cx.s.toasts.is_empty();
    if animating {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}

fn apply_theme(ctx: &egui::Context, k: &Kernel, s: &mut SessionState) {
    let wanted = *k.snapshot().get(&THEME);
    if s.theme != Some(wanted) {
        // `System` follows `RawInput::system_theme` every frame, live.
        ctx.set_theme(match wanted {
            Theme::System => egui::ThemePreference::System,
            Theme::Light => egui::ThemePreference::Light,
            Theme::Dark => egui::ThemePreference::Dark,
        });
        s.theme = Some(wanted);
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    // Another page opens at its top.
    let shown = egui::Id::unique("shell-shown-page");
    if ui.data(|d| d.get_temp::<&'static str>(shown)) != Some(cx.s.page) {
        ui.data_mut(|d| d.insert_temp(shown, cx.s.page));
        kit::scroll_to_top(ui);
    }
    if cx.s.page == PLUGINS_PAGE {
        // Only the plugins with a switch: required ones are part of the app.
        // What the switches say, applied or waiting for a restart.
        let saved = cx.k.saved_toggles.lock().unwrap().clone();
        let enabled = (cx.k.catalog.iter())
            .filter(|p| !p.required && p.supported() && crate::kernel::is_enabled(p, &saved))
            .count();
        let optional = cx.k.catalog.iter().filter(|p| !p.required).count();
        let pending = cx.k.restart_pending();
        let counter = match pending {
            false => format!("{enabled} of {optional} on"),
            true => format!("{enabled} of {optional} on · Restart to apply"),
        };
        // In the header, not above the list: switching never moves the rows.
        let mut restart = false;
        kit::page_header(ui, "Plugins", &counter, |ui| {
            if pending {
                restart = kit::Button::primary("Restart now")
                    .icon(Icon::Refresh)
                    .show(ui)
                    .on_hover_text("Apply plugin changes. Traffic pauses for a moment.")
                    .clicked();
            }
        });
        if restart {
            cx.k.request_restart();
        }
        kit::page_scroll(ui, "plugins", |ui| plugin_manager(ui, cx));
        return;
    }
    if cx.s.page == SETTINGS_PAGE {
        configuration(ui, cx);
        kit::page_scroll(ui, "settings", |ui| settings(ui, cx));
        return;
    }
    let Some(entry) = cx.k.reg.pages.iter().find(|p| p.id == cx.s.page) else {
        cx.s.page = cx.k.reg.pages.first().map_or(SETTINGS_PAGE, |p| p.id);
        return;
    };
    let (id, plugin, make) = (entry.id, entry.plugin, entry.make.clone());
    let mut page = cx.s.pages.remove(id).unwrap_or_else(|| make());
    ui.push_id(id, |ui| cx.scoped(Some(plugin), |cx| page.ui(ui, cx)));
    cx.s.pages.insert(id, page);
}

/// Tray icon state (desktop). The worst answer wins: Failed > Connected > Idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayLink {
    Idle,
    Connected,
    Failed,
}
pub static TRAY: crate::Hook<(), Option<TrayLink>> = crate::Hook::new("shell.tray");
