use super::*;

/// Single-line text input.
pub fn input(
    ui: &mut Ui,
    id: impl egui::AsIdSalt,
    value: &mut String,
    placeholder: &str,
    width: f32,
) -> Response {
    text_input(ui, id, value, placeholder, width, false)
}
/// [`input`] that hides what is typed.
pub fn password(
    ui: &mut Ui,
    id: impl egui::AsIdSalt,
    value: &mut String,
    placeholder: &str,
    width: f32,
) -> Response {
    text_input(ui, id, value, placeholder, width, true)
}
fn text_input(
    ui: &mut Ui,
    id: impl egui::AsIdSalt,
    value: &mut String,
    placeholder: &str,
    width: f32,
    secret: bool,
) -> Response {
    let width = width * super::settings::stretch(ui);
    ui.push_id(id, |ui| {
        ui.add(
            egui::TextEdit::singleline(value)
                .desired_width(width.max(24.))
                // Keep the asked width when the row is short of room: a layout
                // that measures its controls must see their real size.
                .min_size(vec2(width.max(24.), CONTROL))
                .hint_text(placeholder)
                .password(secret)
                .margin(egui::Margin::symmetric(8, 5)),
        )
    })
    .inner
}

/// Combo box over a closed set of values. Returns true when changed.
pub fn choice<T: Copy + Display + PartialEq>(
    ui: &mut Ui,
    id: impl egui::AsIdSalt,
    value: &mut T,
    options: &[T],
    width: f32,
    help: &str,
) -> bool {
    let before = *value;
    let response = select(ui, id, &value.to_string(), width, |ui| {
        for &option in options {
            ui.selectable_value(value, option, option.to_string());
        }
    });
    if !help.is_empty() {
        response.on_hover_text(help);
    }
    *value != before
}
/// Combo box with a fixed width; `menu` adds the options.
pub fn select(
    ui: &mut Ui,
    id: impl egui::AsIdSalt,
    selected: &str,
    width: f32,
    menu: impl FnOnce(&mut Ui),
) -> Response {
    ui.allocate_ui_with_layout(
        vec2(width, CONTROL),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.spacing_mut().button_padding = vec2(8., 4.);
            egui::ComboBox::from_id_salt(id)
                .width(width)
                .height(320.)
                .truncate()
                .icon(|ui, rect, _, open| {
                    let ink = theme::palette(ui).muted;
                    crate::icons::paint_sized(
                        ui.painter(),
                        rect.center(),
                        if open {
                            Icon::ChevronUp
                        } else {
                            Icon::ChevronDown
                        },
                        12.,
                        ink,
                    );
                })
                .selected_text(selected)
                .show_ui(ui, |ui| {
                    ui.set_min_width(width - 8.);
                    menu(ui)
                })
                .response
        },
    )
    .inner
}

/// A single-line TextEdit surrenders focus on Enter in the same frame.
pub fn submitted(ui: &Ui, response: &Response) -> bool {
    ui.is_enabled()
        && (response.has_focus() || response.lost_focus())
        && ui.input(|i| i.key_pressed(egui::Key::Enter))
}
/// Enter or blur: the moment an inline edit is saved.
/// A text field bound to a saved value: edits stay in a draft (kept across
/// frames) and the new text is returned once, on Enter or when focus leaves.
/// Escape restores the saved value.
pub fn field(
    ui: &mut Ui,
    id: impl egui::AsIdSalt,
    saved: &str,
    placeholder: &str,
    width: f32,
) -> Option<String> {
    let key = egui::Id::unique(("kit-field", &id));
    let mut draft: String = ui
        .data(|d| d.get_temp(key))
        .unwrap_or_else(|| saved.to_owned());
    let r = input(ui, id, &mut draft, placeholder, width);
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

/// One row of form fields sharing a width: text inputs split what the fixed
/// controls leave, a choice is as wide as its longest option. No pixel math
/// in plugins:
/// `kit::Fields::new(width).input("pattern", &mut r.pattern, "Domain").choice("route", &mut r.action, Route::ALL).show(ui)`
#[must_use]
pub struct Fields<'a> {
    width: f32,
    items: Vec<Field<'a>>,
}
type Draw<'a> = Box<dyn FnOnce(&mut Ui, f32) + 'a>;
enum Field<'a> {
    Input {
        id: &'static str,
        value: &'a mut String,
        placeholder: &'a str,
        help: &'a str,
    },
    /// Sized to fit `text` (e.g. the longest option).
    Fixed { text: String, draw: Draw<'a> },
}
impl<'a> Fields<'a> {
    pub fn new(width: f32) -> Self {
        Self {
            width,
            items: Vec::new(),
        }
    }
    /// A text input; `help` (if any) is its tooltip.
    pub fn input(mut self, id: &'static str, value: &'a mut String, placeholder: &'a str) -> Self {
        self.items.push(Field::Input {
            id,
            value,
            placeholder,
            help: "",
        });
        self
    }
    /// Tooltip of the last input.
    pub fn help(mut self, text: &'a str) -> Self {
        if let Some(Field::Input { help, .. }) = self.items.last_mut() {
            *help = text;
        }
        self
    }
    /// A combo box over `options`.
    pub fn choice<T: Copy + Display + PartialEq>(
        mut self,
        id: &'static str,
        value: &'a mut T,
        options: &'a [T],
    ) -> Self {
        let text = options
            .iter()
            .map(|o| o.to_string())
            .max_by_key(|t| t.chars().count())
            .unwrap_or_default();
        let draw = move |ui: &mut Ui, width: f32| {
            choice(ui, id, value, options, width, "");
        };
        self.items.push(Field::Fixed {
            text,
            draw: Box::new(draw),
        });
        self
    }
    /// A combo box over `(value, label)` pairs that sets a text value;
    /// `placeholder` shows while the value is none of them.
    pub fn options(
        mut self,
        id: &'static str,
        value: &'a mut String,
        options: Vec<(String, String)>,
        placeholder: &'a str,
    ) -> Self {
        let text = options
            .iter()
            .map(|(_, l)| l.clone())
            .chain([placeholder.to_owned()])
            .max_by_key(|t| t.chars().count())
            .unwrap_or_default();
        let draw = move |ui: &mut Ui, width: f32| {
            let shown = options
                .iter()
                .find(|(v, _)| v == value)
                .map_or(placeholder, |(_, l)| l.as_str());
            select(ui, id, shown, width, |ui| {
                for (v, label) in &options {
                    if ui.selectable_label(value == v, label).clicked() {
                        *value = v.clone();
                    }
                }
            });
        };
        self.items.push(Field::Fixed {
            text,
            draw: Box::new(draw),
        });
        self
    }
    /// Draw the row; true when Enter was pressed in an input.
    pub fn show(self, ui: &mut Ui) -> bool {
        let gap = theme::HORIZONTAL_GAP;
        // A fixed control: its text, the arrow and padding.
        let painter = ui.painter().clone();
        let fit = move |text: &str| {
            let font = FontId::proportional(theme::BODY);
            (painter
                .layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER)
                .size()
                .x
                + 44.)
                .clamp(90., 240.)
        };
        let natural: f32 = self
            .items
            .iter()
            .map(|f| {
                if let Field::Fixed { text, .. } = f {
                    fit(text) + gap
                } else {
                    0.
                }
            })
            .sum();
        let inputs = self
            .items
            .iter()
            .filter(|f| matches!(f, Field::Input { .. }))
            .count() as f32;
        // Too narrow (a phone): one field per line, full width, in the same order.
        let stacked = inputs > 0. && natural > 0. && self.width - natural < inputs * (160. + gap);
        // Choices give up width (down to their share) before inputs get narrower than 80.
        let room = self.width - inputs * (80. + gap);
        let scale = if natural > room && natural > 0. {
            (room / natural).max(0.5)
        } else {
            1.
        };
        let fixed = natural * scale;
        // `width` ends where the next control starts after one more gap, so
        // the fields together leave out the last gap only.
        let rest = if stacked {
            self.width
        } else if inputs > 0. {
            ((self.width - fixed + gap) / inputs - gap).max(60.)
        } else {
            0.
        };
        let width = self.width;
        let fit = move |text: &str| {
            if stacked {
                width
            } else {
                (fit(text) + gap) * scale - gap
            }
        };
        let mut submitted = false;
        let mut draw = |ui: &mut Ui, field: Field<'a>| match field {
            Field::Input {
                id,
                value,
                placeholder,
                help,
            } => {
                let r = input(ui, id, value, placeholder, rest);
                let r = if help.is_empty() {
                    r
                } else {
                    r.on_hover_text(help)
                };
                submitted |= self::submitted(ui, &r);
            }
            Field::Fixed { text, draw } => draw(ui, fit(&text)),
        };
        if stacked {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = theme::SPACE_S;
                self.items.into_iter().for_each(|f| draw(ui, f));
            });
        } else {
            self.items.into_iter().for_each(|f| draw(ui, f));
        }
        submitted
    }
}
