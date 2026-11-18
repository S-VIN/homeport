//! Icons from the Phosphor icon font (regular and fill weights, MIT), drawn as text
//! glyphs so they stay crisp at any scale. Plugins reuse these or pass their own
//! painter function via [`Icon::Custom`].
use egui::{self, Align2, Color32, FontFamily, FontId, Painter, Pos2};

/// Font family names registered by [`crate::theme::install_fonts`].
pub const REGULAR: &str = "Phosphor";
pub const FILL: &str = "Phosphor Fill";
/// Default icon size in points.
pub const SIZE: f32 = 16.0;

#[derive(Clone, Copy, Debug)]
pub enum Icon {
    /// A plugin-supplied icon painter: `(painter, center, ink)`.
    Custom(fn(&Painter, Pos2, Color32)),
    More,
    Activity,
    /// Background tasks (an hourglass).
    Tasks,
    Logs,
    Filter,
    Auto,
    Info,
    Warning,
    Error,
    Plugin,
    Check,
    /// Filled check in a circle: a positive state.
    Connected,
    Globe,
    Add,
    ChevronRight,
    ChevronDown,
    ChevronUp,
    Delete,
    Edit,
    Up,
    Down,
    Close,
    Refresh,
    Stop,
    Copy,
    Lock,
    Unlock,
    Open,
    Disconnect,
    Servers,
    Subscriptions,
    Routing,
    Checks,
    Listeners,
    Settings,
    Exit,
    Shield,
    QrCode,
    Key,
    Users,
    Link,
    Eye,
    EyeOff,
    /// An empty circle (something off or not chosen).
    Circle,
    /// A command prompt: work on a remote machine.
    Terminal,
    /// Bars: amounts over time.
    Statistics,
    /// A broom: take away everything an installation added.
    Uninstall,
}

impl Icon {
    /// `(codepoint, filled)`.
    fn glyph(self) -> Option<(char, bool)> {
        Some(match self {
            Icon::Custom(_) => return None,
            Icon::More => ('\u{E1FE}', false),
            Icon::Activity => ('\u{E000}', false),
            Icon::Tasks => ('\u{E2B6}', false),
            Icon::Logs => ('\u{E2F4}', false),
            Icon::Filter => ('\u{E268}', false),
            Icon::Auto => ('\u{E6B6}', false),
            Icon::Info => ('\u{E2CE}', false),
            Icon::Warning => ('\u{E4E0}', true),
            Icon::Error => ('\u{E4E2}', true),
            Icon::Plugin => ('\u{E596}', false),
            Icon::Check => ('\u{E182}', false),
            Icon::Connected => ('\u{E184}', true),
            Icon::Globe => ('\u{E288}', false),
            Icon::Add => ('\u{E3D4}', false),
            Icon::ChevronRight => ('\u{E13A}', false),
            Icon::ChevronDown => ('\u{E136}', false),
            Icon::ChevronUp => ('\u{E13C}', false),
            Icon::Delete => ('\u{E4A6}', false),
            Icon::Edit => ('\u{E3B4}', false),
            Icon::Up => ('\u{E08E}', false),
            Icon::Down => ('\u{E03E}', false),
            Icon::Close => ('\u{E4F6}', false),
            Icon::Refresh => ('\u{E036}', false),
            Icon::Stop => ('\u{E46C}', true),
            Icon::Copy => ('\u{E1CA}', false),
            Icon::Lock => ('\u{E2FA}', false),
            Icon::Unlock => ('\u{E306}', false),
            Icon::Open => ('\u{E06C}', false),
            Icon::Disconnect => ('\u{E2E4}', false),
            Icon::Servers => ('\u{E2A0}', false),
            Icon::Subscriptions => ('\u{E402}', false),
            Icon::Routing => ('\u{ED3C}', false),
            Icon::Checks => ('\u{E628}', false),
            Icon::Listeners => ('\u{EB56}', false),
            Icon::Settings => ('\u{E272}', false),
            Icon::Exit => ('\u{E42A}', false),
            Icon::Shield => ('\u{E40C}', false),
            Icon::QrCode => ('\u{E3E6}', false),
            Icon::Key => ('\u{E2D6}', false),
            Icon::Users => ('\u{E4D6}', false),
            Icon::Link => ('\u{E2E2}', false),
            Icon::Eye => ('\u{E220}', false),
            Icon::EyeOff => ('\u{E224}', false),
            Icon::Circle => ('\u{E18A}', false),
            Icon::Terminal => ('\u{E47E}', false),
            Icon::Statistics => ('\u{E150}', false),
            Icon::Uninstall => ('\u{EC54}', false),
        })
    }
}

/// Font of an icon glyph at `size`.
pub fn font(size: f32, filled: bool) -> FontId {
    FontId::new(
        size,
        FontFamily::Name(if filled { FILL } else { REGULAR }.into()),
    )
}

/// Paint `icon` centered on `at` at the default size.
pub fn paint(p: &Painter, at: Pos2, icon: Icon, ink: Color32) {
    paint_sized(p, at, icon, SIZE, ink);
}

/// Paint `icon` centered on `at`, `size` points tall.
pub fn paint_sized(p: &Painter, at: Pos2, icon: Icon, size: f32, ink: Color32) {
    match icon.glyph() {
        None => {
            if let Icon::Custom(f) = icon {
                f(p, at, ink);
            }
        }
        Some((c, filled)) => {
            // Phosphor glyphs fill the em square; the row height adds ascent/descent
            // padding, so center on the galley's own rect.
            let galley = p.layout_no_wrap(c.to_string(), font(size, filled), ink);
            let rect = Align2::CENTER_CENTER.anchor_size(at, galley.size());
            p.galley(rect.min, galley, ink);
        }
    }
}
