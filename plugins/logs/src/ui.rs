//! The Logs page: both logs in time order, filtered by level and source.
use crate::{APP, Clear, LINES, Level};
use homeport_kernel::{kit, kit::theme, logging, prelude::*};

/// Whose lines the page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Source {
    #[default]
    All,
    /// Homeport and its plugins.
    App,
    /// mihomo.
    Core,
}

/// What the page shows, kept per window: the lowest level, whose lines, and
/// the core's lines from Homeport's own work.
#[derive(Default)]
pub(crate) struct LogsPage {
    min: Level,
    source: Source,
    all: bool,
}
impl Page for LogsPage {
    fn ui(&mut self, ui: &mut egui::Ui, cx: &mut UiCx) {
        page(ui, cx, self);
    }
}

/// One line of either log, as the page draws it.
struct Row<'a> {
    at: u64,
    level: Level,
    /// The writing plugin of an app line, "Core" for the core's.
    source: &'static str,
    text: &'a str,
    count: u32,
}
fn app_level(level: logging::Level) -> Level {
    match level {
        logging::Level::Error => Level::Error,
        logging::Level::Warn => Level::Warning,
        _ => Level::Info,
    }
}

fn page(ui: &mut egui::Ui, cx: &mut UiCx, view: &mut LogsPage) {
    let (lines, app) = (cx.live(&LINES), cx.live(&APP));
    let LogsPage { min, source, all } = *view;
    let core = lines
        .iter()
        .filter(|l| source != Source::App && l.level >= min && (all || !l.internal))
        .map(|l| Row {
            at: l.at,
            level: l.level,
            source: "Core",
            text: &l.text,
            count: 1,
        });
    let own = app
        .iter()
        .filter(|e| source != Source::Core && app_level(e.level) >= min)
        .map(|e| Row {
            at: e.at,
            level: app_level(e.level),
            source: e.source,
            text: &e.text,
            count: e.count,
        });
    let mut shown: Vec<Row> = core.chain(own).collect();
    // Both logs are in time order: a stable sort interleaves them.
    shown.sort_by_key(|r| r.at);
    let counter = kit::count(shown.len(), "line", "lines");
    let mut clear = false;
    cx.page_header(ui, &counter, |ui, _| {
        clear = kit::Button::secondary("Clear")
            .icon(Icon::Delete)
            .enabled(!lines.is_empty() || !app.is_empty())
            .show(ui)
            .clicked();
    });
    if clear {
        cx.run(Clear);
    }
    filters(ui, view);
    // The core's lines from Homeport's work the toggle hides at this level.
    let hidden = lines
        .iter()
        .filter(|l| source != Source::App && l.level >= min && l.internal && !all)
        .count();
    kit::list_card(ui, |ui| {
        if shown.is_empty() {
            let title = match (min, source) {
                _ if lines.is_empty() && app.is_empty() => "No log lines yet",
                (Level::Info, Source::Core) => "No traffic lines",
                (Level::Info, _) => "No log lines",
                (Level::Warning, _) => "No warnings",
                (Level::Error, _) => "No errors",
            };
            view.all |= empty(ui, title, min, hidden);
        } else {
            rows(ui, &shown);
        }
    });
}

fn filters(ui: &mut egui::Ui, view: &mut LogsPage) {
    let levels = [
        (Level::Info, "All"),
        (Level::Warning, "Warnings"),
        (Level::Error, "Errors"),
    ];
    let sources = [
        (Source::All, "App and core"),
        (Source::App, "App"),
        (Source::Core, "Core"),
    ];
    // What does not fit side by side goes to the next line.
    ui.horizontal_wrapped(|ui| {
        kit::segmented(ui, &mut view.min, &levels);
        ui.add_space(theme::SPACE_L);
        kit::segmented(ui, &mut view.source, &sources);
        if view.source != Source::App {
            ui.add_space(theme::SPACE_L);
            kit::toggle(ui, &mut view.all, "Show tests and reloads")
                .on_hover_text("The core's lines from server tests and configuration reloads");
        }
    });
    ui.add_space(theme::SPACE_M);
}

/// The empty card; true when "Show them" asks for the hidden lines.
fn empty(ui: &mut egui::Ui, title: &str, min: Level, hidden: usize) -> bool {
    let next = match hidden {
        0 if min > Level::Info => "Problems appear here when they happen.".to_owned(),
        0 => "New lines appear here as Homeport works.".to_owned(),
        n => format!(
            "{} from server tests and reloads hidden.",
            kit::plural(n, "line", "lines")
        ),
    };
    kit::empty_with(ui, Icon::Logs, title, &next);
    if hidden == 0 {
        return false;
    }
    let show = ui
        .vertical_centered(|ui| kit::Button::secondary("Show them").show(ui).clicked())
        .inner;
    ui.add_space(theme::SPACE_XL);
    show
}

/// Shared by every line: one font, the widths of the age and source columns.
struct Columns {
    font: egui::FontId,
    age: f32,
    source: f32,
    /// Too narrow for three columns: the age and source go above the text.
    stacked: bool,
}

fn rows(ui: &mut egui::Ui, shown: &[Row]) {
    let pal = kit::colors(ui);
    let font = egui::FontId::proportional(theme::SMALL);
    let width = |ui: &mut egui::Ui, text: &str| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(text.into(), font.clone(), pal.muted)
                .size()
                .x
        })
    };
    // The widest age and source, so every message starts at the same edge.
    const SOURCE_MAX: f32 = 140.;
    const MIN_TEXT: f32 = 320.;
    let age = width(ui, "59\u{a0}min ago") + theme::SPACE_S;
    let source = shown.iter().map(|r| width(ui, r.source)).fold(0., f32::max);
    let source = source.min(SOURCE_MAX) + theme::SPACE_M;
    let columns = Columns {
        stacked: ui.available_width() - age - source < MIN_TEXT,
        font,
        age,
        source,
    };
    // The scroll edge keeps an inset from the card's border.
    let inset = theme::INSET - theme::SPACE_S;
    egui::Frame::NONE
        .inner_margin(egui::Margin::symmetric(0, theme::SPACE_S as i8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("log-lines")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(inset as i8, 0))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = theme::SPACE_XS;
                            // An age shows once for its run of lines, and again on
                            // the first visible line when its run starts above.
                            let (mut last, mut seen) = (String::new(), false);
                            for r in shown {
                                let age = kit::age(r.at);
                                // Mostly in view: a line cut at the top keeps its age.
                                let visible =
                                    ui.cursor().top() + theme::SMALL / 2. >= ui.clip_rect().top();
                                let repeated = age == last && (seen || !visible);
                                seen |= visible;
                                last.clone_from(&age);
                                row(ui, &columns, r, (!repeated).then_some(age));
                            }
                        });
                });
        });
}

/// One line: age (unless it repeats the line above), source, text and repeats.
fn row(ui: &mut egui::Ui, c: &Columns, r: &Row, age: Option<String>) {
    let pal = kit::colors(ui);
    let ink = match r.level {
        Level::Info => pal.foreground,
        Level::Warning => pal.warning,
        Level::Error => pal.danger,
    };
    let muted = |text: &str| {
        egui::RichText::new(text)
            .font(c.font.clone())
            .color(pal.muted)
    };
    // Words wrap whole; one longer than the line breaks anywhere.
    let text = |width: f32, prefix: bool| {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = width;
        let format = |color| egui::TextFormat::simple(c.font.clone(), color);
        if prefix {
            job.append(&format!("{}  ", r.source), 0., format(pal.muted));
        }
        job.append(r.text, 0., format(ink));
        if r.count > 1 {
            job.append(&format!("  ×{}", r.count), 0., format(pal.muted));
        }
        job
    };
    if c.stacked {
        // The age on its own line gives the text the full width.
        if let Some(age) = age {
            ui.label(muted(&age));
        }
        kit::text_label(ui, text(ui.available_width(), true), true, false);
        ui.add_space(theme::SPACE_S);
        return;
    }
    // Age and source on the left in the same type, so all share the first
    // line; wrapped text keeps its own column.
    let column = |ui: &mut egui::Ui, width: f32, add: &dyn Fn(&mut egui::Ui)| {
        ui.allocate_ui_with_layout(
            egui::vec2(width, 0.),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(width);
                add(ui);
            },
        );
    };
    ui.horizontal_top(|ui| {
        column(ui, c.age, &|ui| {
            ui.label(muted(age.as_deref().unwrap_or_default()));
        });
        column(ui, c.source, &|ui| {
            ui.add(egui::Label::new(muted(r.source)).truncate());
        });
        kit::text_label(ui, text(ui.available_width(), false), true, false);
    });
}
