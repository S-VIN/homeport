//! One editor for every ordered list a plugin registers with `Setup::list`:
//! add bar, a list card with column captions, inline edit (Enter/blur saves,
//! Esc discards), reorder, delete, drafts that survive errors and
//! "Saving…/Saved" feedback. Row actions stay faint until the row is hovered.
use super::UiCx;
use crate::{
    Id, Record, State,
    columns::Columns,
    kit::{self, Emphasis, Icon, Tone, theme},
    list::{Add, Edit, Move, Remove, Save},
};
use egui::{Color32, Ui};

type Lens<T> = fn(&mut T) -> &mut String;

/// A column of a [`ListEditor`] row: one field of the item.
pub struct Field<'s, T> {
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
    },
    Edit {
        lens: Lens<T>,
        placeholder: &'s str,
        display: Option<Box<dyn Fn(&T) -> String + 's>>,
        color: Option<Box<dyn Fn(&Ui, &T) -> Color32 + 's>>,
    },
    Widget(Box<dyn Fn(&mut Ui, &mut T) -> bool + 's>),
    Custom(Box<dyn Fn(&mut Ui, &mut UiCx, &T) + 's>),
}
impl<'s, T: 's> Field<'s, T> {
    /// Read-only text.
    pub fn text(title: &'s str, text: impl Fn(&T) -> String + 's) -> Self {
        Self {
            title,
            width: None,
            right: false,
            kind: Kind::Text {
                text: Box::new(text),
                color: None,
            },
        }
    }
    /// Text shown normally and edited inline after the pencil.
    pub fn edit(title: &'s str, placeholder: &'s str, lens: Lens<T>) -> Self {
        Self {
            title,
            width: None,
            right: false,
            kind: Kind::Edit {
                lens,
                placeholder,
                display: None,
                color: None,
            },
        }
    }
    /// A control that saves immediately when it changes (choice, toggle…).
    pub fn widget(title: &'s str, width: f32, f: impl Fn(&mut Ui, &mut T) -> bool + 's) -> Self {
        Self {
            title,
            width: Some(width),
            right: false,
            kind: Kind::Widget(Box::new(f)),
        }
    }
    /// A combo box over a closed set; saves on change.
    pub fn choice<C: Copy + PartialEq + std::fmt::Display + 'static>(
        title: &'s str,
        width: f32,
        lens: fn(&mut T) -> &mut C,
        options: &'static [C],
    ) -> Self {
        Self::widget(title, width, move |ui, item| {
            kit::choice(
                ui,
                "choice",
                lens(item),
                options,
                ui.available_width().min(width),
                "",
            )
        })
    }
    /// Anything else (read-only).
    pub fn custom(title: &'s str, width: f32, f: impl Fn(&mut Ui, &mut UiCx, &T) + 's) -> Self {
        Self {
            title,
            width: Some(width),
            right: false,
            kind: Kind::Custom(Box::new(f)),
        }
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
}

type Fields<'s, T> = Box<dyn FnMut(&mut Ui, &mut T, f32) -> bool + 's>;
type Subtitle<'s, T> = Box<dyn Fn(&[T], usize) -> Option<(String, Tone)> + 's>;
type Leading<'s, T> = (&'s str, f32, Box<dyn Fn(&mut Ui, &mut UiCx, &T) + 's>);

pub struct ListEditor<'s, T: Record> {
    state: &'static State<Vec<T>>,
    columns: Vec<Field<'s, T>>,
    add: Option<(&'s str, Box<dyn Fn() -> T + 's>, Fields<'s, T>)>,
    ordered: bool,
    empty: Option<(Icon, &'s str, &'s str)>,
    subtitle: Option<Subtitle<'s, T>>,
    leading: Option<Leading<'s, T>>,
    headers: bool,
    added: Option<&'static Columns<T>>,
}

impl<'s, T: Record> ListEditor<'s, T> {
    pub fn new(state: &'static State<Vec<T>>) -> Self {
        Self {
            state,
            columns: Vec::new(),
            add: None,
            ordered: true,
            empty: None,
            subtitle: None,
            leading: None,
            headers: true,
            added: None,
        }
    }
    pub fn column(mut self, c: Field<'s, T>) -> Self {
        self.columns.push(c);
        self
    }
    /// Let other plugins add columns ([`Setup::column`](crate::Setup::column)):
    /// they follow the list's own ones.
    pub fn columns_from(mut self, list: &'static Columns<T>) -> Self {
        self.added = Some(list);
        self
    }
    /// Add bar above the list. `fields` draws inputs for the draft into the given
    /// width and returns true on Enter.
    pub fn add_form(
        mut self,
        label: &'s str,
        new: impl Fn() -> T + 's,
        fields: impl FnMut(&mut Ui, &mut T, f32) -> bool + 's,
    ) -> Self {
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
    /// Compact empty state: icon and title.
    pub fn empty(mut self, icon: Icon, title: &'s str, next: &'s str) -> Self {
        self.empty = Some((icon, title, next));
        self
    }
    /// A second line under the first column: status, errors, warnings.
    pub fn subtitle(mut self, f: impl Fn(&[T], usize) -> Option<(String, Tone)> + 's) -> Self {
        self.subtitle = Some(Box::new(f));
        self
    }
    /// A fixed-width marker before the first column (a color dot, a type
    /// badge); `title` heads it like a column's ("" for none).
    pub fn leading(
        mut self,
        title: &'s str,
        width: f32,
        f: impl Fn(&mut Ui, &mut UiCx, &T) + 's,
    ) -> Self {
        self.leading = Some((title, width, Box::new(f)));
        self
    }

    pub fn show(mut self, ui: &mut Ui, cx: &mut UiCx) {
        for c in self.added.map_or(Vec::new(), |list| cx.added(ui, list)) {
            let cell = c.cell.clone();
            let column = Field::custom(
                c.column.title,
                crate::columns::COLUMN_WIDTH,
                move |ui, cx, item| cx.scoped(Some(c.plugin), |cx| cell(ui, cx, item)),
            );
            self.columns.push(column.right());
        }
        let list = egui::Id::unique(("list", self.state.name));
        let editing_id = list.with("editing");
        let mut editing: Option<Id> = ui.data(|d| d.get_temp(editing_id)).flatten();
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            && let Some(id) = editing.take()
        {
            // A closed editor takes its error with it.
            ui.data_mut(|d| d.remove::<String>(list.with(("error", id))));
        }
        self.add_bar(ui, cx, list);
        let rows = cx.get(self.state);
        kit::list_card(ui, |ui| {
            // Rows sit inside `kit::row`'s inset; header and rows share one layout.
            let width = ui.available_width() - 2. * (theme::INSET - theme::SPACE_S);
            let layout = self.layout(width, kit::phone(ui));
            if self.headers && !rows.is_empty() && self.columns.iter().any(|c| !c.title.is_empty())
            {
                self.header(ui, &layout);
            }
            if rows.is_empty() {
                if let Some((icon, title, next)) = self.empty {
                    kit::empty_with(ui, icon, title, next);
                }
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt(("list-rows", self.state.name))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (index, item) in rows.iter().enumerate() {
                        let row = RowView {
                            item,
                            index,
                            len: rows.len(),
                            id: item.id(),
                            list,
                            editing: editing == Some(item.id()),
                            subtitle: self.subtitle.as_ref().and_then(|f| f(&rows, index)),
                        };
                        self.row(ui, cx, &row, &layout, &mut editing);
                    }
                });
        });
        ui.data_mut(|d| d.insert_temp(editing_id, editing));
    }

    fn add_bar(&mut self, ui: &mut Ui, cx: &mut UiCx, list: egui::Id) {
        let Some((label, new, fields)) = &mut self.add else {
            return;
        };
        let draft_id = list.with("new");
        let error_id = list.with("add-error");
        let mut draft: T = ui.data(|d| d.get_temp(draft_id)).unwrap_or_else(&**new);
        let key = (self.state.name, "add");
        match cx.outcome(key) {
            Some(Ok(())) => draft = new(),
            Some(Err(e)) => ui.data_mut(|d| {
                d.insert_temp(error_id, e);
            }),
            None => {}
        }
        let error = ui.data(|d| d.get_temp::<String>(error_id));
        let before = draft.clone();
        let add = kit::add_bar(ui, label, !cx.pending(key), |ui, width| {
            kit::invalid_scope(ui, error.is_some(), |ui| fields(ui, &mut draft, width))
        });
        // The error is about what was submitted: editing the draft clears it.
        if add || draft != before {
            ui.data_mut(|d| d.remove::<String>(error_id));
        }
        if add {
            cx.submit(key, Add::new(self.state, draft.clone()));
        } else if let Some(e) = error.filter(|_| draft == before) {
            kit::field_error(ui, &e, None);
        }
        ui.data_mut(|d| d.insert_temp(draft_id, draft));
    }

    /// Column widths for rows `width` wide.
    /// Narrow rows put the fixed-width columns on a second line, so the main
    /// column keeps at least `MIN_FILL`: plugins don't plan for screen sizes.
    fn layout(&self, width: f32, phone: bool) -> Layout {
        const MIN_FILL: f32 = 160.;
        let gap = theme::HORIZONTAL_GAP;
        let fills = self
            .columns
            .iter()
            .filter(|c| c.width.is_none())
            .count()
            .max(1) as f32;
        let fixed: f32 = self
            .columns
            .iter()
            .filter_map(|c| c.width)
            .map(|w| w + gap)
            .sum();
        let leading = self.leading_width();
        let avail = width - leading - self.actions_width(phone) - gap;
        if fixed > 0. && avail - fixed < MIN_FILL * fills {
            // The second line starts under the main column and has no actions.
            Layout {
                stacked: true,
                scale: ((width - leading) / fixed).min(1.),
                fill: (avail / fills).max(40.),
            }
        } else {
            Layout {
                stacked: false,
                scale: 1.,
                fill: ((avail - fixed) / fills).max(40.),
            }
        }
    }
    fn leading_width(&self) -> f32 {
        self.leading
            .as_ref()
            .map_or(0., |(_, w, _)| w + theme::HORIZONTAL_GAP)
    }
    /// Row actions sit edge to edge: order arrows, edit, delete; on a phone
    /// one button opens them as a menu.
    fn actions_width(&self, phone: bool) -> f32 {
        let n = if self.ordered { 2. } else { 0. } + if self.editable() { 1. } else { 0. } + 1.;
        (if phone { 1. } else { n }) * theme::CONTROL
    }

    /// The row's actions: edit clicked, and a move or delete asked.
    fn actions(&self, ui: &mut Ui, row: &RowView<'_, T>, emphasis: Emphasis) -> (bool, Acted) {
        let mut act = Acted::default();
        let mut edit = false;
        let (icon, label) = if row.editing {
            (Icon::Check, "Close editor")
        } else {
            (Icon::Edit, "Edit")
        };
        ui.spacing_mut().item_spacing.x = 0.;
        let phone = kit::phone(ui);
        if phone && row.editing {
            // The fields take full lines below; only closing stays in reach.
            edit = kit::IconButton::new(icon, label).show(ui).clicked();
            return (edit, act);
        }
        if phone && (self.ordered || self.editable()) {
            kit::row_menu(ui, emphasis, |ui| {
                if self.ordered {
                    if kit::menu_item(ui, Icon::Up, "Move up", row.index > 0, false) {
                        act.moved = Some(true);
                    }
                    if kit::menu_item(ui, Icon::Down, "Move down", row.index + 1 < row.len, false) {
                        act.moved = Some(false);
                    }
                }
                edit = self.editable() && kit::menu_item(ui, icon, label, true, false);
                act.removed = kit::menu_item(ui, Icon::Delete, "Delete", true, true);
            });
            return (edit, act);
        }
        if self.ordered {
            act.moved = kit::order(ui, row.index, row.len, emphasis);
        }
        ui.spacing_mut().item_spacing.x = 0.;
        edit = self.editable()
            && kit::IconButton::new(icon, label)
                .emphasis(emphasis)
                .show(ui)
                .clicked();
        act.removed = kit::remove(ui, emphasis);
        (edit, act)
    }
    fn editable(&self) -> bool {
        self.columns
            .iter()
            .any(|c| matches!(c.kind, Kind::Edit { .. }))
    }

    fn header(&self, ui: &mut Ui, layout: &Layout) {
        kit::list_header(ui, |ui| {
            match &self.leading {
                Some((title, width, _)) if !title.is_empty() => {
                    kit::cell(ui, *width, |ui| kit::caption(ui, *title));
                }
                // Like a row's marker and the gap after it (`add_space` adds no gap).
                _ => ui.add_space(self.leading_width()),
            }
            for c in self.columns.iter().filter(|c| layout.on(c, Line::Main)) {
                let width = layout.width(c);
                if c.right {
                    kit::cell_right(ui, width, |ui| kit::caption(ui, c.title));
                } else {
                    kit::cell(ui, width, |ui| kit::caption(ui, c.title));
                }
            }
        });
    }

    fn row(
        &self,
        ui: &mut Ui,
        cx: &mut UiCx,
        row: &RowView<'_, T>,
        layout: &Layout,
        editing: &mut Option<Id>,
    ) {
        let (id, item) = (row.id, row.item);
        let key = (self.state.name, id);
        let draft_id = row.list.with(("draft", id));
        let error_id = row.list.with(("error", id));
        let close_id = row.list.with(("close", id));
        let mut draft: T = if row.editing {
            ui.data(|d| d.get_temp(draft_id))
                .unwrap_or_else(|| item.clone())
        } else {
            item.clone()
        };
        match cx.outcome(key) {
            Some(Ok(())) => {
                ui.data_mut(|d| d.remove::<String>(error_id));
                if ui
                    .data_mut(|d| d.remove_temp::<bool>(close_id))
                    .unwrap_or(false)
                {
                    *editing = None;
                }
                draft = item.clone();
            }
            Some(Err(e)) => ui.data_mut(|d| {
                d.insert_temp(error_id, e);
            }),
            None => {}
        }
        let mut save = false;
        ui.push_id(id, |ui| {
            kit::row(ui, id, false, |ui, hover| {
                let emphasis = if hover.hovered || row.editing {
                    Emphasis::Normal
                } else {
                    Emphasis::Quiet
                };
                let lines = self.subtitle_lines(ui, row, layout);
                let two_lines = lines > 0;
                let height = [theme::ROW, 44., 58.][lines];
                ui.add_enabled_ui(!cx.pending(key), |ui| {
                    kit::row_line(ui, height, |ui| {
                        self.marker(ui, cx, row, two_lines);
                        self.cells(ui, cx, row, &mut draft, &mut save, layout, Line::Main);
                        let (edit, act) = self.actions(ui, row, emphasis);
                        if let Some(up) = act.moved {
                            cx.submit(key, Move::new(self.state, id, up));
                        }
                        {
                            if edit {
                                if !row.editing {
                                    *editing = Some(id);
                                    draft = item.clone();
                                    ui.data_mut(|d| {
                                        d.insert_temp(row.list.with(("focus", id)), true)
                                    });
                                } else if &draft != item {
                                    save = true;
                                    ui.data_mut(|d| d.insert_temp(close_id, true));
                                } else {
                                    *editing = None;
                                }
                            }
                        }
                        if act.removed {
                            cx.submit(key, Remove::new(self.state, id));
                        }
                    });
                    if row.editing && kit::phone(ui) {
                        // A phone row is too narrow for a field: each gets a full line.
                        for c in &self.columns {
                            if let Kind::Edit {
                                lens, placeholder, ..
                            } = &c.kind
                            {
                                kit::row_line(ui, theme::CONTROL, |ui| {
                                    ui.add_space(self.leading_width());
                                    let width = ui.available_width() - theme::SPACE_S;
                                    let text = lens(&mut draft);
                                    edit_field(
                                        ui,
                                        row,
                                        c.title,
                                        text,
                                        placeholder,
                                        width,
                                        &mut save,
                                    );
                                });
                            }
                        }
                        ui.add_space(4.);
                    }
                    if layout.stacked {
                        // Tucked under the first line; the gap below separates rows.
                        ui.add_space(-4.);
                        // At the row's right edge, where the main line's actions end.
                        let below: f32 = self
                            .columns
                            .iter()
                            .filter(|c| layout.on(c, Line::Below))
                            .map(|c| layout.width(c) + theme::HORIZONTAL_GAP)
                            .sum();
                        kit::row_line(ui, theme::CONTROL, |ui| {
                            ui.add_space((ui.available_width() - below).max(self.leading_width()));
                            self.cells(ui, cx, row, &mut draft, &mut save, layout, Line::Below);
                        });
                        ui.add_space(8.);
                    }
                });
                if let Some(e) = ui.data(|d| d.get_temp::<String>(error_id)) {
                    ui.horizontal(|ui| {
                        ui.add_space(self.leading_width());
                        ui.vertical(|ui| kit::field_error(ui, &e, None));
                    });
                }
            });
        });
        if save && !self.save_edits(cx, row, &mut draft) {
            // Nothing changed: leave the editor if that was asked.
            if ui
                .data_mut(|d| d.remove_temp::<bool>(close_id))
                .unwrap_or(false)
            {
                *editing = None;
            }
        }
        if *editing == Some(id) {
            ui.data_mut(|d| d.insert_temp(draft_id, draft));
        } else {
            ui.data_mut(|d| d.remove::<T>(draft_id));
            if row.editing {
                // A closed editor takes its error with it.
                ui.data_mut(|d| d.remove::<String>(error_id));
            }
        }
    }

    /// Save the edited text fields only, inside the transaction: fields changed
    /// meanwhile (status, timestamps) keep their new values. False when nothing changed.
    fn save_edits(&self, cx: &mut UiCx, row: &RowView<'_, T>, draft: &mut T) -> bool {
        let edits: Vec<(Lens<T>, String)> = self
            .columns
            .iter()
            .filter_map(|c| match &c.kind {
                Kind::Edit { lens, .. } => Some((*lens, lens(draft).clone())),
                _ => None,
            })
            .collect();
        if !edits
            .iter()
            .any(|(lens, value)| lens(&mut row.item.clone()) != value)
        {
            return false;
        }
        cx.submit(
            (self.state.name, row.id),
            Edit::new(self.state, row.id, move |stored: &mut T| {
                for (lens, value) in edits {
                    *lens(stored) = value;
                }
            }),
        );
        true
    }

    /// Lines of a row's subtitle (0: none). On a phone, which has no hover to
    /// read a cut text, a long one takes two.
    fn subtitle_lines(&self, ui: &Ui, row: &RowView<'_, T>, layout: &Layout) -> usize {
        let Some((text, _)) = row.subtitle.as_ref().filter(|_| !row.editing) else {
            return 0;
        };
        let long = |c: &Field<'_, T>| {
            let font = egui::FontId::proportional(theme::SMALL);
            let galley =
                ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), font, egui::Color32::WHITE));
            galley.size().x > layout.width(c)
        };
        if kit::phone(ui) && self.columns.first().is_some_and(long) {
            2
        } else {
            1
        }
    }
    fn marker(&self, ui: &mut Ui, cx: &mut UiCx, row: &RowView<'_, T>, two_lines: bool) {
        let Some((_, width, f)) = &self.leading else {
            return;
        };
        if two_lines {
            // Align the marker with the first line.
            ui.allocate_ui_with_layout(
                egui::vec2(*width, 40.),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(*width);
                    ui.add_space(8.);
                    f(ui, cx, row.item);
                },
            );
        } else {
            kit::cell(ui, *width, |ui| f(ui, cx, row.item));
        }
    }

    /// The cells of one line of a row: the main line or, on narrow rows, the
    /// line below it with the fixed-width columns.
    #[allow(clippy::too_many_arguments)]
    fn cells(
        &self,
        ui: &mut Ui,
        cx: &mut UiCx,
        row: &RowView<'_, T>,
        draft: &mut T,
        save: &mut bool,
        layout: &Layout,
        line: Line,
    ) {
        let item = row.item;
        let lines = self.subtitle_lines(ui, row, layout);
        for (ci, c) in self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, c)| layout.on(c, line))
        {
            let width = layout.width(c);
            // The subtitle lives under the first column's text.
            let text_cell = |ui: &mut Ui, draw: &dyn Fn(&mut Ui)| match &row.subtitle {
                Some((text, tone)) if ci == 0 && lines > 0 => {
                    let ink = theme::palette(ui).tone(*tone);
                    ui.allocate_ui_with_layout(
                        egui::vec2(width, if lines > 1 { 54. } else { 40. }),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(width);
                            ui.spacing_mut().item_spacing.y = 2.;
                            ui.add_space(3.);
                            draw(ui);
                            let small = egui::RichText::new(text).size(theme::SMALL).color(ink);
                            if lines > 1 {
                                let mut job = egui::text::LayoutJob::single_section(
                                    text.clone(),
                                    egui::TextFormat::simple(
                                        egui::FontId::proportional(theme::SMALL),
                                        ink,
                                    ),
                                );
                                job.wrap.max_width = width;
                                job.wrap.max_rows = 2;
                                ui.label(job);
                            } else {
                                kit::truncated(ui, small, None);
                            }
                        },
                    );
                }
                _ if c.right => kit::cell_right(ui, width, draw),
                _ => kit::cell(ui, width, draw),
            };
            match &c.kind {
                Kind::Text { text, color } => text_cell(ui, &|ui| {
                    let mut t = egui::RichText::new(text(item));
                    if let Some(color) = color {
                        t = t.color(color(ui, item));
                    }
                    kit::truncated(ui, t, None);
                }),
                Kind::Edit {
                    lens, placeholder, ..
                } if row.editing && !kit::phone(ui) => {
                    edit_field(ui, row, c.title, lens(draft), placeholder, width, save);
                }
                Kind::Edit {
                    lens,
                    display,
                    color,
                    ..
                } => text_cell(ui, &|ui| {
                    let shown = display
                        .as_ref()
                        .map_or_else(|| lens(&mut item.clone()).clone(), |f| f(item));
                    let mut t = egui::RichText::new(&shown).color(theme::palette(ui).foreground);
                    if let Some(color) = color {
                        t = t.color(color(ui, item));
                    }
                    kit::truncated(ui, t, None);
                }),
                Kind::Widget(f) => {
                    // Applied to the stored item at once, independent of the text draft.
                    let mut fresh = item.clone();
                    if kit::cell(ui, width, |ui| f(ui, &mut fresh)) && fresh != *item {
                        cx.submit((self.state.name, row.id), Save::new(self.state, fresh));
                    }
                }
                Kind::Custom(f) if c.right => kit::cell_right(ui, width, |ui| f(ui, cx, item)),
                Kind::Custom(f) => kit::cell(ui, width, |ui| f(ui, cx, item)),
            }
        }
    }
}

/// The text field of a row being edited: Enter or leaving it saves.
fn edit_field<T>(
    ui: &mut Ui,
    row: &RowView<'_, T>,
    title: &str,
    text: &mut String,
    placeholder: &str,
    width: f32,
    save: &mut bool,
) {
    let r = kit::input(ui, title, text, placeholder, width)
        .on_hover_text("Enter or leave the field to save · Esc discards unsaved text");
    if ui.data(|d| {
        d.get_temp::<String>(row.list.with(("error", row.id)))
            .is_some()
    }) {
        kit::invalid(ui, &r);
    }
    if ui
        .data_mut(|d| d.remove_temp::<bool>(row.list.with(("focus", row.id))))
        .unwrap_or(false)
    {
        r.request_focus();
    }
    *save |= kit::committed(ui, &r);
}

/// What one row shows.
struct RowView<'a, T> {
    item: &'a T,
    index: usize,
    len: usize,
    id: Id,
    list: egui::Id,
    editing: bool,
    subtitle: Option<(String, Tone)>,
}

/// Column widths of one row.
struct Layout {
    /// Too narrow for one line: fixed-width columns move below.
    stacked: bool,
    scale: f32,
    fill: f32,
}
#[derive(Clone, Copy, PartialEq)]
enum Line {
    Main,
    Below,
}
impl Layout {
    fn on<T>(&self, c: &Field<'_, T>, line: Line) -> bool {
        let below = self.stacked && c.width.is_some();
        below == (line == Line::Below)
    }
    fn width<T>(&self, c: &Field<'_, T>) -> f32 {
        c.width.map_or(self.fill, |w| w * self.scale)
    }
}

/// A move or delete asked from a row's actions.
#[derive(Default)]
struct Acted {
    /// `Some(true)`: up.
    moved: Option<bool>,
    removed: bool,
}
