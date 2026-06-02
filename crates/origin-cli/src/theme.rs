// SPDX-License-Identifier: Apache-2.0
//! Origin TUI color palette — "Burnished Copper" identity v2.
//!
//! Refined palette inspired by opencode's deep blacks and jcode's clean
//! hierarchy. All values are 0x00RRGGBB packed u32, matching
//! `Cell::fg` / `Cell::bg`. Zero means "terminal default".

pub const SURFACE: u32 = 0x00_0F_0D_0B;
pub const SURFACE_RAISED: u32 = 0x00_1A_17_14;
pub const BORDER: u32 = 0x00_28_24_20;

pub const MUTED: u32 = 0x00_5C_57_52;
pub const BODY: u32 = 0x00_C8_C1_B8;
pub const BRIGHT: u32 = 0x00_F0_EB_E3;

pub const ACCENT: u32 = 0x00_D4_88_4E;
pub const ACCENT_DIM: u32 = 0x00_8B_66_40;

pub const H1: u32 = 0x00_F0_D0_80;
pub const H2: u32 = 0x00_D4_A8_60;
pub const H3: u32 = 0x00_B8_90_58;

pub const USER: u32 = 0x00_8A_B4_F8;
pub const TOOL: u32 = 0x00_9D_7C_D8;

pub const CODE_FG: u32 = 0x00_B0_A8_9E;
pub const CODE_BG: u32 = 0x00_16_13_11;

// Diff rows render the changed text in near-white (`BRIGHT`) for legibility,
// highlighted with a saturated green (addition) or red (deletion) background so
// the kind of change reads at a glance instead of relying on a dim foreground.
pub const DIFF_ADD_FG: u32 = 0x00_F0_EB_E3;
pub const DIFF_ADD_BG: u32 = 0x00_1E_4D_2A;
pub const DIFF_DEL_FG: u32 = 0x00_F0_EB_E3;
pub const DIFF_DEL_BG: u32 = 0x00_5A_1E_22;

pub const GREEN: u32 = 0x00_7F_D8_8F;
pub const YELLOW: u32 = 0x00_E5_C0_7B;
pub const RED: u32 = 0x00_E0_6C_75;

pub const RULE: u32 = 0x00_28_24_20;
pub const DIM: u32 = 0x00_44_40_3C;

pub const PANEL_HEADER: u32 = 0x00_A0_8A_6E;
pub const PANEL_BG: u32 = 0x00_12_10_0E;

/// A selectable color preset (aider L107 theme parity).
///
/// [`Theme::Default`] reproduces the legacy "Burnished Copper" constants above
/// verbatim, so a session that never switches themes renders byte-identically.
/// The other presets are opt-in via the `/theme <name>` composer command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// The shipped "Burnished Copper" palette — identical to the module
    /// constants. This is the only theme used unless the user opts in.
    #[default]
    Default,
    /// A deeper, cooler dark variant.
    Dark,
    /// A light-background palette for bright terminals.
    Light,
    /// A maximum-contrast palette for accessibility.
    HighContrast,
}

impl Theme {
    /// Parse a `/theme <name>` argument (case-insensitive).
    ///
    /// Recognises the canonical names plus `high-contrast`/`hc` aliases.
    /// Unknown ⇒ `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "default" => Some(Self::Default),
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "high-contrast" | "highcontrast" | "hc" => Some(Self::HighContrast),
            _ => None,
        }
    }

    /// The canonical lower-case name, for status-line echo and round-tripping.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Dark => "dark",
            Self::Light => "light",
            Self::HighContrast => "high-contrast",
        }
    }
}

/// The named colors the status line and chrome consume.
///
/// Bundled as one value so a theme can be swapped at runtime without touching
/// the module constants. Every field is a `0x00RRGGBB` packed `u32` matching
/// `Cell::fg`/`Cell::bg`
/// (`0` = terminal default), exactly like the free constants. The renderer can
/// keep reading the constants for the default theme (byte-identical) or read a
/// `Palette` when a non-default theme is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub surface: u32,
    pub surface_raised: u32,
    pub border: u32,
    pub muted: u32,
    pub body: u32,
    pub bright: u32,
    pub accent: u32,
    pub accent_dim: u32,
    pub user: u32,
    pub tool: u32,
    pub code_fg: u32,
    pub code_bg: u32,
    pub green: u32,
    pub yellow: u32,
    pub red: u32,
    pub dim: u32,
    pub rule: u32,
    pub panel_header: u32,
    pub panel_bg: u32,
}

impl Default for Palette {
    fn default() -> Self {
        palette(Theme::Default)
    }
}

/// Pure theme → palette map.
///
/// [`Theme::Default`] returns exactly the module constants, so the default
/// render path is byte-identical. The other arms return distinct, internally
/// consistent palettes.
#[must_use]
pub const fn palette(theme: Theme) -> Palette {
    match theme {
        Theme::Default => Palette {
            surface: SURFACE,
            surface_raised: SURFACE_RAISED,
            border: BORDER,
            muted: MUTED,
            body: BODY,
            bright: BRIGHT,
            accent: ACCENT,
            accent_dim: ACCENT_DIM,
            user: USER,
            tool: TOOL,
            code_fg: CODE_FG,
            code_bg: CODE_BG,
            green: GREEN,
            yellow: YELLOW,
            red: RED,
            dim: DIM,
            rule: RULE,
            panel_header: PANEL_HEADER,
            panel_bg: PANEL_BG,
        },
        Theme::Dark => Palette {
            surface: 0x00_06_07_0A,
            surface_raised: 0x00_0F_12_18,
            border: 0x00_20_26_30,
            muted: 0x00_55_5E_6B,
            body: 0x00_C2_CA_D4,
            bright: 0x00_EE_F2_F8,
            accent: 0x00_5E_A9_FF,
            accent_dim: 0x00_3A_6A_A8,
            user: 0x00_7F_C8_A0,
            tool: 0x00_B0_8C_F0,
            code_fg: 0x00_A8_B0_BC,
            code_bg: 0x00_0B_0E_13,
            green: 0x00_6F_D8_8F,
            yellow: 0x00_E5_C0_7B,
            red: 0x00_E0_6C_75,
            dim: 0x00_38_3F_49,
            rule: 0x00_20_26_30,
            panel_header: 0x00_8A_98_AA,
            panel_bg: 0x00_09_0B_10,
        },
        Theme::Light => Palette {
            surface: 0x00_FA_F8_F4,
            surface_raised: 0x00_FF_FF_FF,
            border: 0x00_D8_D2_C8,
            muted: 0x00_8A_84_7C,
            body: 0x00_2A_27_22,
            bright: 0x00_10_0E_0B,
            accent: 0x00_B0_5A_1E,
            accent_dim: 0x00_C8_8A_5E,
            user: 0x00_2A_5A_C8,
            tool: 0x00_6A_3A_C0,
            code_fg: 0x00_3A_36_30,
            code_bg: 0x00_F0_ED_E6,
            green: 0x00_2E_8B_4E,
            yellow: 0x00_9A_6A_10,
            red: 0x00_C0_3A_3A,
            dim: 0x00_B0_AA_A0,
            rule: 0x00_D8_D2_C8,
            panel_header: 0x00_6A_5A_46,
            panel_bg: 0x00_F2_EF_E8,
        },
        Theme::HighContrast => Palette {
            surface: 0x00_00_00_00,
            surface_raised: 0x00_0A_0A_0A,
            border: 0x00_FF_FF_FF,
            muted: 0x00_C0_C0_C0,
            body: 0x00_FF_FF_FF,
            bright: 0x00_FF_FF_FF,
            accent: 0x00_FF_E0_00,
            accent_dim: 0x00_C0_A8_00,
            user: 0x00_00_E0_FF,
            tool: 0x00_FF_80_FF,
            code_fg: 0x00_FF_FF_FF,
            code_bg: 0x00_00_00_00,
            green: 0x00_00_FF_00,
            yellow: 0x00_FF_FF_00,
            red: 0x00_FF_40_40,
            dim: 0x00_A0_A0_A0,
            rule: 0x00_FF_FF_FF,
            panel_header: 0x00_FF_E0_00,
            panel_bg: 0x00_00_00_00,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_palette_matches_module_constants() {
        // The Default theme must reproduce the legacy constants verbatim so the
        // default render path is byte-identical.
        let p = palette(Theme::Default);
        assert_eq!(p.surface, SURFACE);
        assert_eq!(p.surface_raised, SURFACE_RAISED);
        assert_eq!(p.border, BORDER);
        assert_eq!(p.muted, MUTED);
        assert_eq!(p.body, BODY);
        assert_eq!(p.bright, BRIGHT);
        assert_eq!(p.accent, ACCENT);
        assert_eq!(p.accent_dim, ACCENT_DIM);
        assert_eq!(p.user, USER);
        assert_eq!(p.tool, TOOL);
        assert_eq!(p.code_fg, CODE_FG);
        assert_eq!(p.code_bg, CODE_BG);
        assert_eq!(p.green, GREEN);
        assert_eq!(p.yellow, YELLOW);
        assert_eq!(p.red, RED);
        assert_eq!(p.dim, DIM);
        assert_eq!(p.rule, RULE);
        assert_eq!(p.panel_header, PANEL_HEADER);
        assert_eq!(p.panel_bg, PANEL_BG);
    }

    #[test]
    fn palette_default_impl_equals_default_theme() {
        assert_eq!(Palette::default(), palette(Theme::Default));
    }

    #[test]
    fn themes_have_distinct_palettes() {
        // Each preset must differ from Default on the visible chrome colors so a
        // user can tell which theme is active.
        let def = palette(Theme::Default);
        for t in [Theme::Dark, Theme::Light, Theme::HighContrast] {
            let p = palette(t);
            assert_ne!(p.accent, def.accent, "{} accent must differ", t.name());
            assert_ne!(p.body, def.body, "{} body must differ", t.name());
            assert_ne!(p.surface, def.surface, "{} surface must differ", t.name());
        }
    }

    #[test]
    fn theme_parse_round_trips_names() {
        for t in [Theme::Default, Theme::Dark, Theme::Light, Theme::HighContrast] {
            assert_eq!(Theme::parse(t.name()), Some(t));
        }
        // Aliases and case-insensitivity.
        assert_eq!(Theme::parse("HIGH_CONTRAST"), Some(Theme::HighContrast));
        assert_eq!(Theme::parse("hc"), Some(Theme::HighContrast));
        assert_eq!(Theme::parse("  Dark  "), Some(Theme::Dark));
        assert_eq!(Theme::parse("nope"), None);
    }

    #[test]
    fn default_theme_is_default() {
        assert_eq!(Theme::default(), Theme::Default);
    }
}
