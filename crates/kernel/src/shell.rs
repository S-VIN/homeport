//! The window shell and its plugin zones.
//!
//! ```text
//! narrow                                       wide (≥ 960 pt)
//! ┌ STATUS chips ──────── Saved · Activity ┐   ┌ sidebar ┬ page ──────────┬ PANELS ┐
//! │ page: header + content                 │   │ pages   │ header        │ cards  │
//! │ toasts (own strip, never over rows)    │   │ …       │ content       │        │
//! ├ navigation: pages … More ─── Settings ┤   │ Activity│ toasts        │        │
//! └────────────────────────────────────────┘   └ Settings┴───────────────┴────────┘
//! ```
//! A status chip opens its plugin's panel in a popover anchored to the chip; in
//! a wide window the panels are docked and the chips are not needed.
//! Settings contains General and About; plugin management has its own page.
use crate::{
    Kernel, Phase, Policy, Slot,
    kernel::{THEME, Theme},
    kit::{self, Icon, Lead, Tone, theme},
    ui::{SessionState, Toast, UiCx},
};
use egui::{Align, Align2, Layout, Rect, RichText, Stroke, pos2, vec2};
use std::sync::Arc;

/// Status bar chips (top, always visible). Keep one short chip per concern;
/// call [`UiCx::toggle_panels`] on click so narrow windows can show details.
pub static STATUS: Slot<()> = Slot::new(
    "shell.status",
    "Status bar: short chips with the state at a glance",
);
/// Detail cards for what the chips summarize. Docked on the right in a wide
/// window, a popover under the status bar otherwise. Start with [`panel_title`].
pub static PANELS: Slot<()> = Slot::new(
    "shell.panels",
    "Side panel cards with details and quick actions",
);
/// Rows of the Settings → General card ([`kit::settings_row`]); the label names the setting.
pub static SETTINGS_SECTIONS: Slot<()> = Slot::new(
    "shell.settings",
    "Settings → General: one settings row per setting",
);

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
    let toast_rect = (!cx.s.toasts.is_empty() && cx.s.toast_height > 0.).then(|| {
        egui::Panel::bottom("toasts")
            .exact_size(cx.s.toast_height)
            .frame(egui::Frame::NONE.fill(p.background))
            .show_separator_line(false)
            .show(root, |ui| ui.max_rect())
            .inner
    });
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
    toasts(root, &mut cx, &p, toast_rect, navigation_rect, wide);
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
    if cx.s.activity_open {
        let anchor = if wide {
            activity_anchor
        } else {
            cx.s.activity_chip.unwrap_or(status_rect)
        };
        activity(ctx, &mut cx, &p, anchor, wide);
    }
    let animating = cx.s.pending_since().is_some()
        || cx.s.saved_at.is_some_and(|t| cx.s.now - t < 2.)
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

/// "Saving…" / "Saved ✓" after a change.
fn save_state(ui: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette) {
    if cx.k.restart_pending() {
        if kit::chip(
            ui,
            Some(Lead::Icon(Icon::Plugin, p.warning)),
            "Restart",
            "Plugin changes are waiting. Open Plugins to apply them.",
        )
        .clicked()
        {
            cx.s.page = PLUGINS_PAGE;
            cx.s.panels_open = false;
        }
        return;
    }
    let saving = cx.s.pending_since().is_some_and(|t| cx.s.now - t > 0.2);
    let saved = !saving && cx.s.saved_at.is_some_and(|t| cx.s.now - t < 2.);
    if saving || saved {
        ui.label(
            RichText::new(if saving { "Saving…" } else { "Saved" })
                .size(theme::SMALL)
                .color(if saving { p.muted } else { p.success }),
        );
        if saved {
            kit::icon(ui, Icon::Check, 13., p.success);
        }
    }
}

/// The Activity button: icon and "Activity" or "2 running".
fn activity_button(ui: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette) -> egui::Response {
    let board = cx.k.board();
    let active: Vec<_> = board
        .runs
        .iter()
        .filter(|r| r.phase.active() && !r.quiet)
        .collect();
    let failed = board.runs.iter().any(|r| r.phase == Phase::Failed);
    let text = if active.is_empty() {
        "Activity".to_owned()
    } else {
        format!("{} running", active.len())
    };
    let tip = if active.is_empty() {
        String::new()
    } else {
        active
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let ink = if !active.is_empty() {
        p.accent
    } else if failed {
        p.muted
    } else {
        p.faint
    };
    let r = kit::chip(ui, Some(Lead::Icon(Icon::Activity, ink)), &text, &tip);
    if r.clicked() {
        cx.s.activity_open = !cx.s.activity_open;
        cx.s.panels_open = false;
    }
    r
}

fn status_bar(ui: &mut egui::Ui, cx: &mut UiCx) {
    let p = theme::palette(ui);
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 2.;
        if let Some(e) = cx.core_status().error() {
            let e = e.to_owned();
            let r = kit::chip(
                ui,
                Some(Lead::Icon(Icon::Error, p.danger)),
                "Core stopped",
                &e,
            );
            if r.clicked() {
                cx.scoped(None, |cx| cx.toggle_panels_from(&r));
            }
        }
        let width = ui.available_width() - 150.;
        ui.allocate_ui_with_layout(
            vec2(width.max(80.), 24.),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                status_chips(ui, cx);
            },
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = activity_button(ui, cx, &p);
            cx.s.activity_chip = Some(r.rect);
            ui.add_space(4.);
            save_state(ui, cx, &p);
        });
    });
}

/// The status chips that fit whole, in order; the rest fold into a "+N" chip
/// that opens all panels. The first chip (the connection) always shows and
/// shortens its text when needed.
fn status_chips(ui: &mut egui::Ui, cx: &mut UiCx) {
    let list = cx.k.reg.contributions(&STATUS).to_vec();
    let mut hidden = 0;
    for (i, c) in list.iter().enumerate() {
        let key = egui::Id::unique(("status-chip", c.id.as_str()));
        // Width from the last frame; 0 for a chip that shows nothing.
        let last: f32 = ui.data(|d| d.get_temp(key)).unwrap_or(0.);
        let room = ui.available_width() - if i + 1 < list.len() { 44. } else { 0. };
        if i > 0 && (hidden > 0 || last > room) {
            hidden += usize::from(last > 0.);
            continue;
        }
        let max = if i == 0 { room } else { room.min(240.) };
        let area = Rect::from_min_size(ui.cursor().min, vec2(max.max(0.), 24.));
        let builder = egui::UiBuilder::new()
            .max_rect(area)
            .layout(Layout::left_to_right(Align::Center));
        let drawn = ui
            .scope_builder(builder, |ui| {
                cx.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, &()))
            })
            .response;
        ui.data_mut(|d| d.insert_temp(key, drawn.rect.width()));
    }
    if hidden > 0 {
        let r = kit::chip(ui, None, &format!("+{hidden}"), "More status");
        if r.clicked() {
            cx.scoped(None, |cx| cx.toggle_panels_from(&r));
        }
    }
}

/// Title row of a panel card: caption on the left, optional content on the right.
pub fn panel_title(ui: &mut egui::Ui, title: &str, right: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 22.),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(ui.available_width(), 22.));
            kit::caption(ui, title);
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        },
    );
    ui.add_space(2.);
}

/// Panel title with a "Manage" button that opens `page` (hidden on that page).
pub fn panel_title_page(ui: &mut egui::Ui, cx: &mut UiCx, title: &str, page: &'static str) {
    let mut open = false;
    panel_title(ui, title, |ui| {
        if cx.page() != page {
            open = kit::Button::ghost("Manage")
                .tone(Tone::Accent)
                .show(ui)
                .clicked();
        }
    });
    if open {
        cx.navigate(page);
    }
}

/// Panel cards. `only`: the panels of one plugin (a chip's popover); all when it has none.
fn panels(ui: &mut egui::Ui, cx: &mut UiCx, cards: bool, only: Option<usize>) {
    let all: Vec<_> = cx.k.reg.contributions(&PANELS).to_vec();
    let mine: Vec<_> = all
        .iter()
        .filter(|c| Some(c.plugin) == only)
        .cloned()
        .collect();
    let core_error = cx.core_status().error().map(str::to_owned);
    let list = if mine.is_empty() { all } else { mine };
    let mut first = true;
    let mut block = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
        if cards {
            if !kit::optional_card(ui, |ui| add(ui)) {
                return;
            }
        } else {
            if !first {
                ui.add_space(theme::SPACE_M);
                kit::separator(ui);
                ui.add_space(theme::SPACE_M);
            }
            add(ui);
        }
        first = false;
    };
    if let Some(e) = core_error.filter(|_| only.is_none() || cards) {
        let k = cx.k.clone();
        block(ui, &mut |ui| {
            panel_title(ui, "Core", |ui| {
                kit::badge(ui, "Stopped", Tone::Danger);
            });
            ui.add(egui::Label::new(RichText::new(&e).size(theme::SMALL)).wrap());
            ui.add_space(4.);
            if kit::action(ui, "Restart core", true) {
                let k = k.clone();
                tokio::spawn(async move {
                    if let Err(e) = k.restart_core().await {
                        k.notify_error("Core did not start", &e.message);
                    }
                });
            }
        });
    }
    for c in list {
        ui.push_id(&c.id, |ui| {
            block(ui, &mut |ui| {
                cx.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, &()))
            })
        });
    }
}

fn popover_frame(p: &kit::Palette) -> egui::Frame {
    egui::Frame::NONE
        .fill(p.surface)
        .stroke(Stroke::new(1_f32, p.border))
        .corner_radius(theme::RADIUS)
        .shadow(egui::epaint::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: p.shadow,
        })
        .inner_margin(12)
}

/// Narrow window: the chip's panel opens as a popover under the chip (the list keeps its height).
fn panels_popover(ctx: &egui::Context, cx: &mut UiCx, p: &kit::Palette, status: Rect) {
    let screen = ctx.content_rect();
    let width = (screen.width() - 16.).min(360.);
    let anchor = cx.s.panel_anchor;
    let chip = anchor
        .map(|a| a.rect)
        .filter(|r| r.is_positive())
        .unwrap_or(Rect::from_min_size(
            pos2(screen.left() + 8., status.top()),
            vec2(0., status.height()),
        ));
    let left = chip.left().clamp(
        screen.left() + 8.,
        (screen.right() - 8. - width).max(screen.left() + 8.),
    );
    let only = anchor.and_then(|a| a.plugin);
    let bounds = Rect::from_min_max(
        pos2(screen.left() + 8., status.bottom() + 4.),
        pos2(
            screen.right() - 8.,
            screen.bottom() - theme::NAVIGATION - 4.,
        ),
    );
    let area = egui::Area::new(egui::Id::unique("panels-popover"))
        .constrain_to(bounds)
        .order(egui::Order::Foreground)
        .fixed_pos(pos2(left, status.bottom() + 4.))
        .show(ctx, |ui| {
            popover_frame(p).show(ui, |ui| {
                ui.set_width(width - 26.);
                // An area starts at last frame's size: let the content grow.
                ui.set_max_height((bounds.height() - 26.).max(0.));
                egui::ScrollArea::vertical()
                    .id_salt("panels-popover-scroll")
                    .max_height((bounds.height() - 26.).max(0.))
                    .show(ui, |ui| panels(ui, cx, false, only));
            });
        });
    let outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .press_origin()
                .is_some_and(|o| !area.response.rect.contains(o) && !status.contains(o))
    });
    if outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        cx.s.panels_open = false;
    }
}

struct Tab {
    id: &'static str,
    title: &'static str,
    icon: Icon,
}

fn tabs(cx: &UiCx) -> Vec<Tab> {
    cx.k.reg
        .pages
        .iter()
        .map(|e| Tab {
            id: e.spec.id,
            title: e.spec.title,
            icon: e.spec.icon,
        })
        .chain(std::iter::once(Tab {
            id: PLUGINS_PAGE,
            title: "Plugins",
            icon: Icon::Plugin,
        }))
        .collect()
}

/// Narrow window: the bottom bar. Returns the rect of the Activity anchor (none here).
fn navigation(root: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette, rect: Rect) -> Rect {
    let mut ui = root.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(6., 4.)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let ui = &mut ui;
    let mut tabs = tabs(cx);
    let settings = Tab {
        id: SETTINGS_PAGE,
        title: "Settings",
        icon: Icon::Settings,
    };
    let gap = 2.;
    let full = ui.available_width();
    let min = 64.;
    let fits = ((full + gap) / (min + gap)).floor().max(2.) as usize;
    let mut overflow = Vec::new();
    if tabs.len() + 1 > fits {
        // Fixed slots: a page opened from More shows on the More tab instead.
        overflow = tabs.split_off(fits.saturating_sub(2));
    }
    let count = tabs.len() + 1 + usize::from(!overflow.is_empty());
    let width = ((full - gap * (count as f32 - 1.)) / count as f32).min(120.);
    let icons_only = width < 56.;
    ui.spacing_mut().item_spacing.x = gap;
    let used = width * count as f32 + gap * (count as f32 - 1.);
    ui.add_space(((full - used) / 2.).max(0.));
    for t in tabs.iter().chain(std::iter::once(&settings)) {
        if tab(ui, p, t, cx.s.page == t.id, width, icons_only).clicked() {
            cx.s.page = t.id;
            cx.s.panels_open = false;
        }
    }
    if !overflow.is_empty() {
        let more = Tab {
            id: "more",
            title: "More",
            icon: Icon::More,
        };
        let active = overflow.iter().any(|t| t.id == cx.s.page);
        let r = tab(ui, p, &more, active, width, icons_only);
        let popup = ui.make_persistent_id("more-pages");
        egui::Popup::from_toggle_button_response(&r)
            .id(popup)
            .align(egui::RectAlign::TOP_END)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_min_width(170.);
                for t in &overflow {
                    if side_tab(ui, p, t, cx.s.page == t.id, 170.).clicked() {
                        cx.s.page = t.id;
                    }
                }
            });
    }
    Rect::NOTHING
}

fn tab(
    ui: &mut egui::Ui,
    p: &kit::Palette,
    t: &Tab,
    active: bool,
    width: f32,
    icons_only: bool,
) -> egui::Response {
    let (_, rect) = ui.allocate_space(vec2(width, 40.));
    let response = ui.interact(
        rect,
        ui.scope_id().with(("navigation", t.id)),
        egui::Sense::click(),
    );
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, active, t.title));
    let hovered = response.hovered();
    if active || hovered {
        ui.painter().rect_filled(
            rect.shrink2(vec2(2., 1.)),
            theme::RADIUS,
            if active { p.selected() } else { p.hover },
        );
    }
    kit::focus_ring(ui, &response);
    let ink = if active {
        p.accent
    } else if hovered {
        p.foreground
    } else {
        p.muted
    };
    let center = rect.center();
    if icons_only {
        kit::icons::paint_sized(ui.painter(), center, t.icon, 18., ink);
    } else {
        kit::icons::paint_sized(ui.painter(), center - vec2(0., 8.), t.icon, 18., ink);
        let font = if active {
            theme::bold(theme::CAPTION)
        } else {
            egui::FontId::proportional(theme::CAPTION)
        };
        let text = ui.painter().layout_no_wrap(t.title.to_owned(), font, ink);
        ui.painter()
            .galley(center + vec2(-text.size().x / 2., 4.), text, ink);
    }
    if icons_only {
        response.on_hover_text(t.title)
    } else {
        response
    }
}

/// A page in the sidebar (wide window) or the More menu: icon and title in a row.
fn side_tab(
    ui: &mut egui::Ui,
    p: &kit::Palette,
    t: &Tab,
    active: bool,
    width: f32,
) -> egui::Response {
    let (_, rect) = ui.allocate_space(vec2(width, 32.));
    let response = ui.interact(
        rect,
        ui.scope_id().with(("navigation", t.id)),
        egui::Sense::click(),
    );
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, active, t.title));
    let hovered = response.hovered();
    if active || hovered {
        ui.painter().rect_filled(
            rect,
            theme::RADIUS_CONTROL,
            if active { p.selected() } else { p.hover },
        );
    }
    kit::focus_ring(ui, &response);
    let ink = if active {
        p.accent
    } else if hovered {
        p.foreground
    } else {
        p.muted
    };
    kit::icons::paint_sized(
        ui.painter(),
        rect.left_center() + vec2(18., 0.),
        t.icon,
        17.,
        ink,
    );
    let font = if active {
        theme::bold(theme::BODY)
    } else {
        egui::FontId::proportional(theme::BODY)
    };
    let text = ui.painter().layout_no_wrap(
        t.title.to_owned(),
        font,
        if active { p.accent } else { p.foreground },
    );
    ui.painter().galley(
        rect.left_center() + vec2(36., -text.size().y / 2.),
        text,
        if active { p.accent } else { p.foreground },
    );
    response
}

/// Wide window: pages on the left, Activity and Settings at the bottom.
/// Returns the rect of the Activity button (its popover opens beside it).
fn sidebar(root: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette, rect: Rect) -> Rect {
    let inner = rect.shrink2(vec2(10., 10.));
    let mut ui = root.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut ui;
    ui.spacing_mut().item_spacing.y = 2.;
    let width = inner.width();
    ui.horizontal(|ui| {
        ui.add_space(8.);
        ui.label(kit::bold("rproxy", theme::TITLE).color(p.foreground));
    });
    ui.add_space(theme::SPACE_L);
    for t in tabs(cx) {
        if side_tab(ui, p, &t, cx.s.page == t.id, width).clicked() {
            cx.s.page = t.id;
        }
    }
    // Bottom block: save state, Activity, Settings.
    let bottom = 32. + 2. + 26. + 2. + 24.;
    ui.add_space((ui.available_height() - bottom).max(theme::SPACE_L));
    ui.allocate_ui_with_layout(
        vec2(width, 24.),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.add_space(10.);
            save_state(ui, cx, p);
        },
    );
    let r = ui.horizontal(|ui| {
        ui.add_space(2.);
        activity_button(ui, cx, p)
    });
    let settings = Tab {
        id: SETTINGS_PAGE,
        title: "Settings",
        icon: Icon::Settings,
    };
    if side_tab(ui, p, &settings, cx.s.page == SETTINGS_PAGE, width).clicked() {
        cx.s.page = SETTINGS_PAGE;
    }
    r.inner.rect
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx) {
    if cx.s.page == PLUGINS_PAGE {
        let enabled = cx.k.reg.plugins.iter().filter(|p| !p.required).count();
        kit::page_header(ui, "Plugins", &format!("{enabled} active"), |_| {});

        // Pinned under the header: it never scrolls away or pushes the content.
        if cx.k.restart_pending()
            && kit::banner(
                ui,
                Tone::Warning,
                "Restart to apply plugin changes. Traffic pauses for a moment.",
                Some("Restart now"),
            )
        {
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
    let Some(entry) = cx.k.reg.pages.iter().find(|p| p.spec.id == cx.s.page) else {
        cx.s.page = cx.k.reg.pages.first().map_or(SETTINGS_PAGE, |p| p.spec.id);
        return;
    };
    let (id, plugin, make) = (entry.spec.id, entry.plugin, entry.make.clone());
    let mut page = cx.s.pages.remove(id).unwrap_or_else(|| make());
    ui.push_id(id, |ui| cx.scoped(Some(plugin), |cx| page.ui(ui, cx)));
    cx.s.pages.insert(id, page);
}

fn settings(ui: &mut egui::Ui, cx: &mut UiCx) {
    kit::section(ui, "General", None);
    let mut theme_choice = *cx.get(&THEME);
    let list: Vec<_> = cx.k.reg.contributions(&SETTINGS_SECTIONS).to_vec();
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
        for c in list {
            ui.push_id(&c.id, |ui| {
                cx.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, &()))
            });
        }
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
                kit::truncated(ui, RichText::new(&dir).color(p.foreground), Some(&dir));
            });
            if kit::icon_button(ui, Icon::Copy, "Copy path", true).clicked() {
                ui.ctx().copy_text(dir.clone());
                cx.notify("Path copied");
            }
        });
    });
}

/// Explicit portable configuration import and export; no hidden backup schedule.
fn configuration(ui: &mut egui::Ui, cx: &mut UiCx) {
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
fn core_row(ui: &mut egui::Ui, cx: &mut UiCx, status: &crate::core::CoreStatus) {
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
    kit::property(ui, "Core", &label);
    // Actions on their own line: they never squeeze the version.
    kit::property_with(ui, "", |ui| {
        ui.horizontal(|ui| {
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
        });
    });
    if let (Some(at), None, None) = (updates.checked, &available, &running) {
        kit::property(ui, "", &format!("Up to date · checked {}", kit::age(at)));
    }
}

/// Where a plugin's own UI lives, as a link for its row in the plugin manager.
fn plugin_link(cx: &UiCx, plugin: usize) -> Option<(&'static str, String)> {
    let reg = &cx.k.reg;
    if let Some(page) = reg.pages.iter().find(|e| e.plugin == plugin) {
        return Some((page.spec.id, format!("Open {}", page.spec.title)));
    }
    // A zone on another plugin's page ("servers.toolbar" → the Servers page).
    for (slot, _) in reg.fills.iter().filter(|(_, p)| *p == plugin) {
        let owner = slot.split('.').next().unwrap_or_default();
        if let Some(page) = reg.pages.iter().find(|e| reg.plugins[e.plugin].id == owner) {
            return Some((page.spec.id, format!("Open {}", page.spec.title)));
        }
    }
    reg.fills
        .iter()
        .any(|(slot, p)| *p == plugin && *slot == SETTINGS_SECTIONS.id)
        .then(|| (SETTINGS_PAGE, "Open Settings".to_owned()))
}

fn plugin_manager(ui: &mut egui::Ui, cx: &mut UiCx) {
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
        let exclusive = cat.policy == Policy::Exclusive;
        kit::section_note(
            ui,
            cat.name,
            if exclusive { "One at a time" } else { "" },
            None,
        );
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
                        let label = ui.label(RichText::new(pl.name).color(p.foreground));
                        if !matches!(pl.id, "subscriptions" | "connections" | "autostart") {
                            label.on_hover_text(pl.description);
                        }
                        if let Some(only) = &unsupported {
                            ui.label(
                                RichText::new(format!("Not available on this system · {only}"))
                                    .size(theme::SMALL)
                                    .color(p.muted),
                            );
                        } else if let Some(why) = &reason {
                            kit::tone_text(ui, format!("Inactive · {why}"), Tone::Warning);
                        } else if let Some((page, text)) = &link
                            && ui.link(text).clicked()
                        {
                            navigate = Some(*page);
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

fn expire_toasts(cx: &mut UiCx) {
    let now = cx.s.now;
    cx.s.toasts.retain(|t| now - t.at < 8.);
}

/// Toasts live in their own strip above the navigation (narrow) or under the
/// page (wide): its room is reserved, so a toast never covers rows. Close with
/// ×, after 8 s, or with the action (Undo); hover keeps them.
fn toasts(
    root: &mut egui::Ui,
    cx: &mut UiCx,
    p: &kit::Palette,
    reserved: Option<Rect>,
    navigation: Rect,
    wide: bool,
) {
    if cx.s.toasts.is_empty() {
        if cx.s.toast_height != 0. {
            cx.s.toast_height = 0.;
            root.ctx().request_discard("toasts closed");
        }
        return;
    }
    let screen = root.ctx().content_rect();
    let fallback = if wide {
        Rect::from_min_max(
            pos2(navigation.right(), screen.bottom() - 70.),
            pos2(screen.right() - theme::UTILITY_WIDTH, screen.bottom()),
        )
    } else {
        Rect::from_min_max(
            pos2(screen.left(), navigation.top() - 70.),
            pos2(screen.right(), navigation.top()),
        )
    };
    let rect = reserved.unwrap_or(fallback);
    let ids: Vec<u64> =
        cx.s.toasts
            .iter()
            .rev()
            .take(if screen.height() < 480. { 1 } else { 2 })
            .map(|t| t.notice.id)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
    let mut dismiss = None;
    let mut open = None;
    let mut hold = false;
    let titles: Vec<(&'static str, &'static str)> =
        cx.k.reg
            .pages
            .iter()
            .map(|e| (e.spec.id, e.spec.title))
            .collect();
    let mut child = root.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::top_down(Align::Min)),
    );
    let used = egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: theme::PADDING as i8,
            right: theme::PADDING as i8,
            top: 6,
            bottom: 2,
        })
        .show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 4.;
            for id in &ids {
                let Some(t) = cx.s.toasts.iter().find(|t| t.notice.id == *id) else {
                    continue;
                };
                let n = &t.notice;
                let (icon, tone) = if n.error {
                    (Icon::Error, p.danger)
                } else {
                    (Icon::Connected, p.success)
                };
                let r = egui::Frame::NONE
                    .fill(p.surface)
                    .stroke(Stroke::new(
                        1_f32,
                        if n.error {
                            p.tint(p.danger, 0.45)
                        } else {
                            p.border
                        },
                    ))
                    .corner_radius(theme::RADIUS)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 2],
                        blur: 8,
                        spread: 0,
                        color: p.shadow.gamma_multiply(0.6),
                    })
                    .inner_margin(egui::Margin {
                        left: 10,
                        right: 4,
                        top: 6,
                        bottom: 6,
                    })
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        // No "Open X" on page X itself.
                        let link = n
                            .page
                            .filter(|id| *id != cx.s.page)
                            .and_then(|id| titles.iter().find(|(pid, _)| *pid == id).copied());
                        kit::row_line(ui, theme::CONTROL, |ui| {
                            kit::icon(ui, icon, 16., tone);
                            let width =
                                (ui.available_width() - theme::CONTROL - theme::HORIZONTAL_GAP)
                                    .max(0.);
                            kit::cell(ui, width, |ui| {
                                kit::truncated(
                                    ui,
                                    kit::bold(&n.title, theme::BODY).color(p.foreground),
                                    None,
                                );
                            });
                            if kit::icon_button(ui, Icon::Close, "Dismiss", true).clicked() {
                                dismiss = Some(n.id);
                            }
                        });
                        if !n.text.is_empty() {
                            let mut text = (*egui::WidgetText::from(
                                RichText::new(&n.text).size(theme::SMALL).color(p.muted),
                            )
                            .into_layout_job(
                                ui.style(),
                                egui::FontSelection::Default,
                                Align::Min,
                            ))
                            .clone();
                            text.wrap.max_rows = 3;
                            let (response, elided) =
                                kit::emoji::text_label_with_elision(ui, text, true, false);
                            if elided {
                                response.on_hover_ui(|ui| {
                                    kit::emoji::text_label(ui, &n.text, true, false);
                                });
                            }
                        }
                        if let Some((id, title)) = link
                            && kit::Button::ghost(&format!("Open {title}"))
                                .tone(Tone::Accent)
                                .show(ui)
                                .clicked()
                        {
                            open = Some(id);
                            dismiss = Some(n.id);
                        }
                    });
                hold |= r.response.contains_pointer();
            }
        });
    let height = used.response.rect.height().ceil();
    if (height - cx.s.toast_height).abs() > 0.5 {
        cx.s.toast_height = height;
        root.ctx().request_discard("toasts resized");
    }
    let now = cx.s.now;
    if hold {
        for t in &mut cx.s.toasts {
            t.at = now;
        }
    }
    if let Some(id) = dismiss {
        cx.s.toasts.retain(|t| t.notice.id != id);
    }
    if let Some(page) = open {
        cx.s.page = page;
    }
}

/// One line of Activity: equal consecutive runs collapse into "× n".
struct Entry<'a> {
    run: &'a crate::Run,
    count: usize,
}

fn activity(ctx: &egui::Context, cx: &mut UiCx, p: &kit::Palette, anchor: Rect, wide: bool) {
    let board = cx.k.board();
    let screen = ctx.content_rect();
    let width = (screen.width() - 16.).min(340.);
    let area = egui::Area::new(egui::Id::unique("activity")).order(egui::Order::Foreground);
    let area = if wide {
        // Beside the sidebar button, growing upwards.
        area.pivot(Align2::LEFT_BOTTOM)
            .fixed_pos(pos2(anchor.right() + 8., anchor.bottom()))
    } else {
        let left = (anchor.right() - width).clamp(
            screen.left() + 8.,
            (screen.right() - width - 8.).max(screen.left() + 8.),
        );
        area.fixed_pos(pos2(left, anchor.bottom() + 8.))
    };
    let bounds = if wide {
        Rect::from_min_max(
            pos2(anchor.right() + 8., screen.top() + 8.),
            pos2(
                screen.right() - 8.,
                anchor.bottom().min(screen.bottom() - 8.),
            ),
        )
    } else {
        Rect::from_min_max(
            pos2(screen.left() + 8., anchor.bottom() + 8.),
            pos2(
                screen.right() - 8.,
                screen.bottom() - theme::NAVIGATION - 4.,
            ),
        )
    };
    let area = area.constrain_to(bounds).show(ctx, |ui| {
        popover_frame(p).show(ui, |ui| {
            ui.set_width(width - 26.);
            // An area starts at last frame's size: let the content grow.
            ui.set_max_height((bounds.height() - 26.).max(0.));
            panel_title(ui, "Activity", |_| {});
            egui::ScrollArea::vertical()
                .id_salt("activity-scroll")
                .max_height((bounds.height() - 54.).max(0.))
                .show(ui, |ui| {
                    let runs: Vec<_> = board
                        .runs
                        .iter()
                        .filter(|r| !r.quiet || r.phase == Phase::Failed)
                        .collect();
                    let mut entries: Vec<Entry> = Vec::new();
                    for r in runs {
                        let same = |e: &Entry| {
                            let a = e.run;
                            !a.phase.active()
                                && !r.phase.active()
                                && a.label == r.label
                                && a.phase == r.phase
                                && a.summary == r.summary
                                && a.error.as_ref().map(|e| &e.message)
                                    == r.error.as_ref().map(|e| &e.message)
                        };
                        match entries.last_mut() {
                            Some(e) if same(e) => e.count += 1,
                            _ => entries.push(Entry { run: r, count: 1 }),
                        }
                    }
                    if entries.is_empty() {
                        kit::hint(ui, "No background tasks yet.");
                    }
                    for e in entries.iter().take(8) {
                        activity_entry(ui, cx, p, e);
                    }
                    let schedules: Vec<_> = board
                        .schedules
                        .iter()
                        .filter(|s| !s.name.starts_with('@') && !s.quiet)
                        .collect();
                    if !schedules.is_empty() {
                        ui.add_space(6.);
                        kit::separator(ui);
                        ui.add_space(4.);
                        kit::caption(ui, "Scheduled");
                        for s in schedules {
                            let secs = s.next_unix_ms / 1000;
                            let left = secs.saturating_sub(crate::now());
                            kit::row_line(ui, 22., |ui| {
                                ui.label(
                                    RichText::new(schedule_name(&s.label, &s.name))
                                        .size(theme::SMALL),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format!("in {}", human(left)))
                                            .size(theme::SMALL)
                                            .color(p.muted),
                                    );
                                });
                            });
                        }
                    }
                });
        });
    });
    let chip = if wide {
        anchor
    } else {
        cx.s.activity_chip.unwrap_or(anchor)
    };
    let outside = ctx.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .press_origin()
                .is_some_and(|o| !area.response.rect.contains(o) && !chip.contains(o))
    });
    if outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        cx.s.activity_open = false;
    }
}

fn activity_entry(ui: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette, e: &Entry) {
    let r = e.run;
    let (color, state) = match r.phase {
        Phase::Running => (p.accent, "Running"),
        Phase::Cancelling => (p.muted, "Stopping…"),
        Phase::Succeeded => (p.success, "Done"),
        Phase::Cancelled => (p.muted, "Cancelled"),
        Phase::Failed => (p.danger, "Failed"),
    };
    ui.spacing_mut().item_spacing.y = 0.;
    kit::row_line(ui, 22., |ui| {
        kit::dot(ui, color);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if r.phase == Phase::Running && kit::icon_button(ui, Icon::Stop, "Stop", true).clicked()
            {
                let _ = cx.k.jobs.cancel(r.id);
            }
            let when = match r.ended {
                _ if r.phase.active() => String::new(),
                Some(t) => kit::age(t),
                None => String::new(),
            };
            if !when.is_empty() {
                ui.label(RichText::new(when).size(theme::SMALL).color(p.muted));
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.;
                kit::truncated(ui, RichText::new(&r.label).color(p.foreground), None);
                if e.count > 1 {
                    ui.label(
                        RichText::new(format!("× {}", e.count))
                            .size(theme::SMALL)
                            .color(p.muted),
                    )
                    .on_hover_text(format!("{} runs in a row", e.count));
                }
            });
        });
    });
    // Second line: progress, the result, or the error.
    let (text, tone) = match (&r.error, &r.summary) {
        _ if r.phase.active() => (
            match r.total {
                Some(t) => format!("{state} · {} of {t}", r.done),
                None => state.to_owned(),
            },
            Tone::Neutral,
        ),
        (Some(err), _) if r.phase == Phase::Failed => (err.message.clone(), Tone::Danger),
        (_, Some(summary)) => (
            summary.clone(),
            if r.phase == Phase::Succeeded {
                Tone::Neutral
            } else {
                Tone::Warning
            },
        ),
        _ => (state.to_owned(), Tone::Neutral),
    };
    ui.horizontal(|ui| {
        ui.add_space(14.);
        ui.add(egui::Label::new(RichText::new(text).size(theme::SMALL).color(p.tone(tone))).wrap());
    });
    ui.add_space(6.);
}

/// The job's label ("Check servers"); a readable name otherwise
/// ("subscriptions/refresh" → "Subscriptions · refresh").
fn schedule_name(label: &str, name: &str) -> String {
    if !label.is_empty() && !label.contains('/') {
        return label.to_owned();
    }
    let mut parts = name.split('/');
    let plugin = parts.next().unwrap_or(name).replace('-', " ");
    let what = parts.next_back().unwrap_or(name);
    let mut s = format!("{plugin} · {what}");
    if let Some(c) = s.get(0..1) {
        s.replace_range(0..1, &c.to_uppercase());
    }
    s
}

fn human(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs} s"),
        60..=3599 => format!("{} min", secs / 60),
        3600..=86399 => format!("{} h", secs / 3600),
        _ => format!("{} d", secs / 86400),
    }
}

/// Tray icon state (desktop). The worst answer wins: Failed > Connected > Idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayLink {
    Idle,
    Connected,
    Failed,
}
pub static TRAY: crate::Hook<(), Option<TrayLink>> = crate::Hook::new("shell.tray");
