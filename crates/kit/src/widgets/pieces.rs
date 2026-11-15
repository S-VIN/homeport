use super::*;

/// Thin divider across the available width.
pub fn separator(ui: &mut Ui) {
    let p = theme::palette(ui);
    let r = ui.available_rect_before_wrap();
    ui.painter().line_segment(
        [
            pos2(r.left(), r.top() + 0.5),
            pos2(r.right(), r.top() + 0.5),
        ],
        Stroke::new(1_f32, p.border),
    );
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
/// In a row on a phone it is a colored dot with the text as a tooltip.
pub fn tag(ui: &mut Ui, text: &str, hue: usize, max_width: f32) -> Response {
    if compact_row(ui) {
        return tag_dot(ui, hue).on_hover_text(text);
    }
    let (ink, fill) = theme::palette(ui).tag(hue);
    pill(ui, text, ink, fill, max_width)
}
/// The width [`tag`] will take, for laying out the rest of a line.
pub fn tag_width(ui: &Ui, text: &str, max_width: f32) -> f32 {
    if compact_row(ui) {
        return 10.;
    }
    let width = ui
        .fonts_mut(|f| {
            f.layout_no_wrap(
                text.to_owned(),
                FontId::proportional(theme::CAPTION),
                Color32::PLACEHOLDER,
            )
        })
        .size()
        .x;
    (width + 12.).min(max_width)
}
/// The dot that stands for a tag color.
pub fn tag_dot(ui: &mut Ui, hue: usize) -> Response {
    let (ink, _) = theme::palette(ui).tag(hue);
    dot(ui, ink)
}
pub(crate) fn pill(
    ui: &mut Ui,
    text: &str,
    ink: Color32,
    fill: Color32,
    max_width: f32,
) -> Response {
    let font = FontId::proportional(theme::CAPTION);
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, ink);
    job.wrap = egui::text::TextWrapping::truncate_at_width((max_width - 12.).max(12.));
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let (rect, r) = ui.allocate_exact_size(vec2(galley.size().x + 12., 18.), Sense::hover());
    ui.painter().rect_filled(rect, 4, fill);
    let elided = galley.elided;
    let pos = rect.center() - galley.size() / 2.;
    ui.painter().galley(pos, galley.clone(), ink);
    if elided {
        r.on_hover_ui(|ui| {
            text_label(ui, text, true, false);
        })
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
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        FontId::proportional(theme::SMALL),
        p.foreground,
    );
    let lead_w = match lead {
        None => 0.,
        Some(Lead::Dot(_)) => 14.,
        Some(Lead::Icon(..)) => 19.,
    };
    job.wrap.max_width = (ui.available_width() - lead_w - 18.).max(0.);
    job.wrap.max_rows = 1;
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let width = (galley.size().x + lead_w + 18.).min(ui.available_width());
    let (rect, response) = ui.allocate_exact_size(vec2(width, 24.), Sense::click());
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(rect, 12, p.subtle);
    }
    let mut x = rect.left() + 9.;
    match lead {
        Some(Lead::Dot(c)) => {
            ui.painter()
                .circle_filled(pos2(x + 4., rect.center().y), 4., c);
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
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            12,
            Stroke::new(2_f32, p.accent),
            egui::StrokeKind::Inside,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, text));
    if !tooltip.is_empty() && (tooltip != text || galley.elided) {
        response.on_hover_ui(|ui| {
            text_label(ui, tooltip, true, false);
        })
    } else if galley.elided {
        response.on_hover_ui(|ui| {
            text_label(ui, text, true, false);
        })
    } else {
        response
    }
}
