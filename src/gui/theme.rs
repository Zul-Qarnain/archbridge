use gpui::*;

// Background palette
pub const BG_DARK: u32 = 0x070b12;
pub const BG_SIDEBAR: u32 = 0x0a0f1a;
pub const BG_CARD: u32 = 0x0f172a;
pub const BG_INPUT: u32 = 0x0f172a;
pub const BG_HOVER: u32 = 0x1e293b;

// Border colors
pub const BORDER_SUBTLE: u32 = 0x1e293b;
pub const BORDER_ACTIVE: u32 = 0x334155;

// Text palette
pub const TEXT_PRIMARY: u32 = 0xe2e8f0;
pub const TEXT_SECONDARY: u32 = 0x94a3b8;
pub const TEXT_MUTED: u32 = 0x475569;
pub const TEXT_PLACEHOLDER: u32 = 0x64748b;

// Accent colors
pub const ACCENT_CYAN: u32 = 0x38bdf8;
pub const ACCENT_GREEN: u32 = 0x10b981;
pub const ACCENT_AMBER: u32 = 0xf59e0b;
pub const ACCENT_RED: u32 = 0xef4444;
pub const ACCENT_PURPLE: u32 = 0x818cf8;
pub const ACCENT_BLUE: u32 = 0x3b82f6;

// Status pill colors
pub const PILL_OK: u32 = 0x134e26;
pub const PILL_WARN: u32 = 0x3d2c00;
pub const PILL_ERR: u32 = 0x450a0a;
pub const PILL_OK_TEXT: u32 = 0x10b981;
pub const PILL_WARN_TEXT: u32 = 0xfbbf24;
pub const PILL_ERR_TEXT: u32 = 0xef4444;

pub fn bg_dark() -> Hsla {
    rgb(BG_DARK).into()
}

pub fn bg_sidebar() -> Hsla {
    rgb(BG_SIDEBAR).into()
}

pub fn bg_card() -> Hsla {
    rgb(BG_CARD).into()
}

pub fn border_subtle() -> Hsla {
    rgb(BORDER_SUBTLE).into()
}

pub fn border_active() -> Hsla {
    rgb(BORDER_ACTIVE).into()
}

pub fn text_primary() -> Hsla {
    rgb(TEXT_PRIMARY).into()
}

pub fn text_secondary() -> Hsla {
    rgb(TEXT_SECONDARY).into()
}

pub fn text_muted() -> Hsla {
    rgb(TEXT_MUTED).into()
}

pub fn accent_cyan() -> Hsla {
    rgb(ACCENT_CYAN).into()
}

pub fn accent_green() -> Hsla {
    rgb(ACCENT_GREEN).into()
}

pub fn accent_red() -> Hsla {
    rgb(ACCENT_RED).into()
}

pub fn accent_amber() -> Hsla {
    rgb(ACCENT_AMBER).into()
}
