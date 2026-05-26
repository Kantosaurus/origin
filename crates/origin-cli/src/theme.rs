//! Origin TUI color palette — "Burnished Copper" identity.
//!
//! All values are 0x00RRGGBB packed u32, matching `Cell::fg` / `Cell::bg`.
//! Zero means "terminal default" (inherit from user's terminal theme).

pub const SURFACE: u32 = 0x00_1C_19_17;
pub const BORDER: u32 = 0x00_2E_2A_25;

pub const MUTED: u32 = 0x00_6B_65_60;
pub const BODY: u32 = 0x00_D5_CE_C5;
pub const BRIGHT: u32 = 0x00_F0_EB_E3;

pub const ACCENT: u32 = 0x00_D4_88_4E;
pub const ACCENT_DIM: u32 = 0x00_8B_66_40;

pub const DIFF_ADD_FG: u32 = 0x00_8C_D4_8C;
pub const DIFF_ADD_BG: u32 = 0x00_14_2A_14;
pub const DIFF_DEL_FG: u32 = 0x00_D4_8C_8C;
pub const DIFF_DEL_BG: u32 = 0x00_2A_14_14;

pub const GREEN: u32 = 0x00_8C_D4_8C;
pub const YELLOW: u32 = 0x00_D4_C0_4E;
pub const RED: u32 = 0x00_D4_5A_5A;
