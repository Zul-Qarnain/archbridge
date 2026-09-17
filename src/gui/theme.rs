use gpui::*;

// Background palette (matching stitch_ui_clone_generator HTML)
pub const BG_DARKEST: u32 = 0x070b14;
pub const BG_DARK: u32 = 0x0b0f19;
pub const BG_TITLEBAR: u32 = 0x090d16;
pub const BG_SIDEBAR: u32 = 0x090d16;
pub const BG_CARD: u32 = 0x0e1626;
pub const BG_CARD_ALT: u32 = 0x0f1524;
pub const BG_INPUT: u32 = 0x0d1424;
pub const BG_HOVER: u32 = 0x121c30;
pub const BG_PILL: u32 = 0x121927;

// Border colors
pub const BORDER_SLATE: u32 = 0x1e293b;
pub const BORDER_SUBTLE: u32 = 0x1e293b;
pub const BORDER_ACTIVE: u32 = 0x38bdf8;
pub const BORDER_GLOW_OFFICIAL: u32 = 0x0ea5e9;
pub const BORDER_GLOW_AUR: u32 = 0xa855f7;
pub const BORDER_GLOW_FLATPAK: u32 = 0x2dd4bf;
pub const BORDER_GLOW_APPIMAGE: u32 = 0xf97316;
pub const BORDER_GLOW_UPSTREAM: u32 = 0x38bdf8;

// Text palette
pub const TEXT_PRIMARY: u32 = 0xffffff;
pub const TEXT_SECONDARY: u32 = 0xc9d1d9;
pub const TEXT_MUTED: u32 = 0x64748b;
pub const TEXT_PLACEHOLDER: u32 = 0x475569;

// Accent & Source Colors
pub const ACCENT_CYAN: u32 = 0x38bdf8;
pub const ACCENT_BLUE: u32 = 0x0284c7;
pub const ACCENT_GREEN: u32 = 0x10b981;
pub const ACCENT_EMERALD: u32 = 0x34d399;
pub const ACCENT_AMBER: u32 = 0xf59e0b;
pub const ACCENT_RED: u32 = 0xef4444;
pub const ACCENT_PURPLE: u32 = 0xa855f7;
pub const ACCENT_TEAL: u32 = 0x2dd4bf;
pub const ACCENT_ORANGE: u32 = 0xf97316;

// Status pill colors
pub const PILL_OK: u32 = 0x064e3b;
pub const PILL_OK_TEXT: u32 = 0x34d399;
pub const PILL_WARN: u32 = 0x451a03;
pub const PILL_WARN_TEXT: u32 = 0xfbbf24;
pub const PILL_ERR: u32 = 0x450a0a;
pub const PILL_ERR_TEXT: u32 = 0xf87171;

pub fn bg_darkest() -> Hsla { rgb(BG_DARKEST).into() }
pub fn bg_dark() -> Hsla { rgb(BG_DARK).into() }
pub fn bg_sidebar() -> Hsla { rgb(BG_SIDEBAR).into() }
pub fn bg_card() -> Hsla { rgb(BG_CARD).into() }
pub fn border_subtle() -> Hsla { rgb(BORDER_SUBTLE).into() }
pub fn text_primary() -> Hsla { rgb(TEXT_PRIMARY).into() }
pub fn text_secondary() -> Hsla { rgb(TEXT_SECONDARY).into() }
pub fn text_muted() -> Hsla { rgb(TEXT_MUTED).into() }
pub fn accent_cyan() -> Hsla { rgb(ACCENT_CYAN).into() }
pub fn accent_green() -> Hsla { rgb(ACCENT_GREEN).into() }
pub fn accent_blue() -> Hsla { rgb(ACCENT_BLUE).into() }
