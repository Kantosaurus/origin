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

pub const DIFF_ADD_FG: u32 = 0x00_7F_D8_8F;
pub const DIFF_ADD_BG: u32 = 0x00_12_24_12;
pub const DIFF_DEL_FG: u32 = 0x00_E0_6C_75;
pub const DIFF_DEL_BG: u32 = 0x00_24_12_12;

pub const GREEN: u32 = 0x00_7F_D8_8F;
pub const YELLOW: u32 = 0x00_E5_C0_7B;
pub const RED: u32 = 0x00_E0_6C_75;

pub const RULE: u32 = 0x00_28_24_20;
pub const DIM: u32 = 0x00_44_40_3C;

pub const PANEL_HEADER: u32 = 0x00_A0_8A_6E;
pub const PANEL_BG: u32 = 0x00_12_10_0E;
