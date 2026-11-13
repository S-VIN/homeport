//! rproxy UI kit: design tokens, palette, fonts, icons and domain-free egui widgets.
//!
//! The kit knows nothing about proxies or plugins. The kernel builds its shell and
//! plugin helpers (list editors, settings bound to state) on top of it.
pub mod emoji;
pub mod icons;
pub mod palette;
pub mod theme;
pub mod widgets;

pub use egui;
pub use icons::Icon;
pub use palette::Palette;
pub use theme::palette as colors;
pub use widgets::*;
