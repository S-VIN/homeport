//! One editor for every ordered list a plugin registers with `Setup::list`:
//! add bar, a list card with column captions, inline edit (Enter/blur saves,
//! Esc discards), reorder, two-step delete, drafts that survive errors and
//! "Saving…/Saved" feedback. Row actions stay faint until the row is hovered.
use super::UiCx;
use crate::{
    kit::{self, theme, Emphasis, Icon, Tone},
    list::{Add, Move, Remove, Save},
    Id, Record, State,
};
use egui::{Color32, Ui};

type Lens<T> = fn(&mut T) -> &mut String;

pub struct Column<'s, T> {
    title: &'s str,
    /// `None` takes the remaining width.
    width: Option<f32>,
    /// Right-align the caption and content (numbers).
    right: bool,
    kind: Kind<'s, T>,
}
enum Kind<'s, T> {
    Text {
        text: Box<dyn Fn(&T) -> String + 's>,
        color: Option<Box<dyn Fn(&Ui, &T) -> Color32 + 's>>,
        tooltip: Option<Box<dyn Fn(&T) -> String + 's>>,
    },
    Edit {
        lens: Lens<T>,
        placeholder: &'s str,
        secret: bool,
        display: Option<Box<dyn Fn(&T) -> String + 's>>,
        color: Option<Box<dyn Fn(&Ui, &T) -> Color32 + 's>>,
    },
    Widget(Box<dyn Fn(&mut Ui, &mut T) -> bool + 's>),
    Custom(Box<dyn Fn(&mut Ui, &mut UiCx, &T) + 's>),
}
impl<'s, T: 's> Column<'s, T> {
    /// Read-only text.
    pub fn text(title: &'s str, text: impl Fn(&T) -> String + 's) -> Self {
        Self { title, width: None, right: false, kind: Kind::Text { text: Box::new(text), color: None, tooltip: None } }
    }
    /// Text shown normally and edited inline after the pencil.
    pub fn edit(title: &'s str, placeholder: &'s str, lens: Lens<T>) -> Self {
        Self { title, width: None, right: false, kind: Kind::Edit { lens, placeholder, secret: false, display: None, color: None } }
    }
    /// A control that saves immediately when it changes (choice, toggle…).
    pub fn widget(title: &'s str, width: f32, f: impl Fn(&mut Ui, &mut T) -> bool + 's) -> Self {
        Self { title, width: Some(width), right: false, kind: Kind::Widget(Box::new(f)) }
    }
    /// A combo box over a closed set; saves on change.
    pub fn choice<C: Copy + PartialEq + std::fmt::Display + 'static>(title: &'s str, width: f32, lens: fn(&mut T) -> &mut C, options: &'static [C]) -> Self {
        Self::widget(title, width, move |ui, item| kit::choice(ui, "choice", lens(item), options, width, title))
    }
    /// Anything else (read-only).
    pub fn custom(title: &'s str, width: f32, f: impl Fn(&mut Ui, &mut UiCx, &T) + 's) -> Self {
        Self { title, width: Some(width), right: false, kind: Kind::Custom(Box::new(f)) }
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    /// Right-aligned column (numbers).
    pub fn right(mut self) -> Self {
        self.right = true;
        self
    }
    pub fn secret(mut self) -> Self {
        if let Kind::Edit { secret, .. } = &mut self.kind {
            *secret = true;
        }
        self
    }
    /// What the cell shows when not editing (default: the edited text).
    pub fn display(mut self, f: impl Fn(&T) -> String + 's) -> Self {
        match &mut self.kind {
            Kind::Edit { display, .. } => *display = Some(Box::new(f)),
            Kind::Text { text, .. } => *text = Box::new(f),
            _ => {}
        }
        self
    }
    pub fn color(mut self, f: impl Fn(&Ui, &T) -> Color32 + 's) -> Self {
        match &mut self.kind {
            Kind::Edit { color, .. } | Kind::Text { color, .. } => *color = Some(Box::new(f)),
            _ => {}
        }
        self
    }
    pub fn tooltip(mut self, f: impl Fn(&T) -> String + 's) -> Self {
        if let Kind::Text { tooltip, .. } = &mut self.kind {
            *tooltip = Some(Box::new(f));
        }
        self
    }
}

type Fields<'s, T> = Box<dyn FnMut(&mut Ui, &mut T, f32) -> bool + 's>;
type Subtitle<'s, T> = Box<dyn Fn(&[T], usize) -> Option<(String, Tone)> + 's>;
type Leading<'s, T> = (f32, Box<dyn Fn(&mut Ui, &mut UiCx, &T) + 's>);

pub struct ListEditor<'s, T: Record> {
    state: &'static State<Vec<T>>,
    columns: Vec<Column<'s, T>>,
    add: Option<(&'s str, Box<dyn Fn() -> T + 's>, Fields<'s, T>)>,
    ordered: bool,
    empty: Option<(Icon, &'s str, &'s str)>,
    subtitle: Option<Subtitle<'s, T>>,
    leading: Option<Leading<'s, T>>,
    below: Option<Box<dyn Fn(&mut Ui, &mut UiCx, &T) + 's>>,
    headers: bool,
}

impl<'s, T: Record> ListEditor<'s, T> {
    pub fn new(state: &'static State<Vec<T>>) -> Self {
        Self { state, columns: Vec::new(), add: None, ordered: true, empty: None, subtitle: None, leading: None, below: None, headers: true }
    }
    pub fn column(mut self, c: Column<'s, T>) -> Self {
        self.columns.push(c);
        self
    }
    /// Add bar above the list. `fields` draws inputs for the draft into the given
    /// width and returns true on Enter.
    pub fn add_form(mut self, label: &'s str, new: impl Fn() -> T + 's, fields: impl FnMut(&mut Ui, &mut T, f32) -> bool + 's) -> Self {
        self.add = Some((label, Box::new(new), Box::new(fields)));
        self
    }
    /// Hide the reorder arrows (order still persists).
    pub fn unordered(mut self) -> Self {
        self.ordered = false;
        self
    }
    pub fn no_headers(mut self) -> Self {
        self.headers = false;
        self
    }
    /// Empty state: icon, title and the next step.
    pub fn empty(mut self, icon: Icon, title: &'s str, hint: &'s str) -> Self {
        self.empty = Some((icon, title, hint));
        self
    }
    /// A second line under the first column: status, errors, warnings.
    pub fn subtitle(mut self, f: impl Fn(&[T], usize) -> Option<(String, Tone)> + 's) -> Self {
        self.subtitle = Some(Box::new(f));
        self
    }
    /// A fixed-width marker before the first column (e.g. a color dot).
    pub fn leading(mut self, width: f32, f: impl Fn(&mut Ui, &mut UiCx, &T) + 's) -> Self {
        self.leading = Some((width, Box::new(f)));
        self
    }
    /// Extra content under a row (details).
    pub fn below(mut self, f: impl Fn(&mut Ui, &mut UiCx, &T) + 's) -> Self {
        self.below = Some(Box::new(f));
        self
    }

    pub fn show(mut self, ui: &mut Ui, cx: &mut UiCx) {
        let list = egui::Id::unique(("list", self.state.name));
        let rows = cx.get(self.state);
        let editing_id = list.with("editing");
        let mut editing: Option<Id> = ui.data(|d| d.get_temp(editing_id)).flatten();
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            editing = None;
        }
        if let Some((label, new, fields)) = &mut self.add {
            let draft_id = list.with("new");
            let mut draft: T = ui.data(|d| d.get_temp(draft_id)).unwrap_or_else(&**new);
            let key = (self.state.name, "add");
            let busy = cx.pending(key);
            match cx.outcome(key) {
                Some(Ok(())) => draft = new(),
                Some(Err(e)) => ui.data_mut(|d| {
                    d.insert_temp(list.with("add-error"), e);
                }),
                None => {}
            }
            if kit::add_bar(ui, label, !busy, |ui, width| fields(ui, &mut draft, width)) {
                ui.data_mut(|d| d.remove::<String>(list.with("add-error")));
                cx.submit(key, Add::new(draft.clone()));
            }
            if let Some(e) = ui.data(|d| d.get_temp::<String>(list.with("add-error"))) {
                kit::tone_text(ui, e, Tone::Danger);
                ui.add_space(theme::SPACE_M);
            }
            ui.data_mut(|d| d.insert_temp(draft_id, draft));
        }
        let editable = self.columns.iter().any(|c| matches!(c.kind, Kind::Edit { .. }));
        let gap = theme::HORIZONTAL_GAP;
        // Row actions sit edge to edge (no gaps between them).
        let actions = if self.ordered { 2. * theme::CONTROL } else { 0. } + if editable { theme::CONTROL } else { 0. } + theme::CONTROL;
        let leading = self.leading.as_ref().map_or(0., |(w, _)| w + gap);
        let fixed: f32 = self.columns.iter().filter_map(|c| c.width).map(|w| w + gap).sum();
        let fills = self.columns.iter().filter(|c| c.width.is_none()).count().max(1) as f32;
        let fill = |ui: &Ui| ((ui.available_width() - leading - actions - fixed - gap) / fills).max(40.);
        kit::list_card(ui, |ui| {
            if self.headers && !rows.is_empty() && self.columns.iter().any(|c| !c.title.is_empty()) {
                kit::list_header(ui, |ui| {
                    // The header has no row inset: match the row's content start.
                    let w = fill(ui) - 0.;
                    if leading > 0. {
                        ui.add_space(leading - gap);
                    }
                    for c in &self.columns {
                        let width = c.width.unwrap_or(w);
                        if c.right {
                            kit::cell_right(ui, width, |ui| kit::caption(ui, c.title));
                        } else {
                            kit::cell(ui, width, |ui| kit::caption(ui, c.title));
                        }
                    }
                });
            }
            if rows.is_empty() {
                if let Some((icon, title, hint)) = self.empty {
                    kit::empty(ui, icon, title, hint);
                }
                return;
            }
            let p = theme::palette(ui);
            egui::ScrollArea::vertical().id_salt(("list-rows", self.state.name)).auto_shrink([false, true]).show(ui, |ui| {
                for (index, item) in rows.iter().enumerate() {
                    let id = item.id();
                    let key = (self.state.name, id);
                    let draft_id = list.with(("draft", id));
                    let is_editing = editing == Some(id);
                    let mut draft: T = if is_editing { ui.data(|d| d.get_temp(draft_id)).unwrap_or_else(|| item.clone()) } else { item.clone() };
                    let error_id = list.with(("error", id));
                    match cx.outcome(key) {
                        Some(Ok(())) => {
                            ui.data_mut(|d| d.remove::<String>(error_id));
                            if ui.data(|d| d.get_temp::<bool>(list.with(("close", id))).unwrap_or(false)) {
                                editing = None;
                                ui.data_mut(|d| d.remove::<bool>(list.with(("close", id))));
                            }
                            draft = item.clone();
                        }
                        Some(Err(e)) => ui.data_mut(|d| {
                            d.insert_temp(error_id, e);
                        }),
                        None => {}
                    }
                    let busy = cx.pending(key);
                    let subtitle = self.subtitle.as_ref().and_then(|f| f(&rows, index));
                    let mut save = false;
                    ui.push_id(id, |ui| {
                        kit::row(ui, id, false, |ui, row| {
                            let emphasis = if row.hovered || is_editing { Emphasis::Normal } else { Emphasis::Quiet };
                            let two_lines = subtitle.is_some() && !is_editing;
                            // The second line lives in the first column; everything else centers on both lines.
                            let second = |ui: &mut Ui, width: f32, first: &dyn Fn(&mut Ui)| {
                                let (text, tone) = subtitle.as_ref().expect("two-line row");
                                ui.allocate_ui_with_layout(egui::vec2(width, 40.), egui::Layout::top_down(egui::Align::Min), |ui| {
                                    ui.set_width(width);
                                    ui.spacing_mut().item_spacing.y = 2.;
                                    ui.add_space(3.);
                                    first(ui);
                                    ui.add(egui::Label::new(egui::RichText::new(text).size(theme::SMALL).color(p.tone(*tone))).truncate()).on_hover_text(text);
                                });
                            };
                            ui.add_enabled_ui(!busy, |ui| {
                                kit::row_line(ui, if two_lines { 44. } else { theme::ROW }, |ui| {
                                    let w = fill(ui);
                                    if let Some((width, f)) = &self.leading {
                                        if two_lines {
                                            // Align the marker with the first line.
                                            ui.allocate_ui_with_layout(egui::vec2(*width, 40.), egui::Layout::top_down(egui::Align::Min), |ui| {
                                                ui.add_space(8.);
                                                f(ui, cx, item);
                                            });
                                        } else {
                                            kit::cell(ui, *width, |ui| f(ui, cx, item));
                                        }
                                    }
                                    for (ci, c) in self.columns.iter().enumerate() {
                                        let width = c.width.unwrap_or(w);
                                        let first_line = ci == 0 && two_lines;
                                        match &c.kind {
                                            Kind::Text { text, color, tooltip } => {
                                                let draw = |ui: &mut Ui| {
                                                    let mut t = egui::RichText::new(text(item));
                                                    if let Some(color) = color {
                                                        t = t.color(color(ui, item));
                                                    }
                                                    let r = ui.add(egui::Label::new(t).truncate());
                                                    if let Some(tip) = tooltip {
                                                        r.on_hover_text(tip(item));
                                                    }
                                                };
                                                if first_line {
                                                    second(ui, width, &draw)
                                                } else if c.right {
                                                    kit::cell_right(ui, width, draw)
                                                } else {
                                                    kit::cell(ui, width, draw)
                                                }
                                            }
                                            Kind::Edit { lens, placeholder, secret, display, color } => {
                                                if is_editing {
                                                    let r = kit::input(ui, c.title, lens(&mut draft), placeholder, width, *secret)
                                                        .on_hover_text("Enter or leave the field to save · Esc discards unsaved text");
                                                    let focus = list.with(("focus", id));
                                                    if ui.data(|d| d.get_temp::<bool>(focus).unwrap_or(false)) {
                                                        r.request_focus();
                                                        ui.data_mut(|d| d.remove::<bool>(focus));
                                                    }
                                                    save |= kit::committed(ui, &r);
                                                } else {
                                                    let draw = |ui: &mut Ui| {
                                                        let shown = display.as_ref().map_or_else(|| lens(&mut item.clone()).clone(), |f| f(item));
                                                        let mut t = egui::RichText::new(&shown).color(p.foreground);
                                                        if let Some(color) = color {
                                                            t = t.color(color(ui, item));
                                                        }
                                                        ui.add(egui::Label::new(t).truncate()).on_hover_text(shown);
                                                    };
                                                    if first_line {
                                                        second(ui, width, &draw)
                                                    } else {
                                                        kit::cell(ui, width, draw)
                                                    }
                                                }
                                            }
                                            Kind::Widget(f) => {
                                                // Applied to the stored item at once, independent of the text draft.
                                                let mut fresh = item.clone();
                                                if kit::cell(ui, width, |ui| f(ui, &mut fresh)) && fresh != *item {
                                                    cx.submit(key, Save(fresh));
                                                }
                                            }
                                            Kind::Custom(f) => {
                                                if c.right {
                                                    kit::cell_right(ui, width, |ui| f(ui, cx, item))
                                                } else {
                                                    kit::cell(ui, width, |ui| f(ui, cx, item))
                                                }
                                            }
                                        }
                                    }
                                    if self.ordered {
                                        if let Some(up) = kit::order(ui, index, rows.len(), emphasis) {
                                            cx.submit(key, Move::<T>::new(id, up));
                                        }
                                    }
                                    ui.spacing_mut().item_spacing.x = 0.;
                                    if editable {
                                        let label = if is_editing { "Close editor" } else { "Edit" };
                                        if kit::icon_button_ex(ui, if is_editing { Icon::Check } else { Icon::Edit }, label, true, emphasis, false).clicked() {
                                            if is_editing {
                                                if &draft != item {
                                                    save = true;
                                                    ui.data_mut(|d| d.insert_temp(list.with(("close", id)), true));
                                                } else {
                                                    editing = None;
                                                }
                                            } else {
                                                editing = Some(id);
                                                draft = item.clone();
                                                ui.data_mut(|d| d.insert_temp(list.with(("focus", id)), true));
                                            }
                                        }
                                    }
                                    if kit::remove(ui, emphasis) {
                                        cx.submit(key, Remove::<T>::new(id));
                                    }
                                });
                            });
                            if let Some(e) = ui.data(|d| d.get_temp::<String>(error_id)) {
                                ui.horizontal(|ui| {
                                    ui.add_space(leading);
                                    kit::tone_text(ui, e, Tone::Danger);
                                });
                                ui.add_space(4.);
                            }
                            if let Some(below) = &self.below {
                                below(ui, cx, item);
                            }
                        });
                    });
                    if save {
                        // Only the edited text fields replace the stored values; fields that
                        // changed meanwhile (status, timestamps) are kept.
                        let mut merged = item.clone();
                        for c in &self.columns {
                            if let Kind::Edit { lens, .. } = &c.kind {
                                let value = lens(&mut draft).clone();
                                *lens(&mut merged) = value;
                            }
                        }
                        if merged != *item {
                            cx.submit(key, Save(merged));
                        } else {
                            editing = editing.filter(|_| !ui.data(|d| d.get_temp::<bool>(list.with(("close", id))).unwrap_or(false)));
                        }
                    }
                    if editing == Some(id) {
                        ui.data_mut(|d| d.insert_temp(draft_id, draft));
                    } else {
                        ui.data_mut(|d| d.remove::<T>(draft_id));
                    }
                }
            });
        });
        ui.data_mut(|d| d.insert_temp(editing_id, editing));
    }
}
