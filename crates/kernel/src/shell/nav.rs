//! Page navigation: the bottom bar (narrow) and the sidebar (wide).
use super::*;

pub(super) struct Tab {
    id: &'static str,
    title: &'static str,
    icon: Icon,
}

pub(super) fn tabs(cx: &UiCx) -> Vec<Tab> {
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
pub(super) fn navigation(root: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette, rect: Rect) -> Rect {
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
    let more = Tab {
        id: "more",
        title: "More",
        icon: Icon::More,
    };
    let gap = 2.;
    let full = ui.available_width();
    // Every label shows whole: a tab is as wide as its title needs, and the
    // pages that do not fit go to More.
    let need = |t: &Tab| {
        let font = theme::bold(theme::CAPTION);
        let text = ui
            .painter()
            .layout_no_wrap(t.title.to_owned(), font, p.muted);
        (text.size().x + 16.).max(52.)
    };
    let mut used = need(&settings);
    let mut shown = 0;
    for (i, t) in tabs.iter().enumerate() {
        let reserve = if i + 1 < tabs.len() {
            gap + need(&more)
        } else {
            0.
        };
        if used + gap + need(t) + reserve > full {
            break;
        }
        used += gap + need(t);
        shown += 1;
    }
    let overflow = tabs.split_off(shown);
    let mut row: Vec<&Tab> = tabs.iter().chain([&settings]).collect();
    if !overflow.is_empty() {
        row.push(&more);
    }
    let needs: Vec<f32> = row.iter().map(|t| need(t)).collect();
    let count = row.len() as f32;
    let room = full - gap * (count - 1.);
    let even = (room / count).min(120.);
    // Equal tabs when they all fit, otherwise each gets its need plus a share.
    let widths: Vec<f32> = if needs.iter().all(|n| *n <= even) {
        vec![even; row.len()]
    } else {
        let extra = ((room - needs.iter().sum::<f32>()) / count).max(0.);
        needs.iter().map(|n| n + extra).collect()
    };
    let icons_only = needs.iter().sum::<f32>() > room;
    ui.spacing_mut().item_spacing.x = gap;
    let total = widths.iter().sum::<f32>() + gap * (count - 1.);
    ui.add_space(((full - total) / 2.).max(0.));
    for (t, width) in row.iter().zip(widths) {
        let is_more = t.id == more.id;
        // A page opened from More shows on the More tab.
        let active = if is_more {
            overflow.iter().any(|t| t.id == cx.s.page)
        } else {
            cx.s.page == t.id
        };
        let r = tab(ui, p, t, active, width, icons_only);
        if is_more {
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
        } else if r.clicked() {
            cx.s.page = t.id;
            cx.s.panels_open = false;
        }
    }
    Rect::NOTHING
}

pub(super) fn tab(
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
        // A title wider than the tab ends with "…"; the tooltip has it whole.
        let mut job = egui::text::LayoutJob::simple_singleline(t.title.to_owned(), font, ink);
        job.wrap = egui::text::TextWrapping::truncate_at_width(width - 6.);
        let text = ui.painter().layout_job(job);
        let cut = text.elided;
        ui.painter()
            .galley(center + vec2(-text.size().x / 2., 4.), text, ink);
        if cut {
            return response.on_hover_text(t.title);
        }
    }
    if icons_only {
        response.on_hover_text(t.title)
    } else {
        response
    }
}

/// A page in the sidebar (wide window) or the More menu: icon and title in a row.
pub(super) fn side_tab(
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
pub(super) fn sidebar(root: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette, rect: Rect) -> Rect {
    let inner = rect.shrink2(vec2(10., 10.));
    let mut ui = root.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::top_down(Align::Min)),
    );
    let ui = &mut ui;
    ui.spacing_mut().item_spacing.y = 2.;
    let width = inner.width();
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
