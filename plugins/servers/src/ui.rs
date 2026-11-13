use crate::*;
use rproxy_kernel::{
    kit::{self, emoji, theme, Button, Lead, Tone},
    shell::{panel_title, PANELS, STATUS},
    Page, PageSpec, UiCx,
};
use std::collections::{HashMap, HashSet};

/// Width of the connect / connected cell at the end of a row.
const ACTION: f32 = 96.;
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
            Some(s) => (
                pal.success,
                short(&s.name, 28),
                format!("Connected via {}", s.name),
            ),
            None => (
                pal.faint,
                "Not connected".to_owned(),
                "No server selected".to_owned(),
            ),
        };
        if kit::chip(ui, Some(Lead::Dot(color)), &text, &tip).clicked() {
            cx.toggle_panels();
        }
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

fn connected_label(ui: &mut egui::Ui) {
    let pal = theme::palette(ui);
    kit::icon(ui, Icon::Connected, 15., pal.success);
    ui.label(
        egui::RichText::new("Connected")
            .size(theme::SMALL)
            .color(pal.success),
    );
}

fn connection_card(ui: &mut egui::Ui, cx: &mut UiCx, _: &()) {
    let pal = theme::palette(ui);
    let snap = cx.snapshot().clone();
    let Some(s) = selected(&snap) else {
        panel_title(ui, "Connection", |ui| {
            kit::badge(ui, "Not connected", Tone::Neutral);
        });
        let hint = snap
            .hooks(&crate::IDLE, &())
            .into_iter()
            .flatten()
            .next()
            .unwrap_or_else(|| "Choose a server on the Servers page or turn on Auto select.".into());
        ui.add(
            egui::Label::new(
                egui::RichText::new(hint)
                    .size(theme::SMALL)
                    .color(pal.muted),
            )
            .wrap(),
        );
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
        emoji::label(ui, &s.name, theme::bold(theme::TITLE), true).on_hover_text(&s.name);
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
        .on_hover_text("Stop using this server")
        .clicked()
    {
        cx.submit(
            "disconnect",
            Select {
                id: None,
                auto: false,
            },
        );
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
                kit::empty(
                    ui,
                    Icon::Servers,
                    "No servers yet",
                    "Servers appear here after a subscription loads.",
                );
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
        let sorts = sort_keys(&snap);
        if self.sort >= sorts.len() {
            self.sort = 0;
        }
        let mut rows: Vec<&Server> = servers
            .iter()
            .filter(|s| {
                self.source.is_none_or(|id| id == s.source)
                    && self.protocol.is_none_or(|p| p == s.protocol)
            })
            .collect();
        let frozen = snap.hooks(&FREEZE, &()).into_iter().any(|f| f) && !self.frozen.is_empty();
        if frozen {
            let rank: HashMap<Id, usize> = self
                .frozen
                .iter()
                .enumerate()
                .map(|(i, id)| (*id, i))
                .collect();
            rows.sort_by_key(|s| rank.get(&s.id).copied().unwrap_or(usize::MAX));
        } else {
            let key = sorts[self.sort].1;
            rows.sort_by_cached_key(|s| {
                (
                    exclusion(&snap, s).is_some(),
                    key(&snap, s),
                    natural_key(&s.name),
                    s.id,
                )
            });
            self.frozen = rows.iter().map(|s| s.id).collect();
        }
        if rows.is_empty() {
            kit::list_card(ui, |ui| {
                kit::empty(
                    ui,
                    Icon::Filter,
                    "No matching servers",
                    "No server matches the source and protocol filters.",
                );
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
        let wide = ui.available_width() >= 500.;
        let selected_id = *snap.get(&SELECTED);
        let core_ok = cx.core_status().running();
        kit::list_card(ui, |ui| {
            kit::list_header(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    kit::cell(ui, ACTION, |_| {});
                    cx.slot_rev(ui, &COLUMNS, &Cell { server: None, wide });
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add_space(CHEVRON + theme::HORIZONTAL_GAP);
                        let caption = if rows.len() == servers.len() {
                            "Server".to_owned()
                        } else {
                            format!("Server · {} of {}", rows.len(), servers.len())
                        };
                        kit::caption(ui, caption);
                    });
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("server-rows")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for s in rows {
                        ui.push_id(s.id, |ui| {
                            self.row(ui, cx, &snap, s, selected_id == Some(s.id), core_ok, wide)
                        });
                    }
                });
        });
    }
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
                    .unwrap_or_else(|| "All sources".into()),
                w,
                |ui| {
                    ui.selectable_value(&mut self.source, None, "All sources");
                    for id in &sources {
                        ui.selectable_value(&mut self.source, Some(*id), source_label(snap, *id));
                    }
                },
            )
            .on_hover_text("Show servers of one subscription");
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
            )
            .on_hover_text("Show servers of one protocol");
            kit::select(
                ui,
                "sort",
                &format!(
                    "Sort by {}",
                    sorts.get(self.sort).map_or("Name", |s| s.0).to_lowercase()
                ),
                w,
                |ui| {
                    for (i, (label, _)) in sorts.iter().enumerate() {
                        ui.selectable_value(&mut self.sort, i, *label);
                    }
                },
            )
            .on_hover_text("Order of the list");
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
        selected: bool,
        core_ok: bool,
        wide: bool,
    ) {
        let pal = theme::palette(ui);
        let excluded = exclusion(snap, s);
        let mut expanded = self.expanded.contains(&s.id);
        let mut connect = None;
        kit::row(ui, s.id, selected, |ui, row| {
            let two_lines = excluded.is_some() && !expanded;
            kit::row_line(ui, if two_lines { 44. } else { theme::ROW }, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    kit::cell_right(ui, ACTION, |ui| {
                        ui.spacing_mut().item_spacing.x = 4.;
                        let usable = excluded.is_none() && core_ok;
                        if selected && row.hovered {
                            if Button::ghost("Disconnect")
                                .tone(Tone::Danger)
                                .width(ACTION)
                                .show(ui)
                                .on_hover_text("Stop using this server")
                                .clicked()
                            {
                                connect = Some(None);
                            }
                        } else if selected {
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add_space(8.);
                                    connected_label(ui);
                                },
                            );
                        } else if row.hovered && usable {
                            if Button::secondary("Connect")
                                .width(ACTION)
                                .show(ui)
                                .on_hover_text("Send traffic through this server")
                                .clicked()
                            {
                                connect = Some(Some(s.id));
                            }
                        } else if row.hovered && excluded.is_none() {
                            kit::hint(ui, "Core offline");
                        }
                    });
                    cx.slot_rev(
                        ui,
                        &COLUMNS,
                        &Cell {
                            server: Some(s.clone()),
                            wide,
                        },
                    );
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        // Chevron, name and source tag share one click target that expands the row.
                        let avail = ui.available_width();
                        let label = source_label(snap, s.source);
                        let tag_w = (ui
                            .painter()
                            .layout_no_wrap(
                                label,
                                egui::FontId::proportional(theme::CAPTION),
                                pal.muted,
                            )
                            .size()
                            .x
                            + 12.)
                            .min(if wide { 130. } else { 84. });
                        let name_w =
                            (avail - CHEVRON - tag_w - 2. * theme::HORIZONTAL_GAP - 4.).max(40.);
                        let (rect, click) = ui.allocate_exact_size(
                            egui::vec2(CHEVRON, theme::CONTROL),
                            egui::Sense::click(),
                        );
                        let ink = if click.hovered() || row.hovered {
                            pal.muted
                        } else {
                            pal.faint
                        };
                        kit::icons::paint_sized(
                            ui.painter(),
                            rect.center(),
                            if expanded {
                                Icon::ChevronDown
                            } else {
                                Icon::ChevronRight
                            },
                            13.,
                            ink,
                        );
                        // Name and source tag; an excluded server says why on a second line.
                        let name_line = |ui: &mut egui::Ui| {
                            let name = ui
                                .allocate_ui_with_layout(
                                    egui::vec2(name_w, theme::CONTROL),
                                    egui::Layout::left_to_right(egui::Align::Center),
                                    |ui| {
                                        ui.set_max_width(name_w);
                                        if excluded.is_some() {
                                            ui.visuals_mut().override_text_color = Some(pal.muted);
                                        }
                                        emoji::label(
                                            ui,
                                            &s.name,
                                            egui::FontId::proportional(theme::BODY),
                                            true,
                                        )
                                    },
                                )
                                .inner;
                            source_tag(ui, snap, s.source, tag_w);
                            name
                        };
                        let name = if let Some(e) = excluded.as_ref().filter(|_| two_lines) {
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 40.),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.spacing_mut().item_spacing.y = 0.;
                                    let name = ui.horizontal(name_line).inner;
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(&e.reason)
                                                .size(theme::SMALL)
                                                .color(pal.warning),
                                        )
                                        .truncate(),
                                    );
                                    name
                                },
                            )
                            .inner
                        } else {
                            name_line(ui)
                        };
                        let tip = match &excluded {
                            Some(e) => format!("{}\n{}", s.name, e.reason),
                            None if selected => format!("{}\nClick for details", s.name),
                            None => {
                                format!("{}\nClick for details · double-click to connect", s.name)
                            }
                        };
                        let name = name.on_hover_text(tip);
                        if name.double_clicked() && !selected && excluded.is_none() && core_ok {
                            connect = Some(Some(s.id));
                        } else if click.clicked() || name.clicked() {
                            expanded = !expanded;
                        }
                        click.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::Role::Button,
                                true,
                                expanded,
                                "Details",
                            )
                        });
                    });
                });
            });
            if expanded {
                ui.horizontal(|ui| {
                    ui.add_space(CHEVRON + theme::HORIZONTAL_GAP);
                    egui::Frame::NONE
                        .fill(if pal.dark { pal.background } else { pal.hover })
                        .stroke(egui::Stroke::new(1_f32, pal.border))
                        .corner_radius(theme::RADIUS_CONTROL)
                        .inner_margin(egui::Margin::symmetric(10, 6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width() - 4.);
                            ui.vertical(|ui| self::details_body(ui, cx, snap, s, &excluded));
                        });
                });
                ui.add_space(6.);
            }
        });
        if let Some(id) = connect {
            cx.submit(("select", s.id), Select { id, auto: false });
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
    kit::property_with(ui, "Source", |ui| {
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
