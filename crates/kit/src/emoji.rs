//! egui lays out the text. A local Noto atlas supplies the color glyphs it cannot shape.
use egui::{
    self, text::LayoutJob, Color32, FontId, Galley, Label, Painter, Rect, Response, Sense,
    TextFormat, Ui,
};
use std::{collections::HashMap, sync::LazyLock};
use unicode_segmentation::UnicodeSegmentation;

const SEQUENCES: &str = include_str!("../../../assets/emoji/sequences.txt");
const ATLAS: &[u8] = include_bytes!("../../../assets/emoji/atlas.png");
const SPACE: char = '\u{2003}';
static INDEX: LazyLock<HashMap<String, u16>> = LazyLock::new(|| {
    let mut map = HashMap::new();
    for (i, emoji) in SEQUENCES.lines().enumerate() {
        map.insert(emoji.to_owned(), i as u16);
        if emoji.contains('\u{fe0f}') {
            map.insert(emoji.replace('\u{fe0f}', ""), i as u16);
        }
    }
    map
});

fn tile(grapheme: &str) -> Option<u16> {
    if grapheme.is_ascii() {
        return None;
    }
    INDEX.get(grapheme).copied().or_else(|| {
        grapheme
            .contains('\u{fe0f}')
            .then(|| INDEX.get(&grapheme.replace('\u{fe0f}', "")).copied())
            .flatten()
    })
}

/// The map goes from the char index of each placeholder in `job.text` to its atlas tile.
pub fn layout(raw: &str, font: FontId, color: Color32) -> (LayoutJob, HashMap<u32, u16>) {
    let normal = TextFormat::simple(font, color);
    let hidden = TextFormat {
        color: Color32::TRANSPARENT,
        ..normal.clone()
    };
    let mut job = LayoutJob::default();
    let mut icons = HashMap::new();
    let mut start = 0;
    for (i, cluster) in raw.grapheme_indices(true) {
        let Some(index) = tile(cluster) else {
            continue;
        };
        if i > start {
            job.append(&raw[start..i], 0., normal.clone());
        }
        icons.insert(job.text.chars().count() as u32, index);
        // One placeholder has width. Remaining scalars retain their cursor indices in TextEdit.
        let mut placeholder = SPACE.to_string();
        placeholder.extend(cluster.chars().skip(1).map(|_| '\u{200b}'));
        job.append(&placeholder, 0., hidden.clone());
        start = i + cluster.len();
    }
    if start < raw.len() || job.sections.is_empty() {
        job.append(&raw[start..], 0., normal);
    }
    (job, icons)
}

/// Draw the color glyphs of a galley laid out with [`layout`].
pub fn paint(ui: &Ui, painter: &Painter, pos: egui::Pos2, galley: &Galley, icons: &HashMap<u32, u16>) {
    if icons.is_empty() {
        return;
    }
    let key = egui::Id::unique("rproxy-emoji-atlas");
    let tex = ui
        .ctx()
        .data_mut(|m| m.get_temp::<egui::TextureHandle>(key))
        .unwrap_or_else(|| {
            let img = image::load_from_memory(ATLAS)
                .expect("bundled emoji atlas")
                .into_rgba8();
            let tex = ui.ctx().load_texture(
                "Noto Color Emoji",
                egui::ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                ),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx().data_mut(|m| m.insert_temp(key, tex.clone()));
            tex
        });
    let atlas = tex.size_vec2();
    // Rows hold one glyph per char; a newline ends a row without a glyph.
    let mut index = 0u32;
    let glyphs = galley.rows.iter().flat_map(|row| {
        let start = index;
        index += row.glyphs.len() as u32 + row.ends_with_newline as u32;
        row.glyphs.iter().enumerate().map(move |(i, g)| (start + i as u32, row.pos, g))
    });
    for (index, row_pos, glyph) in glyphs.filter(|(_, _, g)| g.chr == SPACE) {
        if let Some(&tile) = icons.get(&index) {
            let tile = tile as usize;
            let origin = egui::vec2((tile % 64 * 32) as f32, (tile / 64 * 32) as f32);
            let uv = Rect::from_min_max(
                (origin / atlas).to_pos2(),
                ((origin + egui::Vec2::splat(32.)) / atlas).to_pos2(),
            );
            let r = glyph.logical_rect().translate(pos.to_vec2() + row_pos.to_vec2());
            let size = r.width().min(r.height());
            painter.image(
                tex.id(),
                Rect::from_center_size(r.center(), egui::Vec2::splat(size)),
                uv,
                Color32::WHITE,
            );
        }
    }
}

pub fn label(ui: &mut Ui, text: &str, font: FontId, clickable: bool) -> Response {
    let (response, elided) = text_label_with_elision(ui, egui::RichText::new(text).font(font), !clickable, clickable);
    if elided {
        response.on_hover_ui(|ui| { text_label(ui, text, true, false); })
    } else {
        response
    }
}

/// Shared text renderer, preserving rich text styles and the original accessible label.
pub fn text_label(ui: &mut Ui, text: impl Into<egui::WidgetText>, wrap: bool, clickable: bool) -> Response {
    text_label_with_elision(ui, text, wrap, clickable).0
}

/// Also reports whether layout actually hid part of the text.
pub fn text_label_with_elision(ui: &mut Ui, text: impl Into<egui::WidgetText>, wrap: bool, clickable: bool) -> (Response, bool) {
    let text = text.into();
    let original = text.text().to_owned();
    let color = ui.visuals().text_color();
    let source = text.into_layout_job(ui.style(), egui::FontSelection::Default, egui::Align::Center);
    let mut job = LayoutJob::default();
    let mut icons = HashMap::new();
    for section in &source.sections {
        let raw = &source.text[section.byte_range.start.0..section.byte_range.end.0];
        let (part, map) = layout(raw, section.format.font_id.clone(), section.format.color);
        let offset = job.text.chars().count() as u32;
        icons.extend(map.into_iter().map(|(i, tile)| (i + offset, tile)));
        for run in part.sections {
            let mut format = section.format.clone();
            format.color = run.format.color;
            job.append(&part.text[run.byte_range.start.0..run.byte_range.end.0], 0., format);
        }
    }
    job.wrap = source.wrap.clone();
    let mut label = Label::new(job).selectable(false).show_tooltip_when_elided(false);
    label = if wrap { label.wrap() } else { label.truncate() };
    if clickable {
        label = label.sense(Sense::click());
    }
    let (pos, galley, response) = label.layout_in_ui(ui);
    if ui.is_rect_visible(response.rect) {
        ui.painter().galley(pos, galley.clone(), color);
        paint(ui, ui.painter(), pos, &galley, &icons);
    }
    if clickable && response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(1.),
            crate::theme::RADIUS_CONTROL,
            egui::Stroke::new(2_f32, crate::theme::palette(ui).accent),
            egui::StrokeKind::Outside,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            if clickable {
                egui::Role::Button
            } else {
                egui::Role::Label
            },
            ui.is_enabled(),
            &original,
        )
    });
    (response, galley.elided)
}

pub fn edit(ui: &mut Ui, text: &mut String, width: f32, hint: &str) -> Response {
    let mut icons = HashMap::new();
    let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, _width: f32| {
        let (job, map) = layout(
            text.as_str(),
            egui::TextStyle::Body.resolve(ui.style()),
            ui.visuals().text_color(),
        );
        icons = map;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let o = egui::TextEdit::singleline(text)
        .hint_text(hint)
        .desired_width(width)
        .min_size(egui::vec2(0., crate::theme::CONTROL))
        .margin(egui::Margin::symmetric(8, 5))
        .layouter(&mut layouter)
        .show(ui);
    paint(
        ui,
        &ui.painter_at(o.text_clip_rect),
        o.galley_pos,
        &o.galley,
        &icons,
    );
    o.response.response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compound_emoji_are_single_tiles_with_original_editor_indices() {
        let raw = "A 🇦🇱⚡️🚀 👩🏽‍💻 1️⃣ ♥︎ Z";
        let (job, icons) = layout(raw, FontId::proportional(16.), Color32::WHITE);
        assert_eq!(icons.len(), 5);
        assert_eq!(raw.chars().count(), job.text.chars().count());
        assert!(job.text.contains("♥︎"));
        assert_ne!(tile("🇦🇱"), tile("🇦🇷"));
        assert_eq!(tile("☁"), tile("☁️"));
    }
}
