//! The Activity popover: running, recent and scheduled jobs.
use super::*;

/// One line of Activity: equal consecutive runs collapse into "× n".
pub(super) struct Entry<'a> {
    run: &'a crate::Run,
    count: usize,
}

pub(super) fn activity(
    ctx: &egui::Context,
    cx: &mut UiCx,
    p: &kit::Palette,
    anchor: Rect,
    wide: bool,
) {
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
            panel_title(ui, "Tasks", |_| {});
            egui::ScrollArea::vertical()
                .id_salt("activity-scroll")
                .max_height((bounds.height() - 54.).max(0.))
                .show(ui, |ui| {
                    let mut runs: Vec<_> = board
                        .runs
                        .iter()
                        .filter(|r| !r.quiet || r.phase == Phase::Failed)
                        .collect();
                    // Running ones first, then the latest to end: the times read in order.
                    runs.sort_by_key(|r| std::cmp::Reverse(r.ended.unwrap_or(u64::MAX)));
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
                            // The time first, so a long name is the one cut.
                            kit::row_line(ui, 22., |ui| {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new(format!("in {}", kit::duration(left)))
                                            .size(theme::SMALL)
                                            .color(p.muted),
                                    );
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        kit::truncated(
                                            ui,
                                            RichText::new(schedule_name(&s.label, &s.name))
                                                .size(theme::SMALL),
                                            None,
                                        );
                                    });
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

pub(super) fn activity_entry(ui: &mut egui::Ui, cx: &mut UiCx, p: &kit::Palette, e: &Entry) {
    let r = e.run;
    let (color, state) = match r.phase {
        Phase::Running => (p.accent, "Running"),
        Phase::Cancelling => (p.muted, "Stopping…"),
        Phase::Succeeded => (p.success, "Done"),
        Phase::Cancelled => (p.muted, "Cancelled"),
        Phase::Failed => (p.warning, "Failed"),
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
    // Second line: progress, the result, or the error. A plain "Done" or
    // "Cancelled" says no more than the dot.
    let (text, tone) = match (&r.error, &r.summary) {
        _ if r.phase.active() => (
            match r.total {
                Some(t) => format!("{state} · {} of {t}", r.done),
                None => state.to_owned(),
            },
            Tone::Neutral,
        ),
        (Some(err), _) if r.phase == Phase::Failed => (err.message.clone(), Tone::Warning),
        (_, Some(summary)) => (
            summary.clone(),
            if r.phase == Phase::Succeeded {
                Tone::Neutral
            } else {
                Tone::Warning
            },
        ),
        _ => {
            ui.add_space(6.);
            return;
        }
    };
    ui.horizontal(|ui| {
        ui.add_space(14.);
        ui.add(egui::Label::new(RichText::new(text).size(theme::SMALL).color(p.tone(tone))).wrap());
    });
    ui.add_space(6.);
}

/// The job's label ("Check servers"); a readable name otherwise
/// ("subscriptions/refresh" → "Subscriptions · refresh").
pub(super) fn schedule_name(label: &str, name: &str) -> String {
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
