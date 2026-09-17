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

use crate::gui::state::AppTheme;

#[derive(Debug, Clone, Copy)]
pub struct ThemePalette {
    pub bg_darkest: u32,
    pub bg_dark: u32,
    pub bg_sidebar: u32,
    pub bg_card: u32,
    pub bg_card_alt: u32,
    pub bg_topbar: u32,
    pub bg_input: u32,
    pub border: u32,
    pub border_subtle: u32,
    pub text_primary: u32,
    pub text_secondary: u32,
    pub text_muted: u32,
    pub text_placeholder: u32,
    pub accent_cyan: u32,
    pub accent_green: u32,
    pub accent_red: u32,
}

pub fn get_palette(theme: AppTheme) -> ThemePalette {
    match theme {
        AppTheme::Dark => ThemePalette {
            bg_darkest: 0x070b14,
            bg_dark: 0x0b0f19,
            bg_sidebar: 0x080d16,
            bg_card: 0x0c1829,
            bg_card_alt: 0x0f1524,
            bg_topbar: 0x09101d,
            bg_input: 0x0c192b,
            border: 0x1c304a,
            border_subtle: 0x16243b,
            text_primary: 0xffffff,
            text_secondary: 0xc9d1d9,
            text_muted: 0x64748b,
            text_placeholder: 0x475569,
            accent_cyan: 0x38bdf8,
            accent_green: 0x10b981,
            accent_red: 0xef4444,
        },
        AppTheme::Midnight => ThemePalette {
            bg_darkest: 0x000000,
            bg_dark: 0x030712,
            bg_sidebar: 0x02040a,
            bg_card: 0x090f1e,
            bg_card_alt: 0x0d1527,
            bg_topbar: 0x050a14,
            bg_input: 0x090f1e,
            border: 0x1e293b,
            border_subtle: 0x111827,
            text_primary: 0xf9fafb,
            text_secondary: 0xd1d5db,
            text_muted: 0x9ca3af,
            text_placeholder: 0x6b7280,
            accent_cyan: 0x06b6d4,
            accent_green: 0x10b981,
            accent_red: 0xf43f5e,
        },
        AppTheme::Light => ThemePalette {
            bg_darkest: 0xf1f5f9,
            bg_dark: 0xf8fafc,
            bg_sidebar: 0xebf1f8,
            bg_card: 0xffffff,
            bg_card_alt: 0xf1f5f9,
            bg_topbar: 0xffffff,
            bg_input: 0xffffff,
            border: 0xcbd5e1,
            border_subtle: 0xe2e8f0,
            text_primary: 0x0f172a,
            text_secondary: 0x334155,
            text_muted: 0x64748b,
            text_placeholder: 0x94a3b8,
            accent_cyan: 0x0284c7,
            accent_green: 0x059669,
            accent_red: 0xdc2626,
        },
    }
}
