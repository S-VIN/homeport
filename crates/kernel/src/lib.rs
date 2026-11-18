//! # homeport kernel
//!
//! The kernel holds no proxy features. It provides the mechanisms plugins are built from:
//!
//! | Concept | What a plugin does with it |
//! |---|---|
//! | [`State`] | Declares its data: a persisted list, collection or setting, or memory-only state |
//! | [`Op`] | Declares actions; implements, wraps or replaces other plugins' actions |
//! | Events | Observes facts after a commit ([`Setup::on`]) |
//! | [`Slot`] / [`Hook`] | Adds UI to a zone of the window or to another plugin's screen; answers sync questions |
//!
//! Around them the kernel supplies a single-writer transaction pipeline, SQLite
//! persistence, background jobs and schedules, the mihomo core (typed config
//! fragments and a controller API) and the window shell with its UI zones.
//! Plugin code never sees a client, a server or a protocol: the same egui code
//! draws the desktop window and every browser session.
// Stored plugin callbacks are boxed closures; their full types are the API.
#![allow(clippy::type_complexity)]
extern crate self as homeport_kernel;

pub mod board;
mod changes;
pub mod columns;
pub mod configuration;
pub mod core;
mod host;
mod id;
pub mod jobs;
mod kernel;
pub mod list;
pub mod logging;
mod op;
mod plugin;
mod setup;
pub mod shell;
mod slot;
mod state;
pub mod store;
pub mod ui;
pub mod updates;

pub use board::{Phase, Run, RunId};
pub use columns::Columns;
pub use homeport_derive::Table;
pub use homeport_kit as kit;
pub use homeport_kit::egui;
pub use homeport_mihomo as mihomo;
pub use host::{Host, Session};
pub use id::{Id, hex, now, random_bytes};
pub use kernel::{Cx, Guard, Input, Kernel, LiveMark, Notice, NoticeAction, Started};
pub use log;
pub use op::{Claim, JobSpec, Next, Op, Rank, Repeat};
pub use plugin::{Category, Platform, Plugin, categories};
pub use setup::Setup;
pub use slot::{Hook, Slot};
pub use state::{Changed, Record, Snapshot, State, Write};
pub use ui::{Page, PageSpec, Request, UiCx};

pub type Result<T> = std::result::Result<T, Error>;

/// Replace the value of a setting registered with [`Setup::editable_setting`].
pub struct Set<T: 'static> {
    pub state: &'static State<T>,
    pub value: T,
}
impl<T: Send + Sync + 'static> Op for Set<T> {
    type Reply = ();
    const SAVES: bool = true;
}
/// Change some fields of a setting registered with [`Setup::editable_setting`]
/// (other fields keep their current values). See [`UiCx::update`].
pub struct Update<T: 'static> {
    pub state: &'static State<T>,
    pub change: Box<dyn FnOnce(&mut T) + Send>,
}
impl<T: Send + Sync + 'static> Op for Update<T> {
    type Reply = ();
    const SAVES: bool = true;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: String,
    pub message: String,
}
impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    /// A user-facing failure.
    pub fn msg(message: impl Into<String>) -> Self {
        Self::new("error", message)
    }
    /// The work was dropped on purpose (stopped, or made stale meanwhile):
    /// the caller shows no error toast.
    pub fn cancel(message: impl Into<String>) -> Self {
        Self::new("cancelled", message)
    }
    pub fn cancelled(&self) -> bool {
        self.code == "cancelled"
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}
impl From<String> for Error {
    fn from(m: String) -> Self {
        Self::msg(m)
    }
}
impl From<&str> for Error {
    fn from(m: &str) -> Self {
        Self::msg(m)
    }
}
/// For custom persistence: `sql.execute(..)?`.
impl From<store::rusqlite::Error> for Error {
    fn from(e: store::rusqlite::Error) -> Self {
        Self::new("storage", e.to_string())
    }
}

/// The `*.sql` files of the crate's `migrations/` folder in name order, for
/// [`Plugin::migrations`]. The crate's `build.rs` is
/// `include!("../../crates/kernel/build/migrations.rs");`.
#[macro_export]
macro_rules! migrations {
    () => {
        include!(concat!(env!("OUT_DIR"), "/migrations.rs"))
    };
}

/// `bail!("text {x}")` returns a user-facing error.
#[macro_export]
macro_rules! bail {
    ($($t:tt)*) => { return Err($crate::Error::msg(format!($($t)*))) };
}

/// Everything a plugin usually needs.
pub mod prelude {
    pub use crate::kit::{self, Icon, egui};
    pub use crate::list::{Add, Added, Edit, Move, Remove, Removed, Save, Saved};
    pub use crate::mihomo::{self, Fragment, Rule as CoreRule, Stage, Target};
    pub use crate::shell::{PANELS, SETTINGS_SECTIONS, STATUS};
    pub use crate::{
        Category, Changed, Claim, Columns, Cx, Error, Hook, Id, JobSpec, Next, Op, Page, PageSpec,
        Platform, Plugin, Rank, Record, Request, Result, Set, Setup, Slot, Snapshot, Started,
        State, Table, UiCx, Update, Write, bail, categories, choices, hex, now, random_bytes,
    };
    pub use log::{debug, error, info, warn};
}
