use crate::*;
use homeport_kernel::{
    Page, PageSpec, UiCx,
    columns::{ColumnKey, SortKey},
    kit::{self, Button, Lead, Tone, theme},
    shell::{PANELS, STATUS, panel_title},
};
use std::collections::{HashMap, HashSet};

/// Width of the connect / connected cell at the end of a row.
/// Fits "Disconnect" with its icon, so Connect and Disconnect rows align.
const ACTION: f32 = 116.;
/// Chevron column before the name.
const CHEVRON: f32 = 16.;

pub(crate) fn register(p: &mut Setup) {
    p.page::<ServersPage>(PageSpec {
        title: "Servers",
        icon: Icon::Servers,
        order: 10,
    });
    p.fill(&STATUS, "connection", 0, |ui, cx, _| {
        let pal = theme::palette(ui);
        let snap = cx.snapshot();
        let (color, text, tip) = match selected(snap) {
            // Connected to a server that failed its test: it may not work.
            Some(s) => match exclusion(snap, &s) {
                Some(e) => (
                    pal.warning,
                    short(&s.name, 28),
                    format!("{}: {}", s.name, e.reason),
                ),
                None => (pal.success, short(&s.name, 28), s.name.clone()),
            },
            None => (pal.faint, "Not connected".to_owned(), String::new()),
        };
        cx.status_chip(ui, Lead::Dot(color), &text, &tip);
    });
    p.fill(&PANELS, "connection", 0, connection_card);
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}

fn connection_card(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let pal = theme::palette(ui);
    let snap = cx.snapshot().clone();
    let Some(s) = selected(&snap) else {
        // The status bar already says "Not connected".
        homeport_kernel::shell::panel_title_page(ui, cx, "Connection", &PLUGIN);
        match snap.first(&crate::IDLE, &()) {
            Some(status) => drop(kit::tone_text(ui, status, Tone::Neutral)),
            None => {
                let text = "Traffic that should go through the proxy is blocked until you connect a server.";
                let text = egui::RichText::new(text)
                    .size(theme::SMALL)
                    .color(pal.muted);
                ui.add(egui::Label::new(text).wrap());
            }
        }
        return;
    };
    let excluded = exclusion(&snap, &s);
    let tone = if excluded.is_some() {
        pal.warning
    } else {
        pal.success
    };
    panel_title(ui, "Connection", |ui| {
        ui.spacing_mut().item_spacing.x = 4.;
        let r = ui.label(
            egui::RichText::new("Connected")
                .size(theme::SMALL)
                .color(tone),
        );
        if let Some(e) = &excluded {
            r.on_hover_text(&e.reason);
        }
        kit::icon(ui, Icon::Connected, 14., tone);
    });
    ui.scope(|ui| {
        ui.set_max_width(ui.available_width());
        kit::label(ui, &s.name, theme::bold(theme::TITLE), true);
    });
    ui.add_space(4.);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.;
        source_tag(ui, &snap, s.source, 160.);
        kit::badge(ui, &s.protocol.to_string().to_uppercase(), Tone::Neutral);
    });
    ui.add_space(8.);
    cx.slot(ui, &CONNECTION, &s);
    ui.add_space(4.);
    if Button::secondary("Disconnect")
        .icon(Icon::Disconnect)
        .show(ui)
        .on_hover_text(
            "Proxied traffic is blocked until you connect again; direct rules keep working.",
        )
        .clicked()
    {
        cx.submit("disconnect", Select::manual(None));
    }
}

/// The name column's id in the list's sort.
const NAME: ColumnKey = ("servers", "name");

struct ServersPage {
    /// Shown subscriptions and protocols; empty shows all.
    sources: HashSet<Id>,
    protocols: HashSet<Protocol>,
    sort: kit::Sort<ColumnKey>,
    frozen: Vec<Id>,
    expanded: HashSet<Id>,
}
impl Default for ServersPage {
    fn default() -> Self {
        Self {
            sources: HashSet::new(),
            protocols: HashSet::new(),
            sort: kit::Sort::new(NAME),
            frozen: Vec::new(),
            expanded: HashSet::new(),
        }
    }
}

impl Page for ServersPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let snap = cx.snapshot().clone();
        let servers = snap.get(&SERVERS);
        // "84 servers · 1 excluded by name · 3 failed": one part per exclusion group.
        let mut groups: Vec<(&'static str, usize)> = Vec::new();
        for e in servers.iter().filter_map(|s| exclusion(&snap, s)) {
            match groups.iter_mut().find(|(g, _)| *g == e.group) {
                Some((_, n)) => *n += 1,
                None => groups.push((e.group, 1)),
            }
        }
        let counter = match servers.len() {
            0 => String::new(),
            n => std::iter::once(kit::plural(n, "server", "servers"))
                .chain(
                    groups
                        .iter()
                        .map(|(g, n)| format!("{n} {}", g.to_lowercase())),
                )
                .collect::<Vec<_>>()
                .join(" · "),
        };
        if servers.is_empty() {
            // The toolbar acts on servers: nothing to show it for.
            cx.page_header(ui, "", |_, _| {});
            kit::list_card(ui, |ui| {
                kit::empty(ui, Icon::Servers, "No servers yet");
                ui.vertical_centered(|ui| cx.slot(ui, &EMPTY, &()));
                ui.add_space(theme::SPACE_XL);
            });
            return;
        }
        cx.page_header(ui, &counter, |ui, cx| cx.slot(ui, &TOOLBAR, &()));
        // Filters of subscriptions that are gone no longer apply.
        self.sources
            .retain(|id| servers.iter().any(|s| s.source == *id));
        self.protocols
            .retain(|p| servers.iter().any(|s| s.protocol == *p));
        let key = cx.column_sort(&COLUMNS, self.sort.column);
        let (rows, frozen) = self.rows(&snap, &servers, key);
        self.list(ui, cx, &snap, &servers, &rows, frozen);
    }
}

impl ServersPage {
    /// The rows the filters keep, each with why it can't be used, in display
    /// order; `true` while a check holds the order still.
    fn rows<'a>(
        &mut self,
        snap: &Snapshot,
        servers: &'a [Server],
        key: Option<SortKey<Server>>,
    ) -> (Vec<(&'a Server, Option<Exclusion>)>, bool) {
        if key.is_none() && self.sort.column != NAME {
            // Its plugin was turned off.
            self.sort = kit::Sort::new(NAME);
        }
        // Each row with why it can't be used, worked out once per frame.
        let mut rows: Vec<(&Server, Option<Exclusion>)> = servers
            .iter()
            .filter(|s| {
                (self.sources.is_empty() || self.sources.contains(&s.source))
                    && (self.protocols.is_empty() || self.protocols.contains(&s.protocol))
            })
            .map(|s| (s, exclusion(snap, s)))
            .collect();
        let frozen = snap.hooks(&FREEZE, &()).into_iter().any(|f| f) && !self.frozen.is_empty();
        if frozen {
            let rank: HashMap<Id, usize> = self
                .frozen
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, i))
                .collect();
            rows.sort_by_key(|(s, _)| rank.get(&s.id).copied().unwrap_or(usize::MAX));
        } else {
            // Unusable servers stay last, in groups, whatever the order.
            let reverse = self.sort.dir == kit::SortDir::Desc;
            let mut keyed: Vec<_> = rows
                .into_iter()
                .map(|row| {
                    let primary = key.map_or(0, |k| k(snap, row.0));
                    let group = row.1.as_ref().map(|e| e.group);
                    (group, primary, natural_key(&row.0.name), row)
                })
                .collect();
            keyed.sort_by(|a, b| {
                let (by_key, by_name) = (a.1.cmp(&b.1), a.2.cmp(&b.2));
                let order = if key.is_some() {
                    by_key.then(by_name)
                } else {
                    by_name
                };
                a.0.cmp(&b.0)
                    .then(if reverse { order.reverse() } else { order })
                    .then(a.3.0.id.cmp(&b.3.0.id))
            });
            rows = keyed.into_iter().map(|k| k.3).collect();
            self.frozen = rows.iter().map(|(s, _)| s.id).collect();
        }
        (rows, frozen)
    }

    fn list(
        &mut self,
        ui: &mut egui::Ui,
        cx: &mut UiCx,
        snap: &Snapshot,
        servers: &[Server],
        rows: &[(&Server, Option<Exclusion>)],
        frozen: bool,
    ) {
        let total = servers.len();
        let wide = !kit::phone(ui);
        let selected_id = *snap.get(&SELECTED);
        let core_ok = cx.core_status().running();
        kit::list_card(ui, |ui| {
            kit::list_header(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    kit::action_cell(ui, ACTION, |_| {});
                    cx.column_titles(ui, &COLUMNS, &mut self.sort);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add_space(CHEVRON + theme::HORIZONTAL_GAP);
                        ui.spacing_mut().item_spacing.x = theme::SPACE_S;
                        if kit::sort_caption(ui, "Server", self.sort.of(NAME), false) {
                            self.sort.click(NAME);
                        }
                        self.filter_menu(ui, snap, servers);
                        if rows.len() != total {
                            kit::caption(ui, format!("{} of {}", rows.len(), total));
                        }
                    });
                });
            });
            if rows.is_empty() {
                kit::empty(ui, Icon::Filter, "No matching servers");
                ui.vertical_centered(|ui| {
                    if Button::secondary("Reset filters").show(ui).clicked() {
                        self.sources.clear();
                        self.protocols.clear();
                    }
                });
                ui.add_space(theme::SPACE_XL);
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("server-rows")
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let group = |i: usize| rows[i].1.as_ref().map(|e| e.group);
                    for (i, (s, excluded)) in rows.iter().enumerate() {
                        // Servers that can't be used close the list in groups,
                        // each under its caption (not while a check freezes the order).
                        if let Some(name) = group(i).filter(|_| !frozen && i > 0)
                            && group(i - 1) != Some(name)
                        {
                            let count = (i..rows.len())
                                .take_while(|&j| group(j) == Some(name))
                                .count();
                            ui.add_space(theme::SPACE_M);
                            ui.horizontal(|ui| {
                                ui.add_space(CHEVRON + theme::HORIZONTAL_GAP);
                                kit::caption(ui, format!("{name} · {count}"));
                            });
                        }
                        let excluded = excluded.clone();
                        ui.push_id(s.id, |ui| {
                            let state = RowState {
                                selected: selected_id == Some(s.id),
                                excluded,
                                core_ok,
                                wide,
                            };
                            self.row(ui, cx, snap, s, state)
                        });
                    }
                });
        });
    }
}

fn toggle<T: Eq + std::hash::Hash>(set: &mut HashSet<T>, item: T, on: bool) {
    if on {
        set.insert(item);
    } else {
        set.remove(&item);
    }
}

/// What a row shows besides the server itself.
struct RowState {
    selected: bool,
    excluded: Option<Exclusion>,
    core_ok: bool,
    wide: bool,
}

impl ServersPage {
    /// The funnel after the Server caption: subscriptions and protocols to show.
    fn filter_menu(&mut self, ui: &mut egui::Ui, snap: &Snapshot, servers: &[Server]) {
        let mut sources: Vec<Id> = Vec::new();
        for s in servers {
            if !sources.contains(&s.source) {
                sources.push(s.source);
            }
        }
        let protocols: Vec<Protocol> = Protocol::ALL
            .iter()
            .copied()
            .filter(|p| servers.iter().any(|s| s.protocol == *p))
            .collect();
        let active = self.sources.len() + self.protocols.len();
        kit::filter_button(ui, "servers", "Filter servers", active, |ui| {
            if sources.len() > 1 {
                let kinds: Vec<_> = sources.iter().map(|id| source(snap, *id).kind).collect();
                let same = kinds.iter().all(|k| *k == kinds[0]);
                kit::filter_group(ui, if same { kinds[0] } else { "Source" });
                for id in &sources {
                    let mut on = self.sources.contains(id);
                    let changed = ui
                        .horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = theme::SPACE_S;
                            let src = source(snap, *id);
                            let changed = kit::filter_option(ui, &mut on, &src.label);
                            kit::tag_dot(ui, src.hue);
                            changed
                        })
                        .inner;
                    if changed {
                        toggle(&mut self.sources, *id, on);
                    }
                }
            }
            if protocols.len() > 1 {
                kit::filter_group(ui, "Protocol");
                for p in &protocols {
                    let mut on = self.protocols.contains(p);
                    if kit::filter_option(ui, &mut on, &p.to_string().to_uppercase()) {
                        toggle(&mut self.protocols, *p, on);
                    }
                }
            }
            if sources.len() <= 1 && protocols.len() <= 1 {
                kit::hint(ui, "All servers share one source and protocol");
            }
            if active > 0 {
                ui.add_space(theme::SPACE_S);
                if Button::ghost("Show all")
                    .tone(Tone::Accent)
                    .show(ui)
                    .clicked()
                {
                    self.sources.clear();
                    self.protocols.clear();
                }
            }
        });
    }

    fn row(
        &mut self,
        ui: &mut egui::Ui,
        cx: &mut UiCx,
        snap: &Snapshot,
        s: &Server,
        state: RowState,
    ) {
        let RowState {
            selected,
            excluded,
            core_ok,
            wide,
        } = state;
        // A filter blocks a server; a failed test only warns (it may be wrong).
        let blocked = excluded.as_ref().is_some_and(|e| e.permanent);
        let mut expanded = self.expanded.contains(&s.id);
        let mut connect = None;
        kit::row(ui, s.id, selected, |ui, row| {
            kit::row_line(ui, theme::ROW, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    kit::action_cell(ui, ACTION, |ui| {
                        connect = connect_button(ui, s, selected, excluded.as_ref(), core_ok);
                    });
                    cx.column_cells(ui, &COLUMNS, s, blocked);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let title = Title {
                            server: s,
                            excluded: excluded.as_ref(),
                            expanded,
                            hovered: row.hovered,
                            wide,
                        };
                        match title.show(ui, snap) {
                            Some(TitleClick::Toggle) => expanded = !expanded,
                            Some(TitleClick::Connect) if !selected && !blocked && core_ok => {
                                connect = Some(Some(s.id))
                            }
                            _ => {}
                        }
                    });
                });
            });
            if expanded {
                details_frame(ui, cx, snap, s, &excluded);
            }
        });
        if let Some(id) = connect {
            cx.submit(("select", s.id), Select::manual(id));
        }
        if expanded {
            self.expanded.insert(s.id);
        } else {
            self.expanded.remove(&s.id);
        }
    }
}

fn details_body(
    ui: &mut egui::Ui,
    cx: &mut UiCx,
    snap: &Snapshot,
    s: &Server,
    excluded: &Option<Exclusion>,
) {
    let pal = theme::palette(ui);
    ui.spacing_mut().item_spacing.y = 0.;
    // A long name is cut in the row.
    kit::property(ui, "Name", &s.name);
    if let Some(e) = excluded {
        kit::property_with(ui, "Status", |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&e.reason).color(pal.warning)).truncate());
        });
    }
    kit::property_with(ui, source(snap, s.source).kind, |ui| {
        source_tag(ui, snap, s.source, 200.);
    });
    kit::property(ui, "Address", &format!("{}:{}", s.address, s.port));
    kit::property(ui, "Protocol", &s.protocol.to_string().to_uppercase());
    let (common, rare) = details(s);
    for (k, v) in &common {
        kit::property(ui, k, v);
    }
    cx.slot(ui, &DETAILS, s);
    if !rare.is_empty() {
        ui.add_space(2.);
        kit::disclosure(ui, ("more", s.id), "More details", |ui| {
            for (k, v) in &rare {
                kit::property(ui, k, v);
            }
        });
    }
}

/// Connect, or Disconnect for the connected server: `Some(choice)` when clicked.
fn connect_button(
    ui: &mut egui::Ui,
    s: &Server,
    selected: bool,
    excluded: Option<&Exclusion>,
    core_ok: bool,
) -> Option<Option<Id>> {
    if selected {
        let r = Button::ghost("Disconnect")
            .icon(Icon::Disconnect)
            .tone(Tone::Danger)
            .width(ACTION)
            .show(ui);
        return r.clicked().then_some(None);
    }
    // Blocked rows keep a disabled button, so the column stays even; it says why.
    let blocked = excluded.is_some_and(|e| e.permanent);
    let r = Button::secondary("Connect")
        .icon(Icon::Link)
        .width(ACTION)
        .enabled(!blocked && core_ok)
        .show(ui);
    let r = match excluded {
        Some(e) if blocked => r.on_disabled_hover_text(&e.reason),
        _ if !core_ok => r.on_disabled_hover_text("Core offline"),
        Some(e) => r.on_hover_text(&e.reason),
        None => r,
    };
    r.clicked().then_some(Some(s.id))
}

enum TitleClick {
    /// Chevron or name: show or hide the details.
    Toggle,
    /// Double click on the name.
    Connect,
}
/// Chevron, name and source tag of a row; an excluded server says why on
/// hover (and in its details). One click target expands the row.
enum Tag {
    None,
    Pill(f32),
    Dot,
}
struct Title<'a> {
    server: &'a Server,
    excluded: Option<&'a Exclusion>,
    expanded: bool,
    hovered: bool,
    wide: bool,
}
impl Title<'_> {
    fn show(&self, ui: &mut egui::Ui, snap: &Snapshot) -> Option<TitleClick> {
        let (s, pal) = (self.server, theme::palette(ui));
        let SourceInfo { label, hue, .. } = source(snap, s.source);
        let room = ui.available_width() - CHEVRON - 2. * theme::HORIZONTAL_GAP - 4.;
        // The name comes first; the tag gets what is left, at most a third of
        // the row, and shrinks to a colored dot below that. A source named
        // like its only server (a single link) needs no tag.
        let font = egui::FontId::proportional(theme::BODY);
        let name_need = ui
            .fonts_mut(|f| f.layout_no_wrap(s.name.clone(), font, pal.foreground))
            .size()
            .x;
        let show_tag = label != s.name;
        let tag_room = (room - name_need)
            .min(room / 3.)
            .min(if self.wide { 240. } else { 120. });
        let tag = match () {
            _ if !show_tag => Tag::None,
            _ if tag_room >= 56. => Tag::Pill(kit::tag_width(ui, &label, tag_room)),
            _ => Tag::Dot,
        };
        let tag_w = match tag {
            Tag::None => 0.,
            Tag::Pill(w) => w,
            Tag::Dot => 10.,
        };
        let name_w = (room - tag_w).max(40.);
        let (rect, chevron) =
            ui.allocate_exact_size(egui::vec2(CHEVRON, theme::CONTROL), egui::Sense::click());
        let ink = if chevron.hovered() || self.hovered {
            pal.muted
        } else {
            pal.faint
        };
        let icon = if self.expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        kit::icons::paint_sized(ui.painter(), rect.center(), icon, 13., ink);
        chevron.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::Button, true, self.expanded, "Details")
        });
        let name_line = |ui: &mut egui::Ui| {
            let layout = egui::Layout::left_to_right(egui::Align::Center);
            let name =
                ui.allocate_ui_with_layout(egui::vec2(name_w, theme::CONTROL), layout, |ui| {
                    ui.set_max_width(name_w);
                    // Only a filter's block greys the name; a failed test is
                    // said by the Rating and may be wrong.
                    if self.excluded.is_some_and(|e| e.permanent) {
                        ui.visuals_mut().override_text_color = Some(pal.muted);
                    }
                    kit::label(ui, &s.name, egui::FontId::proportional(theme::BODY), true)
                });
            match tag {
                Tag::None => {}
                Tag::Pill(w) => drop(source_tag(ui, snap, s.source, w)),
                Tag::Dot => drop(kit::tag_dot(ui, hue).on_hover_text(&label)),
            }
            name.inner
        };
        let name = name_line(ui);
        let name = match self.excluded {
            Some(e) => name.on_hover_text(&e.reason),
            None => name,
        };
        if name.double_clicked() {
            Some(TitleClick::Connect)
        } else if chevron.clicked() || name.clicked() {
            Some(TitleClick::Toggle)
        } else {
            None
        }
    }
}

/// The expanded part of a row.
fn details_frame(
    ui: &mut egui::Ui,
    cx: &mut UiCx,
    snap: &Snapshot,
    s: &Server,
    excluded: &Option<Exclusion>,
) {
    let pal = theme::palette(ui);
    ui.horizontal(|ui| {
        ui.add_space(CHEVRON + theme::HORIZONTAL_GAP);
        egui::Frame::NONE
            .fill(if pal.dark { pal.background } else { pal.hover })
            .stroke(egui::Stroke::new(1_f32, pal.border))
            .corner_radius(theme::RADIUS_CONTROL)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width() - 4.);
                ui.vertical(|ui| details_body(ui, cx, snap, s, excluded));
            });
    });
    ui.add_space(6.);
}
