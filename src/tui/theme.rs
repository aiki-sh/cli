//! Theme configuration for TUI rendering.
//!
//! Provides dark and light color schemes with automatic terminal detection,
//! status-aware styling, and unicode symbol constants.

use ratatui::style::{Color, Modifier, Style};

/// Terminal color mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    /// Dark background terminal.
    Dark,
    /// Light background terminal.
    Light,
}

/// Detect the terminal's color mode.
///
/// Checks `AIKI_THEME` env var first (`"light"` or `"dark"`, case-insensitive).
/// Falls back to `terminal_colorsaurus` OSC 11 detection, defaulting to dark.
pub fn detect_mode() -> ThemeMode {
    if let Ok(v) = std::env::var("AIKI_THEME") {
        return match v.to_lowercase().as_str() {
            "light" => ThemeMode::Light,
            _ => ThemeMode::Dark,
        };
    }
    match terminal_colorsaurus::color_scheme(Default::default()) {
        Ok(terminal_colorsaurus::ColorScheme::Light) => ThemeMode::Light,
        _ => ThemeMode::Dark,
    }
}

// ── Symbol constants (mode-independent) ──────────────────────────────

/// Symbol for pending/ready status.
pub const SYM_PENDING: &str = "○";
/// Symbol for failed status.
pub const SYM_FAILED: &str = "✘";
/// Symbol for check/success.
pub const SYM_CHECK: &str = "✔";
/// Symbol for running status.
pub const SYM_RUNNING: &str = "▸";
/// Symbol for starting/spawning status.
pub const SYM_STARTING: &str = "⧗";
/// Symbol for skipped status.
pub const SYM_SKIPPED: &str = "─";
/// Symbol for pending-unassigned status (not yet claimed by any lane).
pub const SYM_PENDING_UNASSIGNED: &str = "◌";

// ── Theme ────────────────────────────────────────────────────────────

/// Color palette for TUI rendering.
///
/// All colors use [`Color::Rgb`] for consistent rendering across terminals.
#[derive(Debug, Clone)]
pub struct Theme {
    /// Done, success.
    pub green: Color,
    /// Review, info.
    pub cyan: Color,
    /// Active, in-progress.
    pub yellow: Color,
    /// Failed, error.
    pub red: Color,
    /// Cursor agent.
    pub magenta: Color,
    /// Borders, inactive.
    pub dim: Color,
    /// Supporting text.
    pub fg: Color,
    /// Primary text.
    pub text: Color,
    /// High-contrast headers.
    pub hi: Color,
    /// Background.
    #[allow(dead_code)]
    pub bg: Color,
    /// Subtle background for agent blocks.
    pub surface: Color,
}

impl Theme {
    /// Dark mode color palette.
    pub fn dark() -> Self {
        Self {
            green: Color::Rgb(0x5f, 0xcc, 0x68),
            cyan: Color::Rgb(0x5b, 0xb8, 0xc9),
            yellow: Color::Rgb(0xd4, 0xa8, 0x40),
            red: Color::Rgb(0xe0, 0x55, 0x55),
            magenta: Color::Rgb(0xc4, 0x70, 0xb0),
            dim: Color::Rgb(0x58, 0x58, 0x6a),
            fg: Color::Rgb(0x8c, 0x8c, 0x96),
            text: Color::Rgb(0xcc, 0xcc, 0xcc),
            hi: Color::Rgb(0xe8, 0xe8, 0xe8),
            bg: Color::Rgb(0x1a, 0x1a, 0x24),
            surface: Color::Rgb(0x25, 0x25, 0x2f),
        }
    }

    /// Light mode color palette.
    pub fn light() -> Self {
        Self {
            green: Color::Rgb(0x3a, 0x9e, 0x44),
            cyan: Color::Rgb(0x2a, 0x8a, 0x9e),
            yellow: Color::Rgb(0xa0, 0x78, 0x20),
            red: Color::Rgb(0xc4, 0x30, 0x30),
            magenta: Color::Rgb(0xa0, 0x48, 0x90),
            dim: Color::Rgb(0xc8, 0xc8, 0xd0),
            fg: Color::Rgb(0x66, 0x66, 0x66),
            text: Color::Rgb(0x2a, 0x2a, 0x2a),
            hi: Color::Rgb(0x11, 0x11, 0x11),
            bg: Color::Rgb(0xf5, 0xf5, 0xf0),
            surface: Color::Rgb(0xe8, 0xe8, 0xec),
        }
    }

    /// Construct a theme from a [`ThemeMode`].
    pub fn from_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }

    /// Style with dim foreground color.
    pub fn dim_style(&self) -> Style {
        Style::default().fg(self.dim)
    }

    /// Style with supporting-text foreground color.
    pub fn fg_style(&self) -> Style {
        Style::default().fg(self.fg)
    }

    /// Style with primary-text foreground color.
    pub fn text_style(&self) -> Style {
        Style::default().fg(self.text)
    }

    /// Style with high-contrast bold foreground.
    pub fn hi_style(&self) -> Style {
        Style::default().fg(self.hi).add_modifier(Modifier::BOLD)
    }
}
