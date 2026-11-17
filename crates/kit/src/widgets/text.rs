use super::*;

pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(theme::bold(size))
}
/// Muted secondary text (counters, short notes). Does not wrap.
pub fn hint(ui: &mut Ui, text: impl Into<String>) -> Response {
    text_label(
        ui,
        RichText::new(text)
            .size(theme::SMALL)
            .color(theme::palette(ui).muted),
        false,
        false,
    )
}
/// Small muted column caption.
pub fn caption(ui: &mut Ui, text: impl Into<String>) -> Response {
    text_label(
        ui,
        RichText::new(text)
            .size(theme::CAPTION)
            .color(theme::palette(ui).muted),
        false,
        false,
    )
}
/// Small text in a status tone (row subtitles, inline errors). Wraps.
pub fn tone_text(ui: &mut Ui, text: impl Into<String>, tone: Tone) -> Response {
    let ink = theme::palette(ui).tone(tone);
    text_label(
        ui,
        RichText::new(text).size(theme::SMALL).color(ink),
        true,
        false,
    )
}
/// A name (server, subscription) on one line; when cut, hovering shows it whole.
pub fn label(ui: &mut Ui, text: &str, font: FontId, clickable: bool) -> Response {
    let (response, elided) =
        text_label_with_elision(ui, RichText::new(text).font(font), !clickable, clickable);
    if elided {
        response.on_hover_ui(|ui| {
            text_label(ui, text, true, false);
        })
    } else {
        response
    }
}
/// Shared text renderer, preserving rich text styles and the original accessible label.
pub fn text_label(
    ui: &mut Ui,
    text: impl Into<egui::WidgetText>,
    wrap: bool,
    clickable: bool,
) -> Response {
    text_label_with_elision(ui, text, wrap, clickable).0
}
/// Also reports whether layout actually hid part of the text.
pub fn text_label_with_elision(
    ui: &mut Ui,
    text: impl Into<egui::WidgetText>,
    wrap: bool,
    clickable: bool,
) -> (Response, bool) {
    let text = text.into();
    let original = text.text().to_owned();
    let job = text.into_layout_job(ui.style(), egui::FontSelection::Default, Align::Center);
    let mut label = egui::Label::new(job)
        .selectable(false)
        .show_tooltip_when_elided(false);
    label = if wrap { label.wrap() } else { label.truncate() };
    if clickable {
        label = label.sense(Sense::click());
    }
    let (pos, galley, response) = label.layout_in_ui(ui);
    if ui.is_rect_visible(response.rect) {
        ui.painter()
            .galley(pos, galley.clone(), ui.visuals().text_color());
    }
    if clickable && response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(1.),
            RADIUS_CONTROL,
            Stroke::new(2_f32, theme::palette(ui).accent),
            egui::StrokeKind::Outside,
        );
    }
    response.widget_info(|| {
        let role = if clickable {
            egui::Role::Button
        } else {
            egui::Role::Label
        };
        egui::WidgetInfo::labeled(role, ui.is_enabled(), &original)
    });
    (response, galley.elided)
}
/// `1 server`, `2 servers`.
/// `1.2 MB`, `640 KB`, `12 B` (decimal units, like file managers).
/// Sizes in steps of 1024, like providers' plans and other proxy apps
/// ("100 GB" is 100 × 1024³ bytes).
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024. && unit < UNITS.len() - 1 {
        v /= 1024.;
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
/// A page counter: `plural`, or nothing when there are none (the empty
/// state says so already).
pub fn count(n: usize, one: &str, many: &str) -> String {
    if n == 0 {
        String::new()
    } else {
        plural(n, one, many)
    }
}
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
