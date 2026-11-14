use super::*;

pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(theme::bold(size))
}
/// Muted secondary text (counters, short notes). Does not wrap.
pub fn hint(ui: &mut Ui, text: impl Into<String>) -> Response {
    crate::emoji::text_label(
        ui,
        RichText::new(text)
            .size(theme::SMALL)
            .color(theme::palette(ui).muted),
        false,
        false,
    )
}
/// One line of help under a page header: what the page does, in plain words.
/// It wraps, so touch screens (no hover) read it too.
pub fn page_note(ui: &mut Ui, text: impl Into<String>) {
    let text = RichText::new(text)
        .size(theme::SMALL)
        .color(theme::palette(ui).muted);
    crate::emoji::text_label(ui, text, true, false);
    ui.add_space(theme::SPACE_M);
}
/// Small muted column caption.
pub fn caption(ui: &mut Ui, text: impl Into<String>) -> Response {
    crate::emoji::text_label(
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
    crate::emoji::text_label(
        ui,
        RichText::new(text).size(theme::SMALL).color(ink),
        true,
        false,
    )
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
