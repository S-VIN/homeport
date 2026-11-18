//! Notifications floating over the page.
use super::*;

/// A confirmation leaves after 4 s; an error or an offer to undo after 8 s.
/// An error about the page on screen leaves at once: the page shows it in place.
pub(super) fn expire_toasts(cx: &mut UiCx) {
    let (now, page) = (cx.s.now, cx.s.page);
    cx.s.toasts.retain(|t| {
        let n = &t.notice;
        if n.error && n.page == Some(page) {
            return false;
        }
        let life = if n.error || n.action.is_some() || n.page.is_some() {
            8.
        } else {
            4.
        };
        now - t.at < life
    });
}

/// Toasts float over the content at the top, newest first: under the status
/// bar in a narrow window, over the panels column otherwise. Nothing moves
/// when they come and go. Hover holds them; the cross closes one.
pub(super) fn toasts(
    ctx: &egui::Context,
    cx: &mut UiCx,
    p: &kit::Palette,
    status: Rect,
    panels: Option<Rect>,
) {
    if cx.s.toasts.is_empty() {
        return;
    }
    let screen = ctx.content_rect();
    // Over the top of the panels column, never over the page's own controls.
    let (left, top, width) = match panels {
        Some(r) => (
            r.left(),
            screen.top() + 10.,
            theme::UTILITY_WIDTH - theme::PADDING,
        ),
        None => {
            let width = (screen.width() - 16.).min(360.);
            (screen.center().x - width / 2., status.bottom() + 6., width)
        }
    };
    let ids: Vec<u64> =
        cx.s.toasts
            .iter()
            .rev()
            .take(if screen.height() < 480. { 2 } else { 3 })
            .map(|t| t.notice.id)
            .collect();
    let mut dismiss = None;
    let mut open = None;
    let mut act = None;
    let live_actions = cx.k.notice_actions();
    let mut hold = false;
    let titles: Vec<(&'static str, &'static str)> =
        cx.k.reg
            .pages
            .iter()
            .map(|e| (e.id, e.spec.title))
            .collect();
    egui::Area::new(egui::Id::unique("toasts"))
        .order(egui::Order::Foreground)
        .fixed_pos(pos2(left, top))
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 6.;
            for id in &ids {
                let Some(t) = cx.s.toasts.iter().find(|t| t.notice.id == *id) else {
                    continue;
                };
                let n = &t.notice;
                // No "Open X" on page X itself.
                let link = n
                    .page
                    .filter(|id| *id != cx.s.page)
                    .and_then(|id| titles.iter().find(|(pid, _)| *pid == id).copied());
                // Pressed in another window: the button has nothing left to do.
                let action = n.action.as_ref().filter(|_| live_actions.contains(&n.id));
                let (clicked, hovered) = toast(ui, p, n, link, action.map(|a| a.label));
                hold |= hovered;
                match clicked {
                    Some(ToastClick::Dismiss) => dismiss = Some(n.id),
                    Some(ToastClick::Open) => {
                        open = link.map(|(id, _)| id);
                        dismiss = Some(n.id);
                    }
                    Some(ToastClick::Act) => {
                        act = Some(n.id);
                        dismiss = Some(n.id);
                    }
                    None => {}
                }
            }
        });
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
    // Every window shows the toast; its action runs once.
    if let Some(action) = act.and_then(|id| cx.k.take_notice_action(id)) {
        cx.scoped(action.plugin, |cx| (action.run)(cx));
    }
}

pub(super) enum ToastClick {
    Dismiss,
    Open,
    Act,
}

/// One toast card: what was pressed, and whether the pointer rests on it.
pub(super) fn toast(
    ui: &mut egui::Ui,
    p: &kit::Palette,
    n: &crate::kernel::Notice,
    link: Option<(&'static str, &'static str)>,
    action: Option<&str>,
) -> (Option<ToastClick>, bool) {
    let mut clicked = None;
    // A toast that offers an action reports a change to undo, not a success.
    let (icon, tone) = match (n.error, &n.action) {
        (true, _) => (Icon::Error, p.danger),
        (false, Some(_)) => (Icon::Info, p.accent),
        (false, None) => (Icon::Connected, p.success),
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
            offset: [0, 6],
            blur: 20,
            spread: 0,
            color: p.shadow,
        })
        .inner_margin(egui::Margin {
            left: 10,
            right: 4,
            top: 6,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            kit::row_line(ui, theme::CONTROL, |ui| {
                kit::icon(ui, icon, 16., tone);
                let width = (ui.available_width() - theme::CONTROL - theme::HORIZONTAL_GAP).max(0.);
                kit::cell(ui, width, |ui| {
                    kit::truncated(
                        ui,
                        kit::bold(&n.title, theme::BODY).color(p.foreground),
                        None,
                    );
                });
                if kit::icon_button(ui, Icon::Close, "Dismiss", true).clicked() {
                    clicked = Some(ToastClick::Dismiss);
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
                let (response, elided) = kit::text_label_with_elision(ui, text, true, false);
                if elided {
                    response.on_hover_ui(|ui| {
                        kit::text_label(ui, &n.text, true, false);
                    });
                }
            }
            if let Some((_, title)) = link
                && kit::Button::ghost(&format!("Open {title}"))
                    .tone(Tone::Accent)
                    .show(ui)
                    .clicked()
            {
                clicked = Some(ToastClick::Open);
            }
            if let Some(label) = action
                && kit::Button::ghost(label)
                    .tone(Tone::Accent)
                    .show(ui)
                    .clicked()
            {
                clicked = Some(ToastClick::Act);
            }
        });
    (clicked, r.response.contains_pointer())
}
