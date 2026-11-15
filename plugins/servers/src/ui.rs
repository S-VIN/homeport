use crate::*;
use rproxy_kernel::{
    Page, PageSpec, UiCx,
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
        id: "servers",
        title: "Servers",
        icon: Icon::Servers,
        order: 10,
    });
    p.fill(&STATUS, "connection", 0, |ui, cx, _| {
        let pal = theme::palette(ui);
        let (color, text, tip) = match selected(cx.snapshot()) {
            Some(s) => (pal.success, short(&s.name, 28), s.name.clone()),
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
        panel_title(ui, "Connection", |ui| {
            kit::badge(ui, "Not connected", Tone::Neutral);
        });
        match snap.hooks(&crate::IDLE, &()).into_iter().flatten().next() {
            Some(status) => drop(kit::tone_text(ui, status, Tone::Neutral)),
            None => {
                let text = "Traffic that should go through the proxy is blocked until you connect a server.";
                let text = egui::RichText::new(text)
                    .size(theme::SMALL)
                    .color(pal.muted);
                ui.add(egui::Label::new(text).wrap());
            }
        }
        if cx.page() != "servers" {
            ui.add_space(6.);
            if Button::secondary("Open Servers")
                .icon(Icon::Servers)
                .show(ui)
                .clicked()
            {
                cx.navigate("servers");
            }
        }
        return;
    };
    panel_title(ui, "Connection", |ui| {
        ui.spacing_mut().item_spacing.x = 4.;
        ui.label(
            egui::RichText::new("Connected")
                .size(theme::SMALL)
                .color(pal.success),
        );
        kit::icon(ui, Icon::Connected, 14., pal.success);
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

#[derive(Default)]
struct ServersPage {
    source: Option<Id>,
    protocol: Option<Protocol>,
    sort: usize,
    frozen: Vec<Id>,
    expanded: HashSet<Id>,
}

impl Page for ServersPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        let snap = cx.snapshot().clone();
        let servers = snap.get(&SERVERS);
        let filtered = servers
            .iter()
            .filter(|s| exclusion(&snap, s).is_some_and(|e| e.disconnect))
            .count();
        let counter = match (servers.len(), filtered) {
            (0, _) => String::new(),
            (n, 0) => kit::plural(n, "server", "servers"),
            (n, f) => format!("{} · {f} excluded", kit::plural(n, "server", "servers")),
        };
        kit::page_header(ui, "Servers", &counter, |ui| cx.slot(ui, &TOOLBAR, &()));
        if !servers.is_empty() {
            self.filters(ui, &snap, &servers);
        }
        cx.slot(ui, &TOOLS, &());
        if servers.is_empty() {
            kit::list_card(ui, |ui| {
                kit::empty(ui, Icon::Servers, "No servers yet");
                ui.vertical_centered(|ui| {
                    if Button::primary("Add subscription")
                        .icon(Icon::Add)
                        .show(ui)
                        .clicked()
                    {
                        cx.navigate("subscriptions");
                    }
                });
                ui.add_space(theme::SPACE_XL);
            });
            return;
        }
        let (rows, frozen) = self.rows(&snap, &servers);
        if rows.is_empty() {
            kit::list_card(ui, |ui| {
                kit::empty(ui, Icon::Filter, "No matching servers");
                ui.vertical_centered(|ui| {
                    if Button::secondary("Reset filters").show(ui).clicked() {
                        self.source = None;
                        self.protocol = None;
                    }
                });
                ui.add_space(theme::SPACE_XL);
            });
            return;
        }
        self.list(ui, cx, &snap, &rows, servers.len(), frozen);
    }
}

impl ServersPage {
    /// The rows the filters keep, each with why it can't be used, in display
    /// order; `true` while a check holds the order still.
    fn rows<'a>(
        &mut self,
        snap: &Snapshot,
        servers: &'a [Server],
    ) -> (Vec<(&'a Server, Option<Exclusion>)>, bool) {
        let sorts = sort_keys(snap);
        if self.sort >= sorts.len() {
            self.sort = 0;
        }
        // Each row with why it can't be used, worked out once per frame.
        let mut rows: Vec<(&Server, Option<Exclusion>)> = servers
            .iter()
            .filter(|s| {
                self.source.is_none_or(|id| id == s.source)
                    && self.protocol.is_none_or(|p| p == s.protocol)
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
            let key = sorts[self.sort].1;
            rows.sort_by_cached_key(|(s, e)| {
                (
                    e.as_ref().map(|e| e.group),
                    key(snap, s),
                    natural_key(&s.name),
                    s.id,
                )
            });
            self.frozen = rows.iter().map(|(s, _)| s.id).collect();
        }
        (rows, frozen)
    }

    fn list(
        &mut self,
        ui: &mut egui::Ui,
        cx: &mut UiCx,
        snap: &Snapshot,
        rows: &[(&Server, Option<Exclusion>)],
        total: usize,
        frozen: bool,
    ) {
        let wide = ui.available_width() >= 500.;
        let selected_id = *snap.get(&SELECTED);
        let core_ok = cx.core_status().running();
        kit::list_card(ui, |ui| {
            kit::list_header(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    kit::action_cell(ui, ACTION, |_| {});
                    cx.slot_rev(
                        ui,
                        &COLUMNS,
                        &Cell {
                            server: None,
                            wide,
                            excluded: false,
                        },
                    );
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add_space(CHEVRON + theme::HORIZONTAL_GAP);
                        let caption = if rows.len() == total {
                            "Server".to_owned()
                        } else {
                            format!("Server · {} of {}", rows.len(), total)
                        };
                        kit::caption(ui, caption);
                    });
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("server-rows")
                .auto_shrink([false, false])
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

/// What a row shows besides the server itself.
struct RowState {
    selected: bool,
    excluded: Option<Exclusion>,
    core_ok: bool,
    wide: bool,
}

fn sort_keys(snap: &Snapshot) -> Vec<SortKey> {
    let mut keys: Vec<SortKey> = vec![("Name", |_, _| 0)];
    keys.extend(snap.hooks(&SORT_KEYS, &()).into_iter().flatten());
    keys
}

impl ServersPage {
    fn filters(&mut self, ui: &mut egui::Ui, snap: &Snapshot, servers: &[Server]) {
        let mut sources: Vec<Id> = Vec::new();
        for s in servers {
            if !sources.contains(&s.source) {
                sources.push(s.source);
            }
        }
        if self.source.is_some_and(|id| !sources.contains(&id)) {
            self.source = None;
        }
        let sorts = sort_keys(snap);
        let gap = theme::HORIZONTAL_GAP;
        let w = ((ui.available_width() - 2. * gap) / 3.).floor();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            kit::select(
                ui,
                "source",
                &self
                    .source
                    .map(|id| source_label(snap, id))
                    .unwrap_or_else(|| "All subscriptions".into()),
                w,
                |ui| {
                    ui.selectable_value(&mut self.source, None, "All subscriptions");
                    for id in &sources {
                        ui.selectable_value(&mut self.source, Some(*id), source_label(snap, *id));
                    }
                },
            );
            kit::select(
                ui,
                "protocol",
                &self
                    .protocol
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "All protocols".into()),
                w,
                |ui| {
                    ui.selectable_value(&mut self.protocol, None, "All protocols");
                    for p in Protocol::ALL
                        .iter()
                        .filter(|p| servers.iter().any(|s| s.protocol == **p))
                    {
                        ui.selectable_value(&mut self.protocol, Some(*p), p.to_string());
                    }
                },
            );
            kit::select(
                ui,
                "sort",
                &format!("Sort: {}", sorts.get(self.sort).map_or("Name", |s| s.0)),
                w,
                |ui| {
                    for (i, (label, _)) in sorts.iter().enumerate() {
                        ui.selectable_value(&mut self.sort, i, *label);
                    }
                },
            );
        });
        ui.add_space(theme::SPACE_M);
    }

    #[allow(clippy::too_many_arguments)]
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
        let mut expanded = self.expanded.contains(&s.id);
        let mut connect = None;
        kit::row(ui, s.id, selected, |ui, row| {
            kit::row_line(ui, theme::ROW, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    kit::action_cell(ui, ACTION, |ui| {
                        connect = connect_button(ui, s, selected, excluded.is_none(), core_ok);
                    });
                    cx.slot_rev(
                        ui,
                        &COLUMNS,
                        &Cell {
                            server: Some(s.clone()),
                            wide,
                            excluded: excluded.is_some(),
                        },
                    );
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
                            Some(TitleClick::Connect)
                                if !selected && excluded.is_none() && core_ok =>
                            {
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
    if let Some(e) = excluded {
        kit::property_with(ui, "Excluded", |ui| {
            ui.add(egui::Label::new(egui::RichText::new(&e.reason).color(pal.warning)).truncate());
        });
    }
    // A long name is cut in the row.
    kit::property(ui, "Name", &s.name);
    kit::property_with(ui, "Subscription", |ui| {
        source_tag(ui, snap, s.source, 200.);
    });
    kit::property(ui, "Address", &format!("{}:{}", s.address, s.port));
    kit::property(ui, "Protocol", &s.protocol.to_string().to_uppercase());
    let (common, rare) = details(snap, s);
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
    usable: bool,
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
    // Unusable rows keep a disabled button, so the column stays even.
    let r = Button::secondary("Connect")
        .icon(Icon::Link)
        .width(ACTION)
        .enabled(usable && core_ok)
        .show(ui);
    let r = if usable && !core_ok {
        r.on_disabled_hover_text("Core offline")
    } else {
        r
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
        let label = source_label(snap, s.source);
        let room = ui.available_width() - CHEVRON - 2. * theme::HORIZONTAL_GAP - 4.;
        // A wide row gives the tag what the name leaves, up to a limit.
        let max_tag = if self.wide {
            let font = egui::FontId::proportional(theme::BODY);
            let name = ui
                .fonts_mut(|f| f.layout_no_wrap(s.name.clone(), font, pal.foreground))
                .size()
                .x;
            (room - name).clamp(180., 320.)
        } else {
            84.
        };
        let tag_w = kit::tag_width(ui, &label, max_tag);
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
                    if self.excluded.is_some() {
                        ui.visuals_mut().override_text_color = Some(pal.muted);
                    }
                    kit::label(ui, &s.name, egui::FontId::proportional(theme::BODY), true)
                });
            source_tag(ui, snap, s.source, tag_w);
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
