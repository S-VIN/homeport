use super::*;

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
    let response = select(ui, id, &value.to_string(), width, |ui| {
        for &option in options {
            ui.selectable_value(value, option, option.to_string());
        }
    });
    if !help.is_empty() { response.on_hover_text(help); }
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
