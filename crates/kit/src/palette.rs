//! Application colors. Two themes with the same roles; widgets never pick raw colors.
//!
//! Surfaces: `background` is the window canvas, `surface` the raised card on it,
//! `hover` a row under the pointer, `subtle` the fill of secondary controls.
//! Text: `foreground` > `muted` > `faint`. Status colors are for meaning only.
use egui::Color32;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub background: Color32,
    pub surface: Color32,
    pub hover: Color32,
    pub subtle: Color32,
    pub foreground: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub border: Color32,
    pub control_border: Color32,
    pub accent: Color32,
    pub action_bg: Color32,
    pub action_hover: Color32,
    pub action_fg: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub shadow: Color32,
}

impl Palette {
    /// `color` mixed into the card surface: soft fills for selections and tones.
    pub fn tint(self, color: Color32, amount: f32) -> Color32 {
        self.surface.lerp_to_gamma(color, amount)
    }
    /// Selected row / active tab fill.
    pub fn selected(self) -> Color32 {
        self.tint(self.accent, if self.dark { 0.16 } else { 0.09 })
    }
    /// Soft background for a status color (badges, banners).
    pub fn soft(self, color: Color32) -> Color32 {
        self.tint(color, if self.dark { 0.18 } else { 0.11 })
    }
    pub fn tone(self, tone: Tone) -> Color32 {
        match tone {
            Tone::Neutral => self.muted,
            Tone::Accent => self.accent,
            Tone::Success => self.success,
            Tone::Warning => self.warning,
            Tone::Danger => self.danger,
        }
    }
    /// Ink and fill of a categorical tag (e.g. a subscription), `hue` wraps around [`TAG_HUES`].
    pub fn tag(self, hue: usize) -> (Color32, Color32) {
        let (light, dark) = TAGS[hue % TAGS.len()];
        let (ink, fill) = if self.dark { dark } else { light };
        (Color32::from_rgb(ink.0, ink.1, ink.2), Color32::from_rgb(fill.0, fill.1, fill.2))
    }
}

/// Meaning of a piece of status text or a badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
}

type Rgb = (u8, u8, u8);
/// Categorical tag colors: (light ink, light fill), (dark ink, dark fill).
/// Ink on fill keeps ≥ 4.5:1 contrast in both themes.
const TAGS: [((Rgb, Rgb), (Rgb, Rgb)); 8] = [
    (((29, 78, 216), (226, 234, 253)), ((156, 185, 252), (30, 44, 76))),
    (((109, 40, 217), (238, 232, 253)), ((198, 174, 252), (47, 35, 78))),
    (((21, 121, 64), (219, 244, 228)), ((118, 216, 156), (22, 56, 39))),
    (((150, 84, 0), (253, 241, 205)), ((236, 190, 98), (60, 46, 18))),
    (((180, 25, 92), (252, 231, 241)), ((244, 150, 190), (70, 28, 47))),
    (((14, 112, 105), (213, 244, 239)), ((104, 212, 198), (18, 55, 53))),
    (((184, 64, 14), (255, 236, 220)), ((248, 166, 112), (68, 37, 20))),
    (((77, 116, 14), (234, 244, 214)), ((168, 212, 98), (38, 52, 20))),
];
/// Number of distinct tag colors.
pub const TAG_HUES: usize = TAGS.len();

pub const LIGHT: Palette = Palette {
    dark: false,
    background: Color32::from_rgb(243, 244, 247),
    surface: Color32::from_rgb(255, 255, 255),
    hover: Color32::from_rgb(246, 247, 249),
    subtle: Color32::from_rgb(236, 238, 242),
    foreground: Color32::from_rgb(28, 33, 42),
    muted: Color32::from_rgb(92, 101, 116),
    faint: Color32::from_rgb(140, 148, 161),
    border: Color32::from_rgb(225, 228, 234),
    control_border: Color32::from_rgb(204, 209, 217),
    accent: Color32::from_rgb(44, 96, 214),
    action_bg: Color32::from_rgb(44, 96, 214),
    action_hover: Color32::from_rgb(33, 78, 182),
    action_fg: Color32::from_rgb(255, 255, 255),
    success: Color32::from_rgb(22, 122, 76),
    warning: Color32::from_rgb(160, 92, 0),
    danger: Color32::from_rgb(196, 36, 56),
    shadow: Color32::from_black_alpha(28),
};
pub const DARK: Palette = Palette {
    dark: true,
    background: Color32::from_rgb(17, 19, 23),
    surface: Color32::from_rgb(26, 29, 35),
    hover: Color32::from_rgb(33, 37, 44),
    subtle: Color32::from_rgb(38, 42, 50),
    foreground: Color32::from_rgb(230, 233, 239),
    muted: Color32::from_rgb(155, 163, 178),
    faint: Color32::from_rgb(108, 116, 130),
    border: Color32::from_rgb(43, 48, 57),
    control_border: Color32::from_rgb(62, 68, 80),
    accent: Color32::from_rgb(124, 163, 247),
    action_bg: Color32::from_rgb(58, 105, 222),
    action_hover: Color32::from_rgb(75, 122, 236),
    action_fg: Color32::from_rgb(255, 255, 255),
    success: Color32::from_rgb(86, 200, 140),
    warning: Color32::from_rgb(232, 172, 80),
    danger: Color32::from_rgb(242, 128, 138),
    shadow: Color32::from_black_alpha(90),
};
