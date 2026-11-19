//! The app's log. Plugins and the kernel write with the `log` macros
//! (`info!`, `warn!`, `error!`, `debug!` in the prelude); the line is tagged
//! with the plugin whose crate wrote it. The kernel keeps the last lines for
//! the Logs page ([`recent`], [`changes`]) and prints them to stderr.
//!
//! A line says what changed or what a finished piece of work found, in the
//! user's words: "Main: 42 servers (3 new)", not one line per tick, item or
//! retry. Equal lines in a row fold into one with a count. `debug!` is for
//! developers and shows only with `HOMEPORT_LOG=debug`.
use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex, RwLock};

pub use ::log::Level;

/// Lines kept in memory; older ones drop off.
const KEEP: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// Grows by one per new line (not when an equal line folds into it).
    pub seq: u64,
    /// Unix seconds of the last time it was written.
    pub at: u64,
    pub level: Level,
    /// The writing plugin's name, or "Homeport" for the app itself.
    pub source: &'static str,
    pub text: String,
    /// Times written in a row.
    pub count: u32,
}

/// The kept lines, oldest first.
pub fn recent() -> Vec<Entry> {
    BOOK.lock().unwrap().lines.iter().cloned().collect()
}
/// Changes whenever a line is written.
pub fn changes() -> tokio::sync::watch::Receiver<()> {
    CHANGES.subscribe()
}
/// The `target:` that tags a line with a plugin, for code that writes on a
/// plugin's behalf: `log::warn!(target: &logging::target(id), "…")`.
pub fn target(plugin: &str) -> String {
    format!("plugin_{}", plugin.replace('-', "_"))
}

/// Install the logger (once per process; later calls only add names).
pub(crate) fn init(plugins: &[&'static crate::Plugin]) {
    let mut names = NAMES.write().unwrap();
    for p in plugins {
        names.insert(target(p.id), p.name);
    }
    drop(names);
    if ::log::set_logger(&Logger).is_ok() {
        let debug = std::env::var("HOMEPORT_LOG").is_ok_and(|v| v == "debug");
        ::log::set_max_level(if debug {
            ::log::LevelFilter::Debug
        } else {
            ::log::LevelFilter::Info
        });
    }
}

struct Book {
    lines: VecDeque<Entry>,
    seq: u64,
}
static BOOK: Mutex<Book> = Mutex::new(Book {
    lines: VecDeque::new(),
    seq: 0,
});
static CHANGES: LazyLock<tokio::sync::watch::Sender<()>> =
    LazyLock::new(|| tokio::sync::watch::Sender::new(()));
/// Plugin names by crate name (`plugin_url_tests`).
static NAMES: LazyLock<RwLock<HashMap<String, &'static str>>> = LazyLock::new(Default::default);

struct Logger;
impl ::log::Log for Logger {
    /// Only homeport's crates: libraries' chatter is not the user's business.
    fn enabled(&self, m: &::log::Metadata) -> bool {
        m.level() <= ::log::max_level()
            && (m.target().starts_with("homeport") || m.target().starts_with("plugin_"))
    }
    fn log(&self, record: &::log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let krate = record.target().split("::").next().unwrap_or_default();
        let source = NAMES.read().unwrap().get(krate).copied();
        let (source, text) = (source.unwrap_or("Homeport"), record.args().to_string());
        let (level, at) = (record.level(), crate::now());
        let mut book = BOOK.lock().unwrap();
        match book.lines.back_mut() {
            Some(last) if last.level == level && last.source == source && last.text == text => {
                last.count += 1;
                last.at = at;
            }
            _ => {
                eprintln!("{} {level:<5} {source}: {text}", clock(at));
                book.seq += 1;
                let seq = book.seq;
                book.lines.push_back(Entry {
                    seq,
                    at,
                    level,
                    source,
                    text,
                    count: 1,
                });
                if book.lines.len() > KEEP {
                    book.lines.pop_front();
                }
            }
        }
        drop(book);
        CHANGES.send_replace(());
    }
    fn flush(&self) {}
}

/// `HH:MM:SS` in UTC.
fn clock(at: u64) -> String {
    let s = at % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_name_their_plugin_and_repeats_fold() {
        static P: crate::Plugin = crate::Plugin {
            id: "fold-test",
            name: "Fold test",
            ..crate::Plugin::DEFAULT
        };
        init(&[&P]);
        let target = target(P.id);
        for _ in 0..3 {
            ::log::warn!(target: &target, "Same thing");
        }
        ::log::warn!(target: "some_library", "Not ours");
        let lines = recent();
        let last = lines.iter().rfind(|e| e.source == "Fold test").unwrap();
        assert_eq!(
            (last.text.as_str(), last.count, last.level),
            ("Same thing", 3, Level::Warn)
        );
        assert!(!lines.iter().any(|e| e.text == "Not ours"));
    }
}
