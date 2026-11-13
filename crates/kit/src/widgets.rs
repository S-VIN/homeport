//! Domain-free widgets. Every function draws with tokens from [`crate::theme`]
//! and the palette, so plugin screens look identical without custom styling.
//!
//! Page anatomy (top to bottom): [`page_header`] (title · counter … actions),
//! optional [`helper`] text, optional [`add_bar`], then [`card`]s or a
//! [`list_card`] whose rows come from [`row`].
pub use crate::icons::Icon;
pub use crate::palette::Tone;
use crate::theme::{self, CONTROL, INSET, RADIUS, RADIUS_CONTROL};
use egui::{self, pos2, vec2, Align, Color32, FontId, Layout, Rect, Response, RichText, Sense, Stroke, Ui};
use std::fmt::Display;

// ---- text -----------------------------------------------------------------------

pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(theme::bold(size))
}
/// Muted secondary text (counters, short notes). Does not wrap.
pub fn hint(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.add(egui::Label::new(RichText::new(text).size(theme::SMALL).color(theme::palette(ui).muted)).truncate())
}
/// Small muted column caption.
pub fn caption(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.add(egui::Label::new(RichText::new(text).size(theme::CAPTION).color(theme::palette(ui).muted)).truncate())
}
/// Small text in a status tone (row subtitles, inline errors). Wraps.
pub fn tone_text(ui: &mut Ui, text: impl Into<String>, tone: Tone) -> Response {
    let ink = theme::palette(ui).tone(tone);
    ui.add(egui::Label::new(RichText::new(text).size(theme::SMALL).color(ink)).wrap())
}
/// One-line page explanation under the header. Same style on every page.
pub fn helper(ui: &mut Ui, text: &str) {
    let p = theme::palette(ui);
    ui.add(egui::Label::new(RichText::new(text).size(theme::SMALL).color(p.muted)).wrap());
    ui.add_space(theme::SPACE_M);
}
/// `1 server`, `2 servers`.
/// `1.2 MB`, `640 KB`, `12 B` (decimal units, like file managers).
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1000. && unit < UNITS.len() - 1 {
        v /= 1000.;
        unit += 1;
    }
    if unit == 0 || v >= 100. {
        format!("{v:.0} {}", UNITS[unit])
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}
/// `1.2 MB/s`.
pub fn rate(bytes_per_sec: u64) -> String {
    format!("{}/s", bytes(bytes_per_sec))
}
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ---- icons ----------------------------------------------------------------------

/// An inline icon of `size` points.
pub fn icon(ui: &mut Ui, icon: Icon, size: f32, ink: Color32) -> Response {
    let (rect, r) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    crate::icons::paint_sized(ui.painter(), rect.center(), icon, size, ink);
    r
}

/// How an icon button looks at rest.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Emphasis {
    /// Muted ink.
    #[default]
    Normal,
    /// Faint until its row is hovered: keeps repeated row actions calm.
    Quiet,
}

/// 26×26 ghost icon button with a tooltip and an accessible label.
pub fn icon_button(ui: &mut Ui, icon: Icon, label: &str, enabled: bool) -> Response {
    icon_button_ex(ui, icon, label, enabled, Emphasis::Normal, false)
}
/// Icon button for row actions; `danger` turns it red on hover only.
pub fn icon_button_ex(ui: &mut Ui, icon: Icon, label: &str, enabled: bool, emphasis: Emphasis, danger: bool) -> Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(CONTROL, CONTROL), if enabled { Sense::click() } else { Sense::hover() });
    let active = enabled && (response.hovered() || response.has_focus());
    let ink = if !enabled {
        p.faint.gamma_multiply(0.6)
    } else if active {
        if danger { p.danger } else { p.foreground }
    } else if emphasis == Emphasis::Quiet {
        quiet_ink(&p)
    } else {
        p.muted
    };
    if active {
        let fill = if danger { p.soft(p.danger) } else { p.subtle };
        ui.painter().rect_filled(rect, RADIUS_CONTROL, fill);
    }
    let size = if matches!(icon, Icon::Up | Icon::Down) { 14. } else { crate::icons::SIZE };
    crate::icons::paint_sized(ui.painter(), rect.center(), icon, size, ink);
    focus_ring(ui, &response);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if enabled {
        response.on_hover_text(label)
    } else {
        response
    }
}

/// Ink of row actions at rest: lighter than `muted` but still ≥ 4.5:1 on the card.
pub fn quiet_ink(p: &crate::palette::Palette) -> Color32 {
    p.muted.lerp_to_gamma(p.faint, 0.35)
}

pub fn focus_ring(ui: &Ui, response: &Response) {
    if response.has_focus() {
        ui.painter().rect_stroke(response.rect, RADIUS_CONTROL, Stroke::new(2_f32, theme::palette(ui).accent), egui::StrokeKind::Inside);
    }
}

// ---- buttons --------------------------------------------------------------------

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
        Self { kind, icon: None, text, width: 0., enabled: true, selected: false, count: None, tone: None }
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
    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let font = FontId::proportional(theme::BODY);
        let galley = ui.painter().layout_no_wrap(self.text.to_owned(), font.clone(), Color32::PLACEHOLDER);
        let count = self.count.as_ref().map(|c| ui.painter().layout_no_wrap(c.clone(), FontId::proportional(theme::CAPTION), Color32::PLACEHOLDER));
        let pad = if self.text.is_empty() { 5. } else { 10. };
        let icon_w = if self.icon.is_some() { 16. + if self.text.is_empty() { 0. } else { 6. } } else { 0. };
        let count_w = count.as_ref().map_or(0., |c| c.size().x + 12.);
        let content = icon_w + galley.size().x + count_w;
        let width = (content + pad * 2.).max(self.width);
        let (rect, response) = ui.allocate_exact_size(vec2(width, CONTROL), if self.enabled { Sense::click() } else { Sense::hover() });
        let hovered = self.enabled && (response.hovered() || response.has_focus());
        let pressed = self.enabled && response.is_pointer_button_down_on();
        let (fill, stroke, ink) = match self.kind {
            Kind::Primary => (if hovered { p.action_hover } else { p.action_bg }, Stroke::NONE, p.action_fg),
            Kind::Danger => (p.danger, Stroke::NONE, if p.dark { p.background } else { Color32::WHITE }),
            Kind::Secondary if self.selected => (p.selected(), Stroke::new(1_f32, p.tint(p.accent, 0.45)), p.accent),
            Kind::Secondary => {
                let fill = if pressed { p.subtle } else if hovered { p.hover } else { p.surface };
                match self.tone.filter(|t| *t != Tone::Neutral) {
                    // A toned secondary (e.g. Disconnect): colored ink, tinted border.
                    Some(t) => (if hovered { p.soft(p.tone(t)) } else { fill }, Stroke::new(1_f32, p.tint(p.tone(t), if hovered { 0.7 } else { 0.45 })), p.tone(t)),
                    None => (fill, Stroke::new(1_f32, if hovered { p.faint } else { p.control_border }), p.foreground),
                }
            }
            Kind::Ghost => (
                if hovered || self.selected { p.subtle } else { Color32::TRANSPARENT },
                Stroke::NONE,
                self.tone.map_or(p.foreground, |t| p.tone(t)),
            ),
        };
        let (fill, ink) = if self.enabled { (fill, ink) } else { (if self.kind == Kind::Ghost { fill } else { p.subtle }, p.faint) };
        let painter = ui.painter();
        painter.rect(rect, RADIUS_CONTROL, fill, if self.enabled { stroke } else { Stroke::NONE }, egui::StrokeKind::Inside);
        let mut x = rect.center().x - content / 2.;
        if let Some(icon) = self.icon {
            crate::icons::paint_sized(painter, pos2(x + 8., rect.center().y), icon, 16., ink);
            x += icon_w;
        }
        painter.galley(pos2(x, rect.center().y - galley.size().y / 2.), galley, ink);
        if let Some(c) = count {
            let x = rect.center().x + content / 2. - count_w + 6.;
            let pill = Rect::from_min_size(pos2(x - 4., rect.center().y - 8.), vec2(c.size().x + 8., 16.));
            let pill_fill = match self.kind {
                Kind::Primary | Kind::Danger => Color32::from_white_alpha(50),
                _ if self.selected => p.tint(p.accent, 0.25),
                _ => p.subtle,
            };
            painter.rect_filled(pill, 8, pill_fill);
            painter.galley(pos2(x, rect.center().y - c.size().y / 2.), c, ink);
        }
        focus_ring(ui, &response);
        let label = self.text.to_owned();
        response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, self.enabled, self.selected, &label));
        response
    }
}

/// Primary call to action.
pub fn action(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    Button::primary(text).enabled(enabled).show(ui).clicked()
}
/// Secondary button with a minimum width.
pub fn button(ui: &mut Ui, text: &str, width: f32, enabled: bool) -> Response {
    Button::secondary(text).width(width).enabled(enabled).show(ui)
}

/// Button that turns into "■ done/total" with a progress line while its task runs.
/// Its width is the widest of both states, so neighbors never move.
pub fn task_button(ui: &mut Ui, icon: Icon, idle: &str, width: f32, running: Option<(u64, Option<u64>)>, enabled: bool) -> Response {
    let measure = |ui: &Ui, text: &str| ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(theme::BODY), Color32::PLACEHOLDER).size().x + 16. + 6. + 20.;
    let sample = match running.and_then(|r| r.1) {
        Some(t) => format!("{t}/{t}"),
        None => "888/888".into(),
    };
    let width = width.max(measure(ui, idle)).max(measure(ui, &sample)).ceil();
    let response = match running {
        None => Button::secondary(idle).icon(icon).width(width).enabled(enabled).show(ui),
        Some((done, total)) => {
            let text = match total {
                Some(t) => format!("{done}/{t}"),
                None => "Stop".into(),
            };
            Button::secondary(&text).icon(Icon::Stop).width(width).show(ui)
        }
    };
    if let Some((done, Some(total))) = running.filter(|r| r.1.is_some_and(|t| t > 0)) {
        progress_line(ui, response.rect.shrink2(vec2(3., 1.)), done as f32 / total as f32);
    }
    response.on_hover_text(if running.is_some() { format!("Stop {}", idle.to_lowercase()) } else { idle.to_owned() })
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
    let p = theme::palette(ui);
    let enabled = ui.is_enabled();
    let text = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(theme::BODY), Color32::PLACEHOLDER);
    let width = 30. + if label.is_empty() { 0. } else { 8. + text.size().x };
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, CONTROL), Sense::click());
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
    }
    let how_on = ui.ctx().animate_bool_responsive(response.id, *value);
    let track = Rect::from_center_size(rect.left_center() + vec2(15., 0.), vec2(30., 18.));
    let off = p.control_border;
    let ink = if !enabled { p.subtle } else { off.lerp_to_gamma(p.action_bg, how_on) };
    ui.painter().rect_filled(track, 9, ink);
    let knob = pos2(track.left() + 9. + how_on * 12., track.center().y);
    ui.painter().circle_filled(knob + vec2(0., 0.5), 7., Color32::from_black_alpha(30));
    ui.painter().circle_filled(knob, 7., if enabled { Color32::WHITE } else { p.surface });
    if !label.is_empty() {
        ui.painter().galley(pos2(track.right() + 8., rect.center().y - text.size().y / 2.), text, if enabled { p.foreground } else { p.faint });
    }
    if response.has_focus() {
        ui.painter().rect_stroke(track.expand(2.), 11, Stroke::new(2_f32, p.accent), egui::StrokeKind::Outside);
    }
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, enabled, *value, label));
    response
}

/// Segmented control: one of a few short options, all visible.
pub fn segmented<T: Copy + PartialEq>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let before = *value;
    let p = theme::palette(ui);
    egui::Frame::NONE.fill(p.subtle).corner_radius(RADIUS_CONTROL).inner_margin(2).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.;
        ui.horizontal(|ui| {
            for (option, label) in options {
                let active = *value == *option;
                let galley = ui.painter().layout_no_wrap((*label).to_owned(), FontId::proportional(theme::SMALL), Color32::PLACEHOLDER);
                let (rect, r) = ui.allocate_exact_size(vec2(galley.size().x + 20., CONTROL - 4.), Sense::click());
                if active {
                    ui.painter().rect(rect, RADIUS_CONTROL - 1, p.surface, Stroke::new(1_f32, p.border), egui::StrokeKind::Inside);
                } else if r.hovered() {
                    ui.painter().rect_filled(rect, RADIUS_CONTROL - 1, p.hover);
                }
                let ink = if active { p.foreground } else { p.muted };
                ui.painter().galley(rect.center() - galley.size() / 2., galley, ink);
                focus_ring(ui, &r);
                r.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, active, *label));
                if r.clicked() {
                    *value = *option;
                }
            }
        });
    });
    *value != before
}

// ---- inputs ---------------------------------------------------------------------

/// Single-line text input. Emoji are drawn from the bundled atlas.
pub fn input(ui: &mut Ui, id: impl egui::AsIdSalt, value: &mut String, placeholder: &str, width: f32, secret: bool) -> Response {
    ui.push_id(id, |ui| {
        let response = if secret {
            ui.add(
                egui::TextEdit::singleline(value)
                    .desired_width(width.max(24.))
                    .min_size(vec2(0., CONTROL))
                    .hint_text(placeholder)
                    .password(true)
                    .margin(egui::Margin::symmetric(8, 5)),
            )
        } else {
            crate::emoji::edit(ui, value, width.max(24.), placeholder)
        };
        response
    })
    .inner
}

/// Combo box over a closed set of values. Returns true when changed.
pub fn choice<T: Copy + Display + PartialEq>(ui: &mut Ui, id: impl egui::AsIdSalt, value: &mut T, options: &[T], width: f32, help: &str) -> bool {
    let before = *value;
    select(ui, id, &value.to_string(), width, |ui| {
        for &option in options {
            ui.selectable_value(value, option, option.to_string());
        }
    })
    .on_hover_text(help);
    *value != before
}
/// Combo box with a fixed width; `menu` adds the options.
pub fn select(ui: &mut Ui, id: impl egui::AsIdSalt, selected: &str, width: f32, menu: impl FnOnce(&mut Ui)) -> Response {
    ui.allocate_ui_with_layout(vec2(width, CONTROL), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().button_padding = vec2(8., 4.);
        egui::ComboBox::from_id_salt(id)
            .width(width)
            .height(320.)
            .truncate()
            .icon(|ui, rect, _, open| {
                let ink = theme::palette(ui).muted;
                crate::icons::paint_sized(ui.painter(), rect.center(), if open { Icon::ChevronUp } else { Icon::ChevronDown }, 12., ink);
            })
            .selected_text(selected)
            .show_ui(ui, |ui| {
                ui.set_min_width(width - 8.);
                menu(ui)
            })
            .response
    })
    .inner
}

/// A single-line TextEdit surrenders focus on Enter in the same frame.
pub fn submitted(ui: &Ui, response: &Response) -> bool {
    ui.is_enabled() && (response.has_focus() || response.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter))
}
/// Enter or blur: the moment an inline edit is saved.
/// A text field bound to a saved value: edits stay in a draft (kept across
/// frames) and the new text is returned once, on Enter or when focus leaves.
/// Escape restores the saved value.
pub fn field(ui: &mut Ui, id: impl egui::AsIdSalt, saved: &str, placeholder: &str, width: f32) -> Option<String> {
    let key = egui::Id::unique(("kit-field", &id));
    let mut draft: String = ui.data(|d| d.get_temp(key)).unwrap_or_else(|| saved.to_owned());
    let r = input(ui, id, &mut draft, placeholder, width, false);
    let mut out = None;
    if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        draft = saved.to_owned();
    }
    if committed(ui, &r) {
        if draft != saved {
            out = Some(draft.clone());
        }
        ui.data_mut(|d| d.remove::<String>(key));
    } else if r.has_focus() || draft != saved {
        ui.data_mut(|d| d.insert_temp(key, draft));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
    }
    out
}
pub fn committed(ui: &Ui, response: &Response) -> bool {
    ui.is_enabled() && (response.lost_focus() || submitted(ui, response))
}

// ---- page structure -------------------------------------------------------------

/// Page header: title and a muted counter on the left, actions on the right.
/// The counter hides when the actions need its room.
pub fn page_header(ui: &mut Ui, title: &str, counter: &str, actions: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    let id = ui.scope_id().with(("page-header", title));
    let actions_width: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.);
    let width = ui.available_width();
    ui.allocate_ui_with_layout(vec2(width, 30.), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(width, 30.));
        ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
        let title = ui.add(egui::Label::new(bold(title, theme::TITLE).color(p.foreground)).truncate());
        let room = width - title.rect.width() - actions_width - 3. * theme::HORIZONTAL_GAP;
        // Long counter, then its first part ("83 servers"), then the number alone.
        let first = counter.split(" · ").next().unwrap_or(counter);
        let number = first.split(' ').next().unwrap_or(first);
        let fits = |text: &str| ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(theme::SMALL), p.muted).size().x <= room;
        if let Some(text) = [counter, first, number].into_iter().find(|t| !t.is_empty() && fits(t)) {
            ui.add_space(2.);
            let r = ui.label(RichText::new(text).size(theme::SMALL).color(p.muted));
            if text != counter {
                r.on_hover_text(counter);
            }
        }
        let left = ui.min_rect().right();
        let free = (ui.max_rect().right() - left - actions_width - theme::HORIZONTAL_GAP).max(0.);
        ui.add_space(free);
        let r = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
            actions(ui)
        });
        let w = r.response.rect.width();
        if (w - actions_width).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(id, w));
            ui.ctx().request_discard("page header actions resized");
        }
    });
    ui.add_space(theme::SPACE_S);
}

/// Inputs and an "Add" button in one row above a list. `fields` gets the width
/// left for inputs and returns true on Enter. Returns true when the user adds.
pub fn add_bar(ui: &mut Ui, label: &str, enabled: bool, fields: impl FnOnce(&mut Ui, f32) -> bool) -> bool {
    let mut add = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(enabled, |ui| {
            let button = 64.;
            let width = ui.available_width() - button - theme::HORIZONTAL_GAP;
            let enter = fields(ui, width);
            let clicked = Button::primary("Add").icon(Icon::Add).width(button).enabled(enabled).show(ui).on_hover_text(label).clicked();
            add = enter || clicked;
        });
    });
    ui.add_space(theme::SPACE_M);
    add
}

fn card_frame(ui: &Ui, margin: f32) -> egui::Frame {
    let p = theme::palette(ui);
    egui::Frame::NONE.fill(p.surface).stroke(Stroke::new(1_f32, p.border)).corner_radius(RADIUS).inner_margin(margin)
}
/// Raised surface grouping related content.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
    let r = card_frame(ui, INSET).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add(ui)
    });
    ui.add_space(theme::SPACE_M);
    r
}
fn settings_first() -> egui::Id {
    egui::Id::unique("kit-settings-first-row")
}
/// Start a group of [`settings_row`]s outside [`settings_card`] (no divider above the first).
pub fn settings_start(ui: &Ui) {
    ui.data_mut(|d| d.insert_temp(settings_first(), true));
}
/// Card holding [`settings_row`]s (rows bring their own vertical padding).
pub fn settings_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.data_mut(|d| d.insert_temp(settings_first(), true));
    let r = card_frame(ui, 0.)
        .inner_margin(egui::Margin::symmetric(INSET as i8, 2))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(theme::SPACE_M);
    r
}
/// Vertical scroll for a page body; keeps card borders off the clip edge.
pub fn page_scroll<R>(ui: &mut Ui, id: impl egui::AsIdSalt, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::ScrollArea::vertical()
        .id_salt(id)
        .auto_shrink([false, false])
        .show(ui, |ui| egui::Frame::NONE.inner_margin(egui::Margin { left: 0, right: 7, top: 0, bottom: 1 }).show(ui, add).inner)
        .inner
}
/// Card for a list: rows from [`row`] fill it edge to edge.
pub fn list_card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let r = card_frame(ui, theme::SPACE_S)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.;
            add(ui)
        })
        .inner;
    ui.add_space(theme::SPACE_M);
    r
}
/// Column captions at the top of a list card, followed by a divider.
pub fn list_header(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    // Same horizontal inset as the rows below, on both sides.
    let inset = INSET - theme::SPACE_S;
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 26.), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(ui.available_width(), 26.));
        ui.add_space(inset);
        ui.allocate_ui_with_layout(vec2(ui.available_width() - inset, 26.), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(vec2(ui.available_width(), 26.));
            add(ui);
        });
    });
    separator(ui);
    ui.add_space(theme::SPACE_XS);
}
/// Section title above a card. The only kind of group heading: cards never
/// carry their own captions.
pub fn section(ui: &mut Ui, title: &str, help: Option<&str>) {
    section_note(ui, title, "", help);
}
/// [`section`] with a short muted note on the right ("One at a time").
pub fn section_note(ui: &mut Ui, title: &str, note: &str, help: Option<&str>) {
    let p = theme::palette(ui);
    ui.add_space(theme::SPACE_S);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 18.), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_width(ui.available_width());
        ui.add_space(2.);
        let r = ui.label(bold(title, theme::SMALL).color(p.muted));
        if let Some(help) = help {
            r.on_hover_text(help);
        }
        if !note.is_empty() {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(2.);
                ui.label(RichText::new(note).size(theme::CAPTION).color(p.muted));
            });
        }
    });
    ui.add_space(theme::SPACE_XS);
}

/// What [`row`] tells its content.
#[derive(Clone, Copy)]
pub struct RowState {
    /// The pointer is over the row (show quiet actions in full).
    pub hovered: bool,
}
/// One list row: rounded hover fill, selected fill, min height [`theme::ROW`].
pub fn row<R>(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, selected: bool, add: impl FnOnce(&mut Ui, RowState) -> R) -> R {
    row_ex(ui, id, selected, 0., add)
}
/// [`row`] with a fading highlight (`0..=1`), e.g. for a row that was just added.
pub fn row_ex<R>(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, selected: bool, highlight: f32, add: impl FnOnce(&mut Ui, RowState) -> R) -> R {
    let p = theme::palette(ui);
    let id = ui.scope_id().with(("kit-row", id));
    let previous: Option<Rect> = ui.data(|d| d.get_temp(id));
    let hovered = previous.is_some_and(|r| ui.rect_contains_pointer(r)) && !ui.ctx().is_being_dragged(egui::Id::NULL);
    let background = ui.painter().add(egui::Shape::Noop);
    let inner = egui::Frame::NONE.inner_margin(egui::Margin::symmetric((INSET - theme::SPACE_S) as i8, 0)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add(ui, RowState { hovered })
    });
    let rect = inner.response.rect;
    if previous != Some(rect) {
        ui.data_mut(|d| d.insert_temp(id, rect));
    }
    let base = if selected {
        Some(p.selected())
    } else if hovered {
        Some(p.hover)
    } else {
        None
    };
    let fill = if highlight > 0. {
        let flash = p.tint(p.accent, if p.dark { 0.3 } else { 0.2 });
        Some(base.unwrap_or(p.surface).lerp_to_gamma(flash, highlight.min(1.)))
    } else {
        base
    };
    if let Some(fill) = fill {
        ui.painter().set(background, egui::Shape::rect_filled(rect, RADIUS_CONTROL, fill));
    }
    inner.inner
}
/// The main line of a row: fixed height, vertically centered, left to right.
pub fn row_line<R>(ui: &mut Ui, height: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(vec2(ui.available_width(), height), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_size(vec2(ui.available_width(), height));
        ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
        add(ui)
    })
    .inner
}

/// Fixed-width cell of a row, vertically centered.
pub fn cell<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(vec2(width, CONTROL), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(width);
        add(ui)
    })
    .inner
}
/// Right-aligned fixed-width cell (numbers).
pub fn cell_right<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(vec2(width, CONTROL), Layout::right_to_left(Align::Center), |ui| {
        ui.set_width(width);
        add(ui)
    })
    .inner
}

/// Move up / move down buttons. `Some(true)` = up.
pub fn order(ui: &mut Ui, index: usize, len: usize, emphasis: Emphasis) -> Option<bool> {
    ui.spacing_mut().item_spacing.x = 0.;
    let up = icon_button_ex(ui, Icon::Up, "Move up", index > 0, emphasis, false).clicked();
    let down = icon_button_ex(ui, Icon::Down, "Move down", index + 1 < len, emphasis, false).clicked();
    ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
    if up {
        Some(true)
    } else if down {
        Some(false)
    } else {
        None
    }
}

/// Delete button of a row: one click deletes, the caller offers Undo in a
/// toast ([`crate`] users: `UiCx::undo`). It turns red only under the pointer.
pub fn remove(ui: &mut Ui, emphasis: Emphasis) -> bool {
    icon_button_ex(ui, Icon::Delete, "Delete", true, emphasis, true).clicked()
}

/// Save (✓) and Cancel (✕) of an inline edit, side by side. Returns
/// `Some(true)` on save, `Some(false)` on cancel.
pub fn edit_actions(ui: &mut Ui, can_save: bool) -> Option<bool> {
    ui.spacing_mut().item_spacing.x = 0.;
    let save = icon_button_ex(ui, Icon::Check, "Save · Enter", can_save, Emphasis::Normal, false).clicked();
    let cancel = icon_button_ex(ui, Icon::Close, "Cancel · Esc", true, Emphasis::Normal, false).clicked();
    ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
    if save {
        Some(true)
    } else if cancel {
        Some(false)
    } else {
        None
    }
}

/// A truncated one-line label with exactly one tooltip: `tooltip` when given,
/// otherwise the full text when it is cut off.
pub fn truncated(ui: &mut Ui, text: impl Into<egui::WidgetText>, tooltip: Option<&str>) -> Response {
    let r = ui.add(egui::Label::new(text).truncate().show_tooltip_when_elided(tooltip.is_none()));
    match tooltip {
        Some(t) if !t.is_empty() => r.on_hover_text(t),
        _ => r,
    }
}

/// An action that is a small ghost button with an icon in a row of actions,
/// or an item of a context menu (`menu`). Returns true when clicked.
pub fn action_item(ui: &mut Ui, icon: Icon, label: &str, enabled: bool, menu: bool) -> bool {
    if menu {
        let p = theme::palette(ui);
        let width = ui.available_width().max(190.);
        let (rect, r) = ui.allocate_exact_size(vec2(width, CONTROL), if enabled { Sense::click() } else { Sense::hover() });
        let hot = enabled && (r.hovered() || r.has_focus());
        if hot {
            ui.painter().rect_filled(rect, RADIUS_CONTROL, p.subtle);
        }
        let ink = if !enabled { p.faint } else if hot { p.foreground } else { p.muted };
        crate::icons::paint_sized(ui.painter(), rect.left_center() + vec2(14., 0.), icon, 15., ink);
        let g = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(theme::BODY), if enabled { p.foreground } else { p.faint });
        ui.painter().galley(rect.left_center() + vec2(30., -g.size().y / 2.), g, p.foreground);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MenuItem, enabled, label));
        let clicked = r.clicked();
        if clicked {
            ui.close();
        }
        clicked
    } else {
        Button::secondary(label).icon(icon).enabled(enabled).show(ui).clicked()
    }
}

/// Empty state that teaches the next step.
pub fn empty(ui: &mut Ui, icon: Icon, title: &str, hint_text: &str) {
    let p = theme::palette(ui);
    ui.add_space(theme::SPACE_XL);
    ui.vertical_centered(|ui| {
        crate::icons::paint_sized(ui.painter(), ui.cursor().center_top() + vec2(0., 13.), icon, 26., p.faint);
        ui.add_space(30.);
        ui.label(bold(title, theme::BODY).color(p.foreground));
        ui.add_space(2.);
        ui.set_max_width(ui.available_width().min(340.));
        ui.add(egui::Label::new(RichText::new(hint_text).size(theme::SMALL).color(p.muted)).wrap());
    });
    ui.add_space(theme::SPACE_XL);
}

/// Tinted strip with an icon, text and an optional action. Returns true when
/// the action is clicked.
pub fn banner(ui: &mut Ui, tone: Tone, text: &str, action_label: Option<&str>) -> bool {
    let p = theme::palette(ui);
    let ink = p.tone(tone);
    let mut clicked = false;
    egui::Frame::NONE
        .fill(p.soft(ink))
        .stroke(Stroke::new(1_f32, p.tint(ink, 0.3)))
        .corner_radius(RADIUS)
        .inner_margin(egui::Margin::symmetric(INSET as i8, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                let icon_kind = match tone {
                    Tone::Danger => Icon::Error,
                    Tone::Warning => Icon::Warning,
                    Tone::Success => Icon::Connected,
                    _ => Icon::Info,
                };
                icon(ui, icon_kind, 16., ink);
                let action_w = if action_label.is_some() { 110. } else { 0. };
                ui.allocate_ui_with_layout(vec2(ui.available_width() - action_w, 0.), Layout::top_down(Align::Min), |ui| {
                    ui.add(egui::Label::new(RichText::new(text).color(p.foreground)).wrap());
                });
                if let Some(a) = action_label {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        clicked = Button::secondary(a).show(ui).clicked();
                    });
                }
            });
        });
    ui.add_space(theme::SPACE_M);
    clicked
}

// ---- settings -------------------------------------------------------------------

/// A row of a settings card: label and description on the left, the control on
/// the right; stacks under a narrow width. Rows after the first get a divider.
pub fn settings_row(ui: &mut Ui, label: &str, help: &str, control: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    settings_item(
        ui,
        label,
        |ui| {
            ui.add(egui::Label::new(RichText::new(label).color(p.foreground)).wrap());
            if !help.is_empty() {
                ui.add(egui::Label::new(RichText::new(help).size(theme::SMALL).color(p.muted)).wrap());
            }
        },
        control,
    );
}
/// [`settings_row`] with custom content on the left (title with badges, notes…).
pub fn settings_item(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, left: impl FnOnce(&mut Ui), control: impl FnOnce(&mut Ui)) {
    let first = ui.data_mut(|d| std::mem::replace(d.get_temp_mut_or(settings_first(), false), false));
    if !first {
        separator(ui);
    }
    ui.add_space(SETTINGS_PAD);
    let id = ui.scope_id().with(("settings-row", id));
    let control_width: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(60.);
    // Small controls (switches, short buttons) stay right of the label; wide ones
    // go under it when the label would get too little room.
    let narrow = ui.available_width() - control_width - theme::SPACE_L < 170.;
    let remember = |ui: &mut Ui, width: f32| {
        if (width - control_width).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(id, width));
            ui.ctx().request_discard("settings row resized");
        }
    };
    let text = |ui: &mut Ui, width: f32, left: Box<dyn FnOnce(&mut Ui) + '_>| {
        ui.allocate_ui_with_layout(vec2(width, 0.), Layout::top_down(Align::Min), |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 2.;
            left(ui);
        });
    };
    if narrow {
        text(ui, ui.available_width(), Box::new(left));
        ui.add_space(theme::SPACE_S);
        let r = ui.horizontal(control).response;
        remember(ui, r.rect.width());
        ui.add_space(SETTINGS_PAD);
        return;
    }
    ui.horizontal(|ui| {
        let total = ui.available_width();
        let label_width = (total - control_width - theme::SPACE_L).max(120.);
        text(ui, label_width, Box::new(left));
        ui.add_space((total - label_width - control_width - theme::HORIZONTAL_GAP * 2.).max(0.));
        let r = ui.horizontal(control).response;
        remember(ui, r.rect.width());
    });
    ui.add_space(SETTINGS_PAD);
}
const SETTINGS_PAD: f32 = 7.;
/// A caption line at the top of a settings card (e.g. a plugin group).
pub fn settings_caption(ui: &mut Ui, text: &str, help: Option<&str>) {
    ui.add_space(SETTINGS_PAD);
    let r = ui.label(bold(text, theme::CAPTION).color(theme::palette(ui).muted));
    if let Some(h) = help {
        r.on_hover_text(h);
    }
}

const PERIODS: [(u32, &str); 7] = [
    (900, "Every 15 minutes"),
    (3600, "Every hour"),
    (10800, "Every 3 hours"),
    (21600, "Every 6 hours"),
    (43200, "Every 12 hours"),
    (86400, "Every day"),
    (604800, "Every week"),
];
/// Period picker: presets, or a number of minutes or hours under "Custom".
/// Returns true when the user committed a new value.
pub fn interval(ui: &mut Ui, id: impl egui::AsIdSalt, value: &mut u32) -> bool {
    let key = egui::Id::unique(("kit-interval", id));
    let mut custom: bool = ui.data(|d| d.get_temp(key.with("custom"))).unwrap_or(false) || !PERIODS.iter().any(|(s, _)| s == value);
    let mut unit: u32 = ui.data(|d| d.get_temp(key.with("unit"))).unwrap_or(if value.is_multiple_of(3600) { 3600 } else { 60 });
    let mut amount: u32 = ui.data(|d| d.get_temp(key.with("amount"))).unwrap_or((*value / unit).max(1));
    let mut next = None;
    ui.push_id(key, |ui| {
        if custom {
            let r = ui.add_sized(
                [56., CONTROL],
                egui::DragValue::new(&mut amount).range(1..=if unit == 60 { 10_080 } else { 168 }).speed(0.2),
            );
            if crate::widgets::committed(ui, &r) || r.drag_stopped() {
                next = Some(amount * unit);
            }
            let units = [(60, "minutes"), (3600, "hours")];
            let label = units.iter().find(|(u, _)| *u == unit).map_or("minutes", |(_, l)| l);
            select(ui, "unit", label, 92., |ui| {
                for (u, l) in units {
                    if ui.selectable_value(&mut unit, u, l).clicked() {
                        amount = ((*value + u / 2) / u).clamp(1, if u == 60 { 10_080 } else { 168 });
                        next = Some(amount * u);
                    }
                }
            });
        }
        let label = if custom { "Custom".to_owned() } else { PERIODS.iter().find(|(s, _)| s == value).map_or("Custom", |(_, l)| l).to_owned() };
        select(ui, "preset", &label, 150., |ui| {
            for (secs, text) in PERIODS {
                if ui.selectable_label(!custom && *value == secs, text).clicked() {
                    custom = false;
                    next = Some(secs);
                }
            }
            if ui.selectable_label(custom, "Custom").clicked() {
                custom = true;
                unit = if value.is_multiple_of(3600) { 3600 } else { 60 };
                amount = (*value / unit).max(1);
            }
        });
    });
    ui.data_mut(|d| {
        d.insert_temp(key.with("custom"), custom);
        d.insert_temp(key.with("unit"), unit);
        d.insert_temp(key.with("amount"), amount);
    });
    match next {
        Some(secs) if secs != *value => {
            *value = secs;
            true
        }
        _ => false,
    }
}

// ---- small pieces ---------------------------------------------------------------

/// Thin divider across the available width.
pub fn separator(ui: &mut Ui) {
    let p = theme::palette(ui);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment([pos2(r.left(), r.top() + 0.5), pos2(r.right(), r.top() + 0.5)], Stroke::new(1_f32, p.border));
    ui.add_space(1.);
}

/// Small rounded label in a status tone (e.g. "Required", "Domain").
pub fn badge(ui: &mut Ui, text: &str, tone: Tone) -> Response {
    let p = theme::palette(ui);
    let (ink, fill) = match tone {
        Tone::Neutral => (p.muted, p.subtle),
        t => (p.tone(t), p.soft(p.tone(t))),
    };
    pill(ui, text, ink, fill, f32::INFINITY)
}
/// Colored tag of a category (e.g. the subscription a server came from).
pub fn tag(ui: &mut Ui, text: &str, hue: usize, max_width: f32) -> Response {
    let (ink, fill) = theme::palette(ui).tag(hue);
    pill(ui, text, ink, fill, max_width)
}
/// The dot that stands for a tag color.
pub fn tag_dot(ui: &mut Ui, hue: usize) -> Response {
    let (ink, _) = theme::palette(ui).tag(hue);
    dot(ui, ink)
}
fn pill(ui: &mut Ui, text: &str, ink: Color32, fill: Color32, max_width: f32) -> Response {
    let font = FontId::proportional(theme::CAPTION);
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, ink);
    job.wrap = egui::text::TextWrapping::truncate_at_width((max_width - 12.).max(12.));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let (rect, r) = ui.allocate_exact_size(vec2(galley.size().x + 12., 18.), Sense::hover());
    ui.painter().rect_filled(rect, 4, fill);
    let elided = galley.elided;
    ui.painter().galley(rect.center() - galley.size() / 2., galley, ink);
    if elided {
        r.on_hover_text(text)
    } else {
        r
    }
}
/// Status dot + text.
pub fn status(ui: &mut Ui, text: &str, color: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.;
        dot(ui, color);
        ui.label(RichText::new(text).color(theme::palette(ui).foreground));
    });
}
pub fn dot(ui: &mut Ui, color: Color32) -> Response {
    let (r, response) = ui.allocate_exact_size(vec2(8., 8.), Sense::hover());
    ui.painter().circle_filled(r.center(), 4., color);
    response
}
/// Small spinner in the muted color.
pub fn spinner(ui: &mut Ui) -> Response {
    let ink = theme::palette(ui).muted;
    ui.add(egui::Spinner::new().size(13.).color(ink))
}

/// What leads a status-bar chip.
#[derive(Clone, Copy)]
pub enum Lead {
    Dot(Color32),
    Icon(Icon, Color32),
}
/// Status-bar chip: dot or icon, short text; click opens details.
pub fn chip(ui: &mut Ui, lead: Option<Lead>, text: &str, tooltip: &str) -> Response {
    let p = theme::palette(ui);
    let (job, icons) = crate::emoji::layout(text, FontId::proportional(theme::SMALL), p.foreground);
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let lead_w = match lead {
        None => 0.,
        Some(Lead::Dot(_)) => 14.,
        Some(Lead::Icon(..)) => 19.,
    };
    let width = galley.size().x + lead_w + 18.;
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.), Sense::click());
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 12, p.subtle);
    }
    let mut x = rect.left() + 9.;
    match lead {
        Some(Lead::Dot(c)) => {
            ui.painter().circle_filled(pos2(x + 4., rect.center().y), 4., c);
            x += lead_w;
        }
        Some(Lead::Icon(icon, c)) => {
            crate::icons::paint_sized(ui.painter(), pos2(x + 7., rect.center().y), icon, 14., c);
            x += lead_w;
        }
        None => {}
    }
    let pos = pos2(x, rect.center().y - galley.size().y / 2.);
    ui.painter().galley(pos, galley.clone(), p.foreground);
    crate::emoji::paint(ui, ui.painter(), pos, &galley, &icons);
    if response.has_focus() {
        ui.painter().rect_stroke(rect, 12, Stroke::new(2_f32, p.accent), egui::StrokeKind::Inside);
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

// ---- properties -----------------------------------------------------------------

/// One "label  value" line with the shared label column ([`theme::LABEL_WIDTH`]).
/// Long values are truncated; hovering the line offers a copy button.
pub fn property(ui: &mut Ui, label: &str, value: &str) {
    property_with(ui, label, |ui| {
        let p = theme::palette(ui);
        let long = value.chars().count() > 24;
        let id = ui.scope_id().with(("property", label));
        let hovered = ui.data(|d| d.get_temp::<Rect>(id)).is_some_and(|r| ui.rect_contains_pointer(r));
        let width = ui.available_width() - if long { 26. } else { 0. };
        let r = ui.allocate_ui_with_layout(vec2(width, 20.), Layout::left_to_right(Align::Center), |ui| {
            truncated(ui, RichText::new(value).color(p.foreground), None);
        });
        ui.data_mut(|d| d.insert_temp(id, r.response.rect.expand2(vec2(30., 2.))));
        if long && (hovered || ui.ctx().memory(|m| m.has_focus(id))) {
            let c = icon_button_ex(ui, Icon::Copy, "Copy", true, Emphasis::Normal, false);
            if c.clicked() {
                ui.ctx().copy_text(value.to_owned());
            }
        }
    });
}
/// A property line with a custom value (a tag, colored text…).
pub fn property_with(ui: &mut Ui, label: &str, value: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 22.), Layout::left_to_right(Align::Center), |ui| {
        ui.set_min_height(22.);
        ui.spacing_mut().item_spacing.x = theme::HORIZONTAL_GAP;
        cell(ui, theme::LABEL_WIDTH, |ui| {
            ui.add(egui::Label::new(RichText::new(label).size(theme::SMALL).color(p.muted)).truncate());
        });
        value(ui);
    });
}
/// Label → value lines.
pub fn properties(ui: &mut Ui, id: impl egui::AsIdSalt, rows: &[(String, String)]) {
    ui.push_id(id, |ui| {
        ui.spacing_mut().item_spacing.y = 0.;
        for (k, v) in rows {
            property(ui, k, v);
        }
    });
}
/// A "More details" disclosure; `add` draws the hidden content.
pub fn disclosure(ui: &mut Ui, id: impl egui::AsIdSalt, label: &str, add: impl FnOnce(&mut Ui)) {
    let id = egui::Id::unique(("kit-disclosure", id));
    let mut open: bool = ui.data(|d| d.get_temp(id)).unwrap_or(false);
    let p = theme::palette(ui);
    let r = ui.horizontal(|ui| {
        let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width().min(160.), 22.), Sense::click());
        let ink = if r.hovered() { p.foreground } else { p.muted };
        crate::icons::paint_sized(ui.painter(), pos2(rect.left() + 6., rect.center().y), if open { Icon::ChevronDown } else { Icon::ChevronRight }, 12., ink);
        let g = ui.painter().layout_no_wrap(label.to_owned(), FontId::proportional(theme::SMALL), ink);
        ui.painter().galley(pos2(rect.left() + 16., rect.center().y - g.size().y / 2.), g, ink);
        focus_ring(ui, &r);
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open, label));
        r
    });
    if r.inner.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    if open {
        add(ui);
    }
}
/// Labelled metrics side by side: caption above a value.
pub fn stats(ui: &mut Ui, items: &[(&str, String, Option<Color32>)]) {
    let p = theme::palette(ui);
    if items.is_empty() {
        return;
    }
    let gap = theme::SPACE_S;
    let width = ((ui.available_width() - gap * (items.len() as f32 - 1.)) / items.len() as f32).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (label, value, color) in items {
            let (rect, _) = ui.allocate_exact_size(vec2(width, 40.), Sense::hover());
            ui.painter().rect_filled(rect, RADIUS_CONTROL, if p.dark { p.subtle } else { p.background });
            let inner = rect.shrink2(vec2(8., 5.));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
            child.spacing_mut().item_spacing.y = 1.;
            child.add(egui::Label::new(RichText::new(*label).size(theme::CAPTION).color(p.muted)).truncate());
            child.add(egui::Label::new(bold(value.clone(), theme::BODY).color(color.unwrap_or(p.foreground))).truncate());
        }
    });
}

// ---- formatting -----------------------------------------------------------------

/// Relative age of a Unix timestamp.
pub fn age(time: u64) -> String {
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    match now.saturating_sub(time) {
        s @ 0..=59 => if s < 5 { "just now".into() } else { format!("{s} s ago") },
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s @ 3600..=86399 => format!("{} h ago", s / 3600),
        s => format!("{} d ago", s / 86400),
    }
}

/// Rating color: red < 40 ≤ amber < 75 ≤ green.
pub fn rating_color(ui: &Ui, rating: Option<u8>) -> Color32 {
    let p = theme::palette(ui);
    match rating {
        Some(0..=39) => p.danger,
        Some(40..=74) => p.warning,
        Some(_) => p.success,
        None => p.faint,
    }
}

/// A QR code of `data`, `size` points square, dark modules on a white quiet zone
/// (white in both themes: phone cameras read it reliably).
pub fn qr_code(ui: &mut Ui, data: &str, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let Ok(code) = qrcode::QrCode::new(data.as_bytes()) else {
        hint(ui, "Too long for a QR code");
        return response;
    };
    let n = code.width();
    let quiet = 2;
    let cell = size / (n + 2 * quiet) as f32;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, crate::theme::RADIUS, Color32::WHITE);
    let ink = Color32::from_rgb(17, 19, 23);
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color == qrcode::Color::Dark {
            let (x, y) = ((i % n + quiet) as f32, (i / n + quiet) as f32);
            let min = rect.min + egui::vec2(x * cell, y * cell);
            painter.rect_filled(Rect::from_min_size(min, egui::vec2(cell + 0.25, cell + 0.25)), 0., ink);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn switch_works_with_keyboard_and_ignores_disabled_input() {
        let ctx = egui::Context::default();
        let mut value = false;
        let key = || egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
        let mut draw = |events: Vec<egui::Event>, enabled: bool, focus: bool| {
            let mut changed = false;
            let out = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        let response = toggle(ui, &mut value, "Auto select");
                        if focus {
                            response.request_focus();
                        }
                        changed = response.changed();
                    });
                });
            });
            out.drop_without_applying_deltas();
            (value, changed)
        };
        draw(vec![], true, true);
        assert_eq!(draw(vec![key()], true, false), (true, true));
        draw(vec![], false, false);
        assert_eq!(draw(vec![key()], false, false), (true, false));
    }

    #[test]
    fn plural_and_age() {
        assert_eq!(plural(1, "server", "servers"), "1 server");
        assert_eq!(plural(3, "server", "servers"), "3 servers");
        assert_eq!(plural(0, "rule", "rules"), "0 rules");
    }
}

