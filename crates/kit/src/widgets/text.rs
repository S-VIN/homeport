use super::*;

pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).font(theme::bold(size))
}
/// Muted secondary text (counters, short notes). Does not wrap.
pub fn hint(ui: &mut Ui, text: impl Into<String>) -> Response {
    crate::emoji::text_label(ui, RichText::new(text).size(theme::SMALL).color(theme::palette(ui).muted), false, false)
}
/// Small muted column caption.
pub fn caption(ui: &mut Ui, text: impl Into<String>) -> Response {
    crate::emoji::text_label(ui, RichText::new(text).size(theme::CAPTION).color(theme::palette(ui).muted), false, false)
}
/// Small text in a status tone (row subtitles, inline errors). Wraps.
pub fn tone_text(ui: &mut Ui, text: impl Into<String>, tone: Tone) -> Response {
    let ink = theme::palette(ui).tone(tone);
    crate::emoji::text_label(ui, RichText::new(text).size(theme::SMALL).color(ink), true, false)
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
