//! Domain-free widgets. Every function draws with tokens from [`crate::theme`]
//! and the palette, so plugin screens look identical without custom styling.
//!
//! Page anatomy (top to bottom): [`page_header`] (title · counter … actions),
//! optional [`add_bar`], then [`card`]s or a
//! [`list_card`] whose rows come from [`row`].
pub use crate::icons::Icon;
pub use crate::palette::Tone;
pub(crate) use crate::theme::{self, CONTROL, INSET, RADIUS, RADIUS_CONTROL};
pub(crate) use egui::{pos2, vec2, Align, Color32, FontId, Layout, Rect, Response, RichText, Sense, Stroke, Ui};
pub(crate) use std::fmt::Display;

mod text;
mod icon_buttons;
mod buttons;
mod inputs;
mod page;
mod settings;
mod pieces;
mod properties;
mod format;

pub use text::*;
pub use icon_buttons::*;
pub use buttons::*;
pub use inputs::*;
pub use page::*;
pub use settings::*;
pub use pieces::*;
pub use properties::*;
pub use format::*;
