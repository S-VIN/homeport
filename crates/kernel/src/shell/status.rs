//! The status bar (chips, Saved, Activity) and the panels: docked or in a popover.
use super::*;

/// "Saving…" / "Saved ✓" after a change.
pub(super) fn save_state(ui: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette) {
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
    let saved = !saving && cx.s.saved_at().is_some_and(|t| cx.s.now - t < 2.);
    if kit::phone(ui) {
        // Just the mark, in room kept for it: status chips don't jump or fold.
        let (rect, response) = ui.allocate_exact_size(vec2(16., 16.), egui::Sense::hover());
        if saving || saved {
            let ink = if saving { p.faint } else { p.success };
            kit::icons::paint_sized(ui.painter(), rect.center(), Icon::Check, 13., ink);
            response.on_hover_text(if saving { "Saving…" } else { "Saved" });
        }
        return;
    }
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

/// The Tasks button: icon and "Tasks" or "2 running".
pub(super) fn activity_button(
    ui: &mut egui::Ui,
    cx: &mut UiCx,
    p: &kit::Palette,
) -> egui::Response {
    let a = Activity::of(cx);
    // On a phone an idle Activity is just its icon: the room goes to status chips.
    let text = if a.running == 0 && kit::phone(ui) {
        ""
    } else {
        &a.text
    };
    let ink = if a.running > 0 { p.accent } else { p.faint };
    let r = kit::chip(ui, Some(Lead::Icon(Icon::Tasks, ink)), text, &a.tip);
    a.mark(ui, p, r.rect.left_center() + vec2(15., -5.));
    if r.clicked() {
        a.toggle(cx);
    }
    r
}

/// What the Activity button shows: running tasks, and failures not seen yet.
pub(super) struct Activity {
    pub text: String,
    pub tip: String,
    pub running: usize,
    unseen: bool,
}
impl Activity {
    pub fn of(cx: &mut UiCx) -> Self {
        let board = cx.k.board();
        let active: Vec<_> = board
            .runs
            .iter()
            .filter(|r| r.phase.active() && !r.quiet)
            .collect();
        let failed = board
            .runs
            .iter()
            .filter(|r| r.phase == Phase::Failed)
            .count();
        if cx.s.activity_open || failed < cx.s.seen_failures {
            cx.s.seen_failures = failed;
        }
        Self {
            text: match active.len() {
                0 => "Tasks".to_owned(),
                n => format!("{n} running"),
            },
            tip: if active.is_empty() {
                "Background tasks".to_owned()
            } else {
                active
                    .iter()
                    .map(|r| r.label.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            },
            running: active.len(),
            unseen: failed > cx.s.seen_failures,
        }
    }
    /// A dot on the icon while a failure waits to be seen.
    pub fn mark(&self, ui: &egui::Ui, p: &kit::Palette, at: egui::Pos2) {
        if self.unseen {
            ui.painter().circle_filled(at, 3.5, p.danger);
        }
    }
    pub fn toggle(&self, cx: &mut UiCx) {
        cx.s.activity_open = !cx.s.activity_open;
        cx.s.panels_open = false;
    }
}

pub(super) fn status_bar(ui: &mut egui::Ui, cx: &mut UiCx) {
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
        // Chips get all but the right block, as wide as it was last frame.
        let right_key = egui::Id::unique("status-right-width");
        let right: f32 = ui.data(|d| d.get_temp(right_key)).unwrap_or(150.);
        let width = ui.available_width() - right - 8.;
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
            let used = ui.min_rect().width();
            if (used - right).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(right_key, used));
                ui.ctx().request_repaint();
            }
        });
    });
}

/// The status chips that fit whole, in order; the rest fold into a "+N" chip
/// that opens all panels. The first chip (the connection) gives up to 45% of
/// the bar to the others and shortens its text instead.
pub(super) fn status_chips(ui: &mut egui::Ui, cx: &mut UiCx) {
    const MORE: f32 = 44.;
    let list = cx.k.reg.contributions(&STATUS).to_vec();
    let gap = ui.spacing().item_spacing.x;
    let bar = ui.available_width();
    // Natural widths, measured off-screen this frame; 0 for a chip that shows nothing.
    let natural: Vec<f32> = list
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let cap = if i == 0 { bar } else { 240. };
            let area = Rect::from_min_size(ui.cursor().min, vec2(cap, 24.));
            let mut probe = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("status-probe", c.id.as_str()))
                    .max_rect(area)
                    .layout(Layout::left_to_right(Align::Center))
                    .sizing_pass()
                    .invisible(),
            );
            cx.scoped(Some(c.plugin), |cx| (c.view)(&mut probe, cx, &()));
            probe.min_rect().width()
        })
        .collect();
    let Some((&first, rest)) = natural.split_first() else {
        return;
    };
    let total: f32 = natural.iter().filter(|w| **w > 0.).map(|w| w + gap).sum();
    let mut shown = vec![true; natural.len()];
    let mut first_width = first;
    if total > bar {
        // Others fill what the first chip leaves, in order, whole or not at all.
        let mut used = first.min(bar * 0.55) + gap;
        for (i, &w) in rest.iter().enumerate() {
            let left = rest[i + 1..].iter().any(|w| *w > 0.);
            if w > 0. && used + w + gap > bar - if left { MORE } else { 0. } {
                shown[i + 1..].fill(false);
                break;
            }
            used += w + gap;
        }
        let others: f32 = rest
            .iter()
            .zip(&shown[1..])
            .filter(|(w, on)| **on && **w > 0.)
            .map(|(w, _)| w + gap)
            .sum();
        let hidden = rest.iter().zip(&shown[1..]).any(|(w, on)| !on && *w > 0.);
        let more = if hidden { MORE } else { 0. };
        first_width = first.min(bar - others - more - gap).max(0.);
    }
    for (i, c) in list.iter().enumerate() {
        if !shown[i] {
            continue;
        }
        let width = if i == 0 { first_width } else { natural[i] };
        let area = Rect::from_min_size(ui.cursor().min, vec2(width, 24.));
        let builder = egui::UiBuilder::new()
            .max_rect(area)
            .layout(Layout::left_to_right(Align::Center));
        ui.scope_builder(builder, |ui| {
            ui.set_clip_rect(area.intersect(ui.clip_rect()));
            cx.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, &()))
        });
    }
    cx.s.folded = (0..list.len())
        .filter(|&i| !shown[i] && natural[i] > 0.)
        .collect();
    if !cx.s.folded.is_empty() {
        let hidden = cx.s.folded.len();
        let r = kit::chip(ui, None, &format!("+{hidden}"), "More status");
        if r.clicked() {
            cx.scoped(None, |cx| cx.toggle_panels_from(&r));
        }
    }
}

/// The status chips the bar had no room for, at the top of the "+N" popover
/// (those with a panel there show it instead).
pub(super) fn folded_chips(ui: &mut egui::Ui, cx: &mut UiCx) {
    let list = cx.k.reg.contributions(&STATUS).to_vec();
    // A chip whose plugin has a panel here would say the same thing twice.
    let panels: Vec<usize> =
        cx.k.reg
            .contributions(&PANELS)
            .iter()
            .map(|c| c.plugin)
            .collect();
    let folded: Vec<_> =
        cx.s.folded
            .iter()
            .filter_map(|&i| list.get(i))
            .filter(|c| !panels.contains(&c.plugin))
            .collect();
    if folded.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for c in folded {
            ui.push_id(c.id.as_str(), |ui| {
                cx.scoped(Some(c.plugin), |cx| (c.view)(ui, cx, &()))
            });
        }
    });
    ui.add_space(theme::SPACE_M);
    let line = theme::palette(ui).border;
    let y = ui.cursor().top() + 0.5;
    ui.painter().hline(ui.max_rect().x_range(), y, (1., line));
    ui.add_space(theme::SPACE_M);
}

/// Title row of a panel card: caption on the left, optional content on the right.
pub fn panel_title(ui: &mut egui::Ui, title: &str, right: impl FnOnce(&mut egui::Ui)) {
    // As tall as a button, so a card keeps its height with or without one.
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), theme::CONTROL),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_min_size(vec2(ui.available_width(), theme::CONTROL));
            kit::caption(ui, title);
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        },
    );
    ui.add_space(2.);
}

/// Panel title with an "Open" button that opens `plugin`'s page (hidden on that page).
pub fn panel_title_page(
    ui: &mut egui::Ui,
    cx: &mut UiCx,
    title: &str,
    plugin: &'static crate::Plugin,
) {
    let mut open = false;
    panel_title(ui, title, |ui| {
        if !cx.on_page(plugin) {
            open = kit::Button::ghost("Open")
                .tone(Tone::Accent)
                .show(ui)
                .clicked();
        }
    });
    if open {
        cx.navigate(plugin);
    }
}

/// Panel cards. `only`: the panels of one plugin (a chip's popover); all when it has none.
pub(super) fn panels(ui: &mut egui::Ui, cx: &mut UiCx, cards: bool, only: Option<usize>) {
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
            // A divider above every panel but the first; a panel that draws
            // nothing leaves no trace.
            let gap = if first { 0. } else { theme::SPACE_M };
            let mut frame = egui::Frame::NONE
                .inner_margin(egui::Margin {
                    top: (gap * 2.) as i8,
                    ..Default::default()
                })
                .begin(ui);
            frame
                .content_ui
                .set_min_width(frame.content_ui.available_width());
            add(&mut frame.content_ui);
            if frame.content_ui.min_rect().height() <= 0. {
                return;
            }
            let rect = frame.end(ui).rect;
            if !first {
                let line = theme::palette(ui).border;
                ui.painter()
                    .hline(rect.x_range(), rect.top() + gap + 0.5, (1., line));
            }
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

pub(super) fn popover_frame(p: &kit::Palette) -> egui::Frame {
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
pub(super) fn panels_popover(ctx: &egui::Context, cx: &mut UiCx, p: &kit::Palette, status: Rect) {
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
                    .show(ui, |ui| {
                        if only.is_none() {
                            folded_chips(ui, cx);
                        }
                        panels(ui, cx, false, only)
                    });
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
