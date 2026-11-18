//! Columns other plugins add to a list: the owner declares [`Columns`] and
//! draws them with [`UiCx::column_titles`] and [`UiCx::column_cells`] (or
//! hands them to a `ListEditor`); a plugin adds one with [`Setup::column`].
//! The kernel keeps them aligned, sorts by them and leaves the secondary
//! ones out on a phone.
use crate::{Setup, Snapshot, UiCx, egui, kit};
use std::{marker::PhantomData, sync::Arc};

/// A list that accepts columns from other plugins:
/// `pub static COLUMNS: Columns<Server> = Columns::new("servers.columns", "…")`.
pub struct Columns<T: 'static> {
    pub id: &'static str,
    /// One line for documentation: which list and what belongs there.
    pub about: &'static str,
    _t: PhantomData<fn(&T)>,
}
impl<T> Columns<T> {
    pub const fn new(id: &'static str, about: &'static str) -> Self {
        Self {
            id,
            about,
            _t: PhantomData,
        }
    }
}

/// A column added to a [`Columns`] list.
pub struct Column<T: 'static> {
    /// Unique within the plugin.
    pub id: &'static str,
    /// Lower is further left.
    pub order: i32,
    pub title: &'static str,
    /// Tooltip of the title.
    pub help: &'static str,
    /// Left out on a phone: only the most useful columns stay.
    pub secondary: bool,
    /// Click the title to sort by this key (lower first; again to reverse).
    pub sort: Option<SortKey<T>>,
}
/// The sort key of a column: lower keys first in its natural order.
pub type SortKey<T> = fn(&Snapshot, &T) -> i64;
/// An added column: its plugin's id and its own. List owners use the same
/// shape for their built-in columns in a `kit::Sort`.
pub type ColumnKey = (&'static str, &'static str);
/// Width of an added column.
pub const COLUMN_WIDTH: f32 = 60.;

type CellFn<T> = Arc<dyn Fn(&mut egui::Ui, &mut UiCx, &T) + Send + Sync>;
pub(crate) struct Added<T: 'static> {
    pub key: ColumnKey,
    pub plugin: usize,
    pub column: Column<T>,
    pub cell: CellFn<T>,
}
impl<T> Clone for Added<T> {
    fn clone(&self) -> Self {
        Self {
            cell: self.cell.clone(),
            ..*self
        }
    }
}
impl<T> Clone for Column<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Column<T> {}
pub(crate) struct AddedColumns<T: 'static>(pub Vec<Added<T>>);

impl Setup<'_> {
    /// Add a column to another plugin's list: `cell` draws one item's value.
    pub fn column<T: 'static>(
        &mut self,
        list: &'static Columns<T>,
        column: Column<T>,
        cell: impl Fn(&mut egui::Ui, &mut UiCx, &T) + Send + Sync + 'static,
    ) {
        let (plugin, key) = (self.me(), (self.plugin_id(), column.id));
        let all = &mut self
            .reg
            .columns
            .entry(list.id)
            .or_insert_with(|| Box::new(AddedColumns::<T>(Vec::new())))
            .downcast_mut::<AddedColumns<T>>()
            .expect("column item type")
            .0;
        all.push(Added {
            key,
            plugin,
            column,
            cell: Arc::new(cell),
        });
        all.sort_by_key(|c| (c.column.order, c.plugin));
    }
}

impl UiCx<'_> {
    /// The columns of `list` that fit: all of them, or only the primary ones on a phone.
    pub(crate) fn added<T: 'static>(&self, ui: &egui::Ui, list: &Columns<T>) -> Vec<Added<T>> {
        let phone = kit::phone(ui);
        (self.k.reg.columns.get(list.id))
            .and_then(|c| c.downcast_ref::<AddedColumns<T>>())
            .map_or(&[][..], |c| &c.0)
            .iter()
            .filter(|c| !(phone && c.column.secondary))
            .cloned()
            .collect()
    }
    /// The sort key of an added column of `list`; `None` when its plugin is off
    /// or it does not sort.
    pub fn column_sort<T: 'static>(&self, list: &Columns<T>, key: ColumnKey) -> Option<SortKey<T>> {
        (self.k.reg.columns.get(list.id))
            .and_then(|c| c.downcast_ref::<AddedColumns<T>>())?
            .0
            .iter()
            .find(|c| c.key == key)?
            .column
            .sort
    }
    /// The titles of `list`'s added columns in a header row (also in a
    /// right-to-left layout); a click on a sortable one updates `sort`.
    pub fn column_titles<T: 'static>(
        &mut self,
        ui: &mut egui::Ui,
        list: &Columns<T>,
        sort: &mut kit::Sort<ColumnKey>,
    ) {
        for c in in_layout_order(ui, self.added(ui, list)) {
            kit::cell_right(ui, COLUMN_WIDTH, |ui| {
                let r = match c.column.sort {
                    Some(_) => {
                        let r = ui.scope(|ui| {
                            kit::sort_caption(ui, c.column.title, sort.of(c.key), true)
                        });
                        if r.inner {
                            sort.click(c.key);
                        }
                        r.response
                    }
                    None => kit::caption(ui, c.column.title),
                };
                r.on_hover_text(c.column.help);
            });
        }
    }
    /// The cells of `item` under [`UiCx::column_titles`]; `dimmed` when the
    /// item does not apply now (say, a server a filter excludes).
    pub fn column_cells<T: 'static>(
        &mut self,
        ui: &mut egui::Ui,
        list: &Columns<T>,
        item: &T,
        dimmed: bool,
    ) {
        for c in in_layout_order(ui, self.added(ui, list)) {
            kit::cell_right(ui, COLUMN_WIDTH, |ui| {
                if dimmed {
                    ui.multiply_opacity(kit::theme::DIMMED);
                }
                ui.push_id(c.key, |ui| {
                    self.scoped(Some(c.plugin), |cx| (c.cell)(ui, cx, item))
                });
            });
        }
    }
}
/// Left to right as listed, whichever way `ui` lays out.
fn in_layout_order<T>(ui: &egui::Ui, mut list: Vec<Added<T>>) -> Vec<Added<T>> {
    if ui.layout().prefer_right_to_left() {
        list.reverse();
    }
    list
}
