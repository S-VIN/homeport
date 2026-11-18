use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Filled accent: the one main action of a view.
    Primary,
    /// Bordered neutral: every other action.
    Secondary,
    /// Text only until hovered: inline and low-priority actions.
    Ghost,
    /// Filled red: confirming a destructive action.
    Danger,
}

/// Text button with an optional leading icon.
#[must_use]
pub struct Button<'a> {
    kind: Kind,
    icon: Option<Icon>,
    text: &'a str,
    width: f32,
    enabled: bool,
    selected: bool,
    count: Option<String>,
    tone: Option<Tone>,
}
impl<'a> Button<'a> {
    pub fn new(kind: Kind, text: &'a str) -> Self {
        Self {
            kind,
            icon: None,
            text,
            width: 0.,
            enabled: true,
            selected: false,
            count: None,
            tone: None,
        }
    }
    pub fn primary(text: &'a str) -> Self {
        Self::new(Kind::Primary, text)
    }
    pub fn secondary(text: &'a str) -> Self {
        Self::new(Kind::Secondary, text)
    }
    pub fn ghost(text: &'a str) -> Self {
        Self::new(Kind::Ghost, text)
    }
    pub fn danger(text: &'a str) -> Self {
        Self::new(Kind::Danger, text)
    }
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }
    /// Minimum width, so a label change (e.g. "Stop") does not move neighbors.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    /// Pressed state of a toggle button (e.g. an open editor).
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    /// Small counter after the label.
    pub fn count(mut self, count: Option<usize>) -> Self {
        self.count = count.map(|n| n.to_string());
        self
    }
    /// Ink of a ghost button; ink and border of a secondary one.
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = Some(tone);
        self
    }
    pub fn show(mut self, ui: &mut Ui) -> Response {
        // In a list row on a phone, a button with an icon shows only the icon.
        let full_label = self.text;
        if self.icon.is_some() && compact_row(ui) {
            self.text = "";
            self.width = 0.;
            self.count = None;
        }
        let p = theme::palette(ui);
        let font = FontId::proportional(theme::BODY);
        let galley =
            ui.painter()
                .layout_no_wrap(self.text.to_owned(), font.clone(), Color32::PLACEHOLDER);
        let count = self.count.as_ref().map(|c| {
            ui.painter().layout_no_wrap(
                c.clone(),
                FontId::proportional(theme::CAPTION),
                Color32::PLACEHOLDER,
            )
        });
        let pad = if self.text.is_empty() { 5. } else { 10. };
        let icon_w = if self.icon.is_some() {
            16. + if self.text.is_empty() { 0. } else { 6. }
        } else {
            0.
        };
        let count_w = count.as_ref().map_or(0., |c| c.size().x + 12.);
        let content = icon_w + galley.size().x + count_w;
        let width = (content + pad * 2.).max(self.width);
        let (rect, response) = ui.allocate_exact_size(
            vec2(width, CONTROL),
            if self.enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let hovered = self.enabled && (response.hovered() || response.has_focus());
        let pressed = self.enabled && response.is_pointer_button_down_on();
        let (fill, stroke, ink) = match self.kind {
            Kind::Primary => (
                if hovered { p.action_hover } else { p.action_bg },
                Stroke::NONE,
                p.action_fg,
            ),
            Kind::Danger => (
                p.danger,
                Stroke::NONE,
                if p.dark { p.background } else { Color32::WHITE },
            ),
            Kind::Secondary if self.selected => (
                p.selected(),
                Stroke::new(1_f32, p.tint(p.accent, 0.45)),
                p.accent,
            ),
            Kind::Secondary => {
                let fill = if pressed {
                    p.subtle
                } else if hovered {
                    p.hover
                } else {
                    p.surface
                };
                match self.tone.filter(|t| *t != Tone::Neutral) {
                    // A toned secondary (e.g. Disconnect): colored ink, tinted border.
                    Some(t) => (
                        if hovered { p.soft(p.tone(t)) } else { fill },
                        Stroke::new(1_f32, p.tint(p.tone(t), if hovered { 0.7 } else { 0.45 })),
                        p.tone(t),
                    ),
                    None => (
                        fill,
                        Stroke::new(1_f32, if hovered { p.faint } else { p.control_border }),
                        p.foreground,
                    ),
                }
            }
            Kind::Ghost => (
                if hovered || self.selected {
                    p.subtle
                } else {
                    Color32::TRANSPARENT
                },
                Stroke::NONE,
                self.tone.map_or(p.foreground, |t| p.tone(t)),
            ),
        };
        // Disabled keeps the style's shape, dimmed: a primary stays the main action.
        let (fill, stroke, ink) = match self.kind {
            _ if self.enabled => (fill, stroke, ink),
            Kind::Ghost => (fill, Stroke::NONE, p.faint),
            Kind::Secondary => (p.surface, Stroke::new(1_f32, p.control_border), p.faint),
            Kind::Primary | Kind::Danger => (fill.gamma_multiply(theme::DIMMED), Stroke::NONE, ink),
        };
        let painter = ui.painter();
        painter.rect(rect, RADIUS_CONTROL, fill, stroke, egui::StrokeKind::Inside);
        let mut x = rect.center().x - content / 2.;
        if let Some(icon) = self.icon {
            crate::icons::paint_sized(painter, pos2(x + 8., rect.center().y), icon, 16., ink);
            x += icon_w;
        }
        painter.galley(pos2(x, rect.center().y - galley.size().y / 2.), galley, ink);
        if let Some(c) = count {
            let x = rect.center().x + content / 2. - count_w + 6.;
            let pill = Rect::from_min_size(
                pos2(x - 4., rect.center().y - 8.),
                vec2(c.size().x + 8., 16.),
            );
            let pill_fill = match self.kind {
                Kind::Primary | Kind::Danger => Color32::from_white_alpha(50),
                _ if self.selected => p.tint(p.accent, 0.25),
                _ => p.subtle,
            };
            painter.rect_filled(pill, 8, pill_fill);
            painter.galley(pos2(x, rect.center().y - c.size().y / 2.), c, ink);
        }
        focus_ring(ui, &response);
        let label = full_label.to_owned();
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, self.enabled, self.selected, &label)
        });
        if self.text.is_empty() && !full_label.is_empty() {
            return response.on_hover_text(full_label);
        }
        response
    }
}

/// Primary call to action.
pub fn action(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    Button::primary(text).enabled(enabled).show(ui).clicked()
}
/// Secondary button with a minimum width.
pub fn button(ui: &mut Ui, text: &str, width: f32, enabled: bool) -> Response {
    Button::secondary(text)
        .width(width)
        .enabled(enabled)
        .show(ui)
}

/// Button that turns into "■ done/total" with a progress line while its task runs.
/// Its width is the widest of both states, so neighbors never move.
pub fn task_button(
    ui: &mut Ui,
    icon: Icon,
    idle: &str,
    running: Option<(u64, Option<u64>)>,
    enabled: bool,
) -> Response {
    let measure = |ui: &Ui, text: &str| {
        ui.painter()
            .layout_no_wrap(
                text.to_owned(),
                FontId::proportional(theme::BODY),
                Color32::PLACEHOLDER,
            )
            .size()
            .x
            + 16.
            + 6.
            + 20.
    };
    // One width idle and running ("Stop 12/82"), so neighbors do not move;
    // only a count of three digits widens it.
    let sample = match running.and_then(|r| r.1).filter(|t| *t >= 100) {
        Some(t) => format!("Stop {t}/{t}"),
        None => "Stop 88/88".into(),
    };
    let width = measure(ui, idle).max(measure(ui, &sample)).ceil();
    let response = match running {
        None => Button::secondary(idle)
            .icon(icon)
            .width(width)
            .enabled(enabled)
            .show(ui),
        Some((done, total)) => {
            let text = match total {
                Some(t) => format!("Stop {done}/{t}"),
                None => "Stop".into(),
            };
            Button::secondary(&text)
                .icon(Icon::Stop)
                .width(width)
                .show(ui)
        }
    };
    if let Some((done, Some(total))) = running.filter(|r| r.1.is_some_and(|t| t > 0)) {
        progress_line(
            ui,
            response.rect.shrink2(vec2(3., 1.)),
            done as f32 / total as f32,
        );
    }
    if running.is_some() {
        response.on_hover_text(format!("Stop {}", idle.to_lowercase()))
    } else {
        response
    }
}
/// 2-pt accent line along the bottom of `rect`.
pub fn progress_line(ui: &Ui, rect: Rect, fraction: f32) {
    let p = theme::palette(ui);
    let track = Rect::from_min_max(rect.left_bottom() - vec2(0., 2.), rect.right_bottom());
    ui.painter().rect_filled(track, 1, p.subtle);
    let done = Rect::from_min_size(track.min, vec2(track.width() * fraction.clamp(0., 1.), 2.));
    ui.painter().rect_filled(done, 1, p.accent);
}

/// Compact switch with the whole label as hit area and checkbox semantics.
pub fn toggle(ui: &mut Ui, value: &mut bool, label: &str) -> Response {
    toggle_named(ui, value, label, label)
}
/// An optional visible label and a required accessible name for a switch.
pub fn toggle_named(ui: &mut Ui, value: &mut bool, label: &str, accessible: &str) -> Response {
    let p = theme::palette(ui);
    let enabled = ui.is_enabled();
    let text = ui.painter().layout_no_wrap(
        label.to_owned(),
        FontId::proportional(theme::BODY),
        Color32::PLACEHOLDER,
    );
    let width = 30.
        + if label.is_empty() {
            0.
        } else {
            8. + text.size().x
        };
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, CONTROL), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let how_on = ui.ctx().animate_bool_responsive(response.id, *value);
    let track = Rect::from_center_size(rect.left_center() + vec2(15., 0.), vec2(30., 18.));
    let off = p.control_border;
    let ink = if !enabled {
        p.subtle
    } else {
        off.lerp_to_gamma(p.action_bg, how_on)
    };
    ui.painter().rect_filled(track, 9, ink);
    let knob = pos2(track.left() + 9. + how_on * 12., track.center().y);
    ui.painter()
        .circle_filled(knob + vec2(0., 0.5), 7., Color32::from_black_alpha(30));
    ui.painter()
        .circle_filled(knob, 7., if enabled { Color32::WHITE } else { p.surface });
    if !label.is_empty() {
        ui.painter().galley(
            pos2(track.right() + 8., rect.center().y - text.size().y / 2.),
            text,
            if enabled { p.foreground } else { p.faint },
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            track.expand(2.),
            11,
            Stroke::new(2_f32, p.accent),
            egui::StrokeKind::Outside,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, *value, accessible)
    });
    response
}

/// Segmented control: one of a few short options, all visible.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let before = *value;
    let p = theme::palette(ui);
    egui::Frame::NONE
        .fill(p.subtle)
        .corner_radius(RADIUS_CONTROL)
        .inner_margin(2)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.;
            ui.horizontal(|ui| {
                for (option, label) in options {
                    let active = *value == *option;
                    let galley = ui.painter().layout_no_wrap(
                        (*label).to_owned(),
                        FontId::proportional(theme::SMALL),
                        Color32::PLACEHOLDER,
                    );
                    let (rect, r) = ui.allocate_exact_size(
                        vec2(galley.size().x + 20., CONTROL - 4.),
                        Sense::click(),
                    );
                    if active {
                        // Raised: lighter than the track (a dark surface would sink in).
                        let (fill, stroke) = match p.dark {
                            true => (p.control_border, Stroke::NONE),
                            false => (p.surface, Stroke::new(1_f32, p.border)),
                        };
                        ui.painter().rect(
                            rect,
                            RADIUS_CONTROL - 1,
                            fill,
                            stroke,
                            egui::StrokeKind::Inside,
                        );
                    } else if r.hovered() {
                        ui.painter().rect_filled(rect, RADIUS_CONTROL - 1, p.hover);
                    }
                    let ink = if active { p.foreground } else { p.muted };
                    ui.painter()
                        .galley(rect.center() - galley.size() / 2., galley, ink);
                    focus_ring(ui, &r);
                    r.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::RadioButton, true, active, *label)
                    });
                    if r.clicked() {
                        *value = *option;
                    }
                }
            });
        });
    *value != before
}

/// The button of a toolbar tool: icon, label and a count badge; pressed while
/// its panel is open. Phones show the icon only (the label becomes the tooltip).
pub fn tool_button(ui: &mut Ui, icon: Icon, label: &str, count: usize) -> Response {
    let text = if phone(ui) { "" } else { label };
    let r = Button::secondary(text)
        .icon(icon)
        .count((count > 0).then_some(count))
        .show(ui);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    if text.is_empty() {
        r.on_hover_text(label)
    } else {
        r
    }
}
