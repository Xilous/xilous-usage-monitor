//! Themes for the floating widget.
//!
//! A theme is nine colour roles: background, text, one colour per usage
//! window (5h, 7d, Fable), one per device ring (CPU, RAM), and the two warning
//! colours a window turns as it runs ahead of its pace. The muted text, the
//! tracks, the pace ticks and the hairlines are derived from the background
//! and the text, so no theme can pair a track that vanishes with its
//! background. Three modes sit on top of the roles: how the windows are
//! coloured, how the warnings are coloured, and what the widget stands on.
use crate::native_interop::Color;
use serde::{Deserialize, Deserializer, Serialize};

/// How the usage windows are told apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowColours {
    /// One colour (the 7d one), deeper for each ring further in.
    Shaded,
    /// Grey everywhere; colour only when a window runs ahead of its pace.
    Mono,
    /// Each window in its own colour.
    #[default]
    #[serde(other)]
    Own,
}

/// How a window running ahead of its pace is marked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningColours {
    /// A yellow and magenta pair that stays distinct with colour blindness.
    ColourBlind,
    /// No colour change; the hatching past the pace notch carries it alone.
    Hatching,
    /// The theme's own over-pace and far-over colours.
    #[default]
    #[serde(other)]
    Theme,
}

/// What the widget stands on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Material {
    /// No backing at all collapsed: the rings sit straight on the desktop.
    Floating,
    /// A translucent pill collapsed, an opaque card expanded.
    #[default]
    #[serde(other)]
    Solid,
}

/// The nine colour roles, as 0xRRGGBB. `ram` is optional: without it the RAM
/// ring is a deeper shade of the CPU one, so the two never read as one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roles {
    #[serde(with = "hex")]
    pub background: u32,
    #[serde(with = "hex")]
    pub text: u32,
    #[serde(with = "hex")]
    pub session: u32,
    #[serde(with = "hex")]
    pub weekly: u32,
    #[serde(with = "hex")]
    pub fable: u32,
    #[serde(with = "hex")]
    pub cpu: u32,
    #[serde(with = "hex_opt", default, skip_serializing_if = "Option::is_none")]
    pub ram: Option<u32>,
    #[serde(with = "hex")]
    pub over_pace: u32,
    #[serde(with = "hex")]
    pub far_over: u32,
}

/// The roles in the order the appearance studio lists them.
pub const ROLE_NAMES: [&str; 9] = [
    "Background",
    "Text",
    "5h",
    "7d",
    "Fable",
    "CPU",
    "RAM",
    "Over pace",
    "Far over",
];

impl Roles {
    const fn new(c: [u32; 9]) -> Self {
        Self {
            background: c[0],
            text: c[1],
            session: c[2],
            weekly: c[3],
            fable: c[4],
            cpu: c[5],
            ram: Some(c[6]),
            over_pace: c[7],
            far_over: c[8],
        }
    }

    /// The RAM colour, given or derived.
    pub fn ram(&self) -> u32 {
        self.ram.unwrap_or_else(|| {
            if is_dark_rgb(self.background) {
                mix(self.cpu, self.background, 0.35)
            } else {
                mix(self.cpu, 0x000000, 0.3)
            }
        })
    }

    /// Role `i` in ROLE_NAMES order.
    pub fn get(&self, i: usize) -> u32 {
        match i {
            0 => self.background,
            1 => self.text,
            2 => self.session,
            3 => self.weekly,
            4 => self.fable,
            5 => self.cpu,
            6 => self.ram(),
            7 => self.over_pace,
            _ => self.far_over,
        }
    }

    pub fn set(&mut self, i: usize, rgb: u32) {
        let rgb = rgb & 0xFF_FFFF;
        match i {
            0 => self.background = rgb,
            1 => self.text = rgb,
            2 => self.session = rgb,
            3 => self.weekly = rgb,
            4 => self.fable = rgb,
            5 => self.cpu = rgb,
            6 => self.ram = Some(rgb),
            7 => self.over_pace = rgb,
            _ => self.far_over = rgb,
        }
    }
}

pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub roles: Roles,
    pub window_colours: WindowColours,
}

pub const SYSTEM: &str = "system";
pub const CUSTOM: &str = "custom";

/// Background, text, 5h, 7d, Fable, CPU, RAM, over pace, far over. The
/// warnings step outside each theme's own hue family wherever its window
/// colours are warm, so a warning never reads as one more window colour.
pub const PRESETS: [Preset; 10] = [
    Preset {
        id: "graphite",
        name: "Graphite",
        roles: Roles::new([
            0x1c1c1e, 0xeceff2, 0x22d3ee, 0x38a3f5, 0xa78bfa, 0x34d399, 0x2c936e, 0xfbbf24,
            0xf87171,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "paper",
        name: "Paper",
        roles: Roles::new([
            0xf7f7f8, 0x1d2125, 0x0891b2, 0x1d6fd8, 0x7c3aed, 0x059669, 0x04694a, 0xb45309,
            0xdc2626,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "midnight",
        name: "Midnight",
        roles: Roles::new([
            0x151923, 0xe7edf7, 0xf6c9a8, 0xf0aa87, 0xa6b9ff, 0x74d8d3, 0x4f8f97, 0xffd27a,
            0xff8a8a,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "porcelain",
        name: "Porcelain",
        roles: Roles::new([
            0xf6f3ed, 0x292d36, 0xb8683f, 0x9e4d32, 0x3859a8, 0x226c65, 0x174c47, 0xa16207,
            0xb91c1c,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "evergreen",
        name: "Evergreen",
        roles: Roles::new([
            0x122522, 0xe0eee6, 0xf2d2ae, 0xeac098, 0x8fcada, 0x88d9b4, 0x5f9e84, 0xf5c451,
            0xf59090,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "afterglow",
        name: "Afterglow",
        roles: Roles::new([
            0x281d2c, 0xf8e5ee, 0xffd0b0, 0xffb98f, 0xd3afff, 0xf19abf, 0xa76c8a, 0xffd166,
            0xff7a90,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "black_and_gold",
        name: "Black & gold",
        roles: Roles::new([
            0x0b0b0c, 0xf3ead2, 0xf7d774, 0xd4af37, 0xb07a3b, 0xe5e4e2, 0x7d8c99, 0xff5a4e,
            0xe0245e,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "red_and_white",
        name: "Red & white",
        roles: Roles::new([
            0xffffff, 0x1a1a1a, 0xf04a55, 0xd7192a, 0x8e0e1b, 0x6b6b6b, 0x3d3d3d, 0xa86f00,
            0x111827,
        ]),
        window_colours: WindowColours::Own,
    },
    Preset {
        id: "mono",
        name: "Mono",
        roles: Roles::new([
            0x18181a, 0xf1f1f1, 0xd0d0d0, 0xd0d0d0, 0xd0d0d0, 0xd0d0d0, 0x8a8a8a, 0xfbbf24,
            0xf87171,
        ]),
        window_colours: WindowColours::Mono,
    },
    Preset {
        id: "high_contrast",
        name: "High contrast",
        roles: Roles::new([
            0x000000, 0xffffff, 0x00e5ff, 0x4da3ff, 0xc79bff, 0x3dff9e, 0x1fa866, 0xffd400,
            0xff5c5c,
        ]),
        window_colours: WindowColours::Own,
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The presets the System theme follows.
fn system_roles(dark: bool) -> Roles {
    PRESETS[if dark { 0 } else { 1 }].roles
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Appearance {
    /// "system", a preset id, or "custom" for `roles`.
    pub theme: String,
    /// The custom theme's colours, kept while a preset is in use so switching
    /// back to Custom restores them.
    pub roles: Roles,
    pub window_colours: WindowColours,
    pub warning_colours: WarningColours,
    pub material: Material,
    /// WIDGET SIZE as a whole percent of the default size, on top of DPI.
    pub widget_size: u32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: SYSTEM.to_owned(),
            roles: system_roles(true),
            window_colours: WindowColours::Own,
            warning_colours: WarningColours::Theme,
            material: Material::Solid,
            widget_size: WIDGET_SIZE_DEFAULT,
        }
    }
}

/// The settings file as any version wrote it. Before themes, the appearance
/// was a `mode` (system, light, dark or custom) and five `colors`:
/// background, text, Claude, Codex and Antigravity.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Stored {
    theme: Option<String>,
    roles: Option<Roles>,
    window_colours: WindowColours,
    warning_colours: WarningColours,
    material: Material,
    #[serde(deserialize_with = "deserialize_widget_size")]
    widget_size: Option<u32>,
    mode: Option<LegacyMode>,
    colors: Option<[u32; 5]>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum LegacyMode {
    Light,
    Dark,
    Custom,
    #[serde(other)]
    System,
}

/// The four palettes the old appearance studio offered, by their colours.
const LEGACY_PRESETS: [(&str, [u32; 5]); 4] = [
    (
        "midnight",
        [0x151923, 0xe7edf7, 0xf0aa87, 0xa6b9ff, 0x74d8d3],
    ),
    (
        "porcelain",
        [0xf6f3ed, 0x292d36, 0x9e4d32, 0x3859a8, 0x226c65],
    ),
    (
        "evergreen",
        [0x122522, 0xe0eee6, 0xeac098, 0x88d9b4, 0x8fcada],
    ),
    (
        "afterglow",
        [0x281d2c, 0xf8e5ee, 0xffb98f, 0xd3afff, 0xf19abf],
    ),
];

impl From<Stored> for Appearance {
    fn from(s: Stored) -> Self {
        let mut a = Appearance {
            widget_size: s.widget_size.unwrap_or(WIDGET_SIZE_DEFAULT),
            window_colours: s.window_colours,
            warning_colours: s.warning_colours,
            material: s.material,
            ..Appearance::default()
        };
        if let Some(roles) = s.roles {
            a.roles = roles;
        }
        if let Some(theme) = s.theme {
            a.theme = theme;
            return a;
        }
        // A settings file from before themes.
        a.theme = match s.mode {
            Some(LegacyMode::Light) => "paper".to_owned(),
            Some(LegacyMode::Dark) => "graphite".to_owned(),
            Some(LegacyMode::Custom) => {
                let colors = s.colors.unwrap_or(LEGACY_PRESETS[0].1);
                match LEGACY_PRESETS.iter().find(|(_, c)| *c == colors) {
                    Some((id, _)) => (*id).to_owned(),
                    None => {
                        // A palette of the user's own: one accent, shaded, as
                        // it was drawn before.
                        let warm = system_roles(is_dark_rgb(colors[0]));
                        a.roles = Roles {
                            background: colors[0],
                            text: colors[1],
                            session: colors[2],
                            weekly: colors[2],
                            fable: colors[2],
                            cpu: colors[4],
                            ram: None,
                            over_pace: warm.over_pace,
                            far_over: warm.far_over,
                        };
                        a.window_colours = WindowColours::Shaded;
                        CUSTOM.to_owned()
                    }
                }
            }
            _ => SYSTEM.to_owned(),
        };
        a
    }
}

impl<'de> Deserialize<'de> for Appearance {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Stored::deserialize(deserializer).map(Appearance::from)
    }
}

pub const WIDGET_SIZE_DEFAULT: u32 = 100;
pub const WIDGET_SIZE_MIN: u32 = 75;
pub const WIDGET_SIZE_MAX: u32 = 200;
pub const WIDGET_SIZE_STEP: u32 = 5;

/// Bring any WIDGET SIZE into range: snapped to the nearest 5% step (112
/// becomes 110, 113 becomes 115, and an exact half such as 112.5 rounds up),
/// then clamped to 75..=200. Not-a-number falls back to 100%.
pub fn snap_widget_size(value: f64) -> u32 {
    if value.is_nan() {
        return WIDGET_SIZE_DEFAULT;
    }
    let step = WIDGET_SIZE_STEP as f64;
    let snapped = (value / step).round() * step;
    snapped.clamp(WIDGET_SIZE_MIN as f64, WIDGET_SIZE_MAX as f64) as u32
}

/// Any JSON number is accepted and snapped, so a hand-edited value such as
/// -10 or 1000 cannot reset the rest of the settings file or break layout.
fn deserialize_widget_size<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u32>, D::Error> {
    Option::<f64>::deserialize(deserializer).map(|v| v.map(snap_widget_size))
}

/// Colours are stored as "#RRGGBB"; a bare number is accepted too.
mod hex {
    use serde::{Deserialize, Deserializer, Serializer};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum In {
        Text(String),
        Number(u32),
    }

    pub fn parse(s: &str) -> Option<u32> {
        let s = s.trim().trim_start_matches('#');
        (s.len() == 6)
            .then(|| u32::from_str_radix(s, 16).ok())
            .flatten()
    }

    pub fn serialize<S: Serializer>(v: &u32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("#{:06X}", v & 0xFF_FFFF))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        match In::deserialize(d)? {
            In::Number(n) => Ok(n & 0xFF_FFFF),
            In::Text(t) => parse(&t).ok_or_else(|| serde::de::Error::custom("expected #RRGGBB")),
        }
    }
}

mod hex_opt {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<u32>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(v) => super::hex::serialize(v, s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        #[derive(Deserialize)]
        struct Wrap(#[serde(with = "super::hex")] u32);
        Option::<Wrap>::deserialize(d).map(|v| v.map(|w| w.0))
    }
}

pub fn color(rgb: u32) -> Color {
    Color::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

#[cfg(test)]
pub fn rgb_of(c: Color) -> u32 {
    (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32
}

pub fn is_dark_rgb(rgb: u32) -> bool {
    let c = color(rgb);
    (299 * c.r as u32 + 587 * c.g as u32 + 114 * c.b as u32) < 128000
}

/// `a` moved toward `b` by `t`.
pub fn mix(a: u32, b: u32, t: f64) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |shift: u32| {
        let (x, y) = (((a >> shift) & 255) as f64, ((b >> shift) & 255) as f64);
        ((x + (y - x) * t).round() as u32) << shift
    };
    ch(16) | ch(8) | ch(0)
}

/// WCAG contrast ratio between two colours.
#[cfg(test)]
pub fn contrast(a: u32, b: u32) -> f64 {
    fn lum(rgb: u32) -> f64 {
        let lin = |c: u32| {
            let c = c as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin((rgb >> 16) & 255) + 0.7152 * lin((rgb >> 8) & 255) + 0.0722 * lin(rgb & 255)
    }
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// Everything the widget is drawn with, worked out from a theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color,
    pub ink: Color,
    pub muted: Color,
    pub track: Color,
    pub tick: Color,
    /// A window with no reading.
    pub off: Color,
    /// What a figure flashes toward when a new reading lands.
    pub flash: Color,
    /// The usage windows: 5h, 7d, Fable.
    pub windows: [Color; 3],
    pub cpu: Color,
    pub ram: Color,
    pub over_pace: Color,
    pub far_over: Color,
    /// Warnings are carried by the hatching alone.
    pub hatch_only: bool,
}

impl Appearance {
    /// The roles in use: a preset's, the custom ones, or for System the dark
    /// or light preset that matches Windows.
    pub fn roles_for(&self, system_dark: bool) -> Roles {
        match self.theme.as_str() {
            CUSTOM => self.roles,
            id => preset(id)
                .map(|p| p.roles)
                .unwrap_or_else(|| system_roles(system_dark)),
        }
    }

    pub fn is_dark(&self, system_dark: bool) -> bool {
        is_dark_rgb(self.roles_for(system_dark).background)
    }

    /// Whether a theme other than System is chosen.
    pub fn is_themed(&self) -> bool {
        self.theme != SYSTEM
    }

    /// The stored WIDGET SIZE, re-snapped in case it was set out of range.
    pub fn clamped_widget_size(&self) -> u32 {
        snap_widget_size(self.widget_size as f64)
    }

    /// Background, text, then the 7d, Fable and CPU colours, for the
    /// taskbar-embedded widget, which predates themes.
    pub fn palette(&self, system_dark: bool) -> [Color; 5] {
        let r = self.roles_for(system_dark);
        [r.background, r.text, r.weekly, r.fable, r.cpu].map(color)
    }

    /// Switch to the preset `id`, or to System, taking its window colouring.
    pub fn use_theme(&mut self, id: &str) {
        self.theme = id.to_owned();
        self.window_colours = preset(id).map_or(WindowColours::Own, |p| p.window_colours);
    }

    /// Set one colour role. A preset or System is copied into the custom
    /// theme first, so the edit starts from what is on screen.
    pub fn set_role(&mut self, system_dark: bool, i: usize, rgb: u32) {
        if self.theme != CUSTOM {
            self.roles = self.roles_for(system_dark);
            self.theme = CUSTOM.to_owned();
        }
        self.roles.set(i, rgb);
    }

    /// The colours to draw with. `session_shown` says whether the 5h window
    /// has a ring, which decides the shades of a shaded theme.
    pub fn resolve(&self, system_dark: bool, session_shown: bool) -> Palette {
        let r = self.roles_for(system_dark);
        let dark = is_dark_rgb(r.background);
        let (bg, text) = (r.background, r.text);
        let (mut over, mut far) = (r.over_pace, r.far_over);
        if self.warning_colours == WarningColours::ColourBlind {
            (over, far) = if dark {
                (0xffb000, 0xff5fa2)
            } else {
                (0x8f5f00, 0xb5136a)
            };
        }
        let mut windows = [r.session, r.weekly, r.fable];
        let (mut cpu, mut ram) = (r.cpu, r.ram());
        match self.window_colours {
            WindowColours::Own => {}
            WindowColours::Shaded => {
                // The outermost shown ring takes the colour, each further in
                // a deeper shade of it.
                let deeper = |depth: usize| {
                    if dark {
                        mix(r.weekly, bg, 0.3 * depth as f64)
                    } else {
                        mix(r.weekly, 0x000000, 0.22 * depth as f64)
                    }
                };
                let first = usize::from(!session_shown);
                for (i, w) in windows.iter_mut().enumerate() {
                    *w = deeper(i.saturating_sub(first));
                }
            }
            WindowColours::Mono => {
                windows = [mix(bg, text, 0.78); 3];
                cpu = mix(bg, text, 0.78);
                ram = mix(bg, text, 0.5);
            }
        }
        Palette {
            bg: color(bg),
            ink: color(text),
            muted: color(mix(bg, text, 0.62)),
            track: color(mix(bg, text, 0.16)),
            tick: color(mix(bg, text, 0.9)),
            off: color(mix(bg, text, 0.38)),
            flash: color(if dark { 0xffffff } else { 0x000000 }),
            windows: windows.map(color),
            cpu: color(cpu),
            ram: color(ram),
            over_pace: color(over),
            far_over: color(far),
            hatch_only: self.warning_colours == WarningColours::Hatching,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_follows_windows_and_presets_do_not() {
        let mut a = Appearance::default();
        assert!(a.is_dark(true));
        assert!(!a.is_dark(false));
        a.use_theme("paper");
        assert!(!a.is_dark(true));
        a.use_theme("graphite");
        assert!(a.is_dark(false));
    }

    #[test]
    fn missing_preferences_default_safely() {
        assert_eq!(
            serde_json::from_str::<Appearance>("{}").unwrap(),
            Appearance::default()
        );
    }

    #[test]
    fn themes_survive_a_round_trip() {
        let mut a = Appearance::default();
        a.set_role(true, 6, 0x123456);
        a.warning_colours = WarningColours::ColourBlind;
        a.material = Material::Floating;
        a.window_colours = WindowColours::Shaded;
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"#123456\""));
        let b: Appearance = serde_json::from_str(&json).unwrap();
        assert_eq!(a, b);
        assert_eq!(b.theme, CUSTOM);
    }

    #[test]
    fn old_settings_move_onto_themes() {
        let old = |json: &str| serde_json::from_str::<Appearance>(json).unwrap();
        assert_eq!(
            old(r#"{"mode":"dark","colors":[1,2,3,4,5]}"#).theme,
            "graphite"
        );
        assert_eq!(old(r#"{"mode":"light"}"#).theme, "paper");
        assert_eq!(old(r#"{"mode":"system"}"#).theme, SYSTEM);
        // The palette this user picked from the old presets keeps its name.
        let midnight = old(
            r#"{"mode":"custom","colors":[1382691,15199735,15772295,10926591,7657683],"widget_size":200}"#,
        );
        assert_eq!(midnight.theme, "midnight");
        assert_eq!(midnight.widget_size, 200);
        // A palette of their own stays theirs, one accent shaded as before.
        let own = old(r#"{"mode":"custom","colors":[0,16777215,16711680,65280,255]}"#);
        assert_eq!(own.theme, CUSTOM);
        assert_eq!(own.roles.weekly, 0xff0000);
        assert_eq!(own.roles.cpu, 0x0000ff);
        assert_eq!(own.window_colours, WindowColours::Shaded);
    }

    #[test]
    fn editing_a_preset_starts_from_its_colours() {
        let mut a = Appearance::default();
        a.use_theme("black_and_gold");
        a.set_role(true, 0, 0x000000);
        assert_eq!(a.theme, CUSTOM);
        assert_eq!(a.roles.weekly, 0xd4af37);
        assert_eq!(a.roles.background, 0x000000);
    }

    #[test]
    fn cpu_and_ram_never_share_a_colour() {
        for p in PRESETS {
            assert_ne!(p.roles.cpu, p.roles.ram(), "{}", p.name);
        }
        let mut r = PRESETS[0].roles;
        r.ram = None;
        assert_ne!(r.cpu, r.ram());
        let mut a = Appearance::default();
        a.use_theme("mono");
        let p = a.resolve(true, false);
        assert_ne!(rgb_of(p.cpu), rgb_of(p.ram));
    }

    #[test]
    fn every_preset_keeps_its_text_legible() {
        for p in PRESETS {
            let r = p.roles;
            assert!(contrast(r.text, r.background) >= 4.5, "{} text", p.name);
            for (name, c) in [("7d", r.weekly), ("Fable", r.fable)] {
                assert!(contrast(c, r.background) >= 4.5, "{} {name}", p.name);
            }
            for (name, c) in [("over pace", r.over_pace), ("far over", r.far_over)] {
                assert!(contrast(c, r.background) >= 3.0, "{} {name}", p.name);
            }
        }
    }

    #[test]
    fn shaded_windows_deepen_from_the_outermost_ring() {
        let mut a = Appearance::default();
        a.use_theme("graphite");
        a.window_colours = WindowColours::Shaded;
        let weekly = color(PRESETS[0].roles.weekly);
        let without_5h = a.resolve(true, false);
        assert_eq!(rgb_of(without_5h.windows[1]), rgb_of(weekly));
        let with_5h = a.resolve(true, true);
        assert_eq!(rgb_of(with_5h.windows[0]), rgb_of(weekly));
        assert_ne!(rgb_of(with_5h.windows[1]), rgb_of(weekly));
    }

    #[test]
    fn colour_blind_warnings_replace_the_theme_pair() {
        let mut a = Appearance::default();
        a.use_theme("midnight");
        a.warning_colours = WarningColours::ColourBlind;
        let p = a.resolve(true, false);
        assert_eq!(rgb_of(p.over_pace), 0xffb000);
        a.warning_colours = WarningColours::Hatching;
        assert!(a.resolve(true, false).hatch_only);
    }

    #[test]
    fn widget_size_snaps_to_five_percent_steps_within_range() {
        assert_eq!(snap_widget_size(60.0), 75);
        assert_eq!(snap_widget_size(203.0), 200);
        assert_eq!(snap_widget_size(112.0), 110);
        assert_eq!(snap_widget_size(113.0), 115);
        assert_eq!(snap_widget_size(112.5), 115);
        assert_eq!(snap_widget_size(125.0), 125);
        assert_eq!(snap_widget_size(-40.0), 75);
        assert_eq!(snap_widget_size(f64::NAN), 100);
        let a = Appearance {
            widget_size: 999,
            ..Appearance::default()
        };
        assert_eq!(a.clamped_widget_size(), 200);
    }

    #[test]
    fn widget_size_defaults_and_hand_edits_are_snapped_on_load() {
        let edited: Appearance = serde_json::from_str(r#"{"widget_size":-12}"#).unwrap();
        assert_eq!(edited.widget_size, 75);
        let edited: Appearance = serde_json::from_str(r#"{"widget_size":187.4}"#).unwrap();
        assert_eq!(edited.widget_size, 185);
    }
}
