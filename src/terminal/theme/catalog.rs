//! Named color themes and the config that selects one.
//!
//! A theme supplies the default foreground and background plus the 16 ANSI
//! colors. The 256-color cube and truecolor stay as specified by the
//! application; only the named slots are themed.
//!
//! The active theme is read once at startup, in this order:
//!
//! 1. `SHITTY_THEME` (a flavor name, or `default`)
//! 2. `theme` in the config file (`$SHITTY_CONFIG`, else
//!    `~/.config/shitty/config.toml`)
//! 3. [`Theme::Default`]
//!
//! Unknown names and unreadable files fall back to the default theme so a bad
//! config never stops the terminal from launching. A theme picked from the
//! View menu is written back to the same file, so it survives a restart;
//! `SHITTY_THEME` still wins for that one launch.

use super::color::{ANSI_16, Color32};
use super::config::normalize;
use super::palette;

/// A selectable terminal color theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Theme {
    /// The original 16-color VGA palette on a black background.
    #[default]
    Default,
    /// Catppuccin Latte, the light flavor.
    CatppuccinLatte,
    /// Catppuccin Frappé, the medium dark flavor.
    CatppuccinFrappe,
    /// Catppuccin Macchiato, the darker flavor.
    CatppuccinMacchiato,
    /// Catppuccin Mocha, the darkest flavor.
    CatppuccinMocha,
}

impl Theme {
    /// Config names, in menu order. `default` is the unthemed palette.
    pub(crate) const NAMES: [&'static str; 5] = [
        "default",
        "catppuccin-latte",
        "catppuccin-frappe",
        "catppuccin-macchiato",
        "catppuccin-mocha",
    ];

    /// Parse a config value. Hyphens, spaces, and underscores are equivalent,
    /// and the `catppuccin` prefix is optional (`mocha` selects Mocha).
    pub(crate) fn parse(name: &str) -> Option<Self> {
        match normalize(name).as_str() {
            "default" | "none" => Some(Self::Default),
            "catppuccinlatte" | "latte" => Some(Self::CatppuccinLatte),
            "catppuccinfrappe" | "frappe" => Some(Self::CatppuccinFrappe),
            "catppuccinmacchiato" | "macchiato" => Some(Self::CatppuccinMacchiato),
            "catppuccinmocha" | "mocha" => Some(Self::CatppuccinMocha),
            _ => None,
        }
    }

    /// The name written to and read from the config file.
    pub(crate) fn name(self) -> &'static str {
        Self::NAMES[self as usize]
    }

    /// Label shown in the View menu.
    pub(crate) fn menu_title(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::CatppuccinLatte => "Catppuccin Latte",
            Self::CatppuccinFrappe => "Catppuccin Frappé",
            Self::CatppuccinMacchiato => "Catppuccin Macchiato",
            Self::CatppuccinMocha => "Catppuccin Mocha",
        }
    }

    /// Foreground, background, and the 16 ANSI colors for this theme.
    pub(crate) fn scheme(self) -> ColorScheme {
        match self {
            Self::Default => ColorScheme::default_vga(),
            Self::CatppuccinLatte => palette::CATPPUCCIN_LATTE,
            Self::CatppuccinFrappe => palette::CATPPUCCIN_FRAPPE,
            Self::CatppuccinMacchiato => palette::CATPPUCCIN_MACCHIATO,
            Self::CatppuccinMocha => palette::CATPPUCCIN_MOCHA,
        }
    }
}

/// The colors a theme overrides: default foreground and background, plus the
/// 16 ANSI colors in palette order (0 black … 7 white, 8–15 bright).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorScheme {
    pub fg: Color32,
    pub bg: Color32,
    pub ansi: [Color32; 16],
    /// Highlight drawn behind a selection. Opaque themes paint it translucent.
    pub selection: Color32,
}

impl ColorScheme {
    /// The palette the terminal used before themes existed.
    fn default_vga() -> Self {
        Self {
            fg: Color32::from_rgb(204, 204, 204),
            bg: Color32::BLACK,
            ansi: ANSI_16,
            selection: Color32::from_rgba(70, 110, 220, 110),
        }
    }

    /// ANSI color `index` (0–15), or `None` for the cube and grayscale ramp.
    pub(crate) fn ansi(&self, index: u8) -> Option<Color32> {
        self.ansi.get(index as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_parse_with_flexible_spelling() {
        assert_eq!(Theme::parse("mocha"), Some(Theme::CatppuccinMocha));
        assert_eq!(
            Theme::parse("Catppuccin Macchiato"),
            Some(Theme::CatppuccinMacchiato)
        );
        assert_eq!(
            Theme::parse("catppuccin_frappe"),
            Some(Theme::CatppuccinFrappe)
        );
        assert_eq!(Theme::parse("latte"), Some(Theme::CatppuccinLatte));
        assert_eq!(Theme::parse("default"), Some(Theme::Default));
        assert_eq!(Theme::parse("solarized"), None);
    }

    #[test]
    fn every_theme_name_round_trips() {
        for name in Theme::NAMES {
            let theme = Theme::parse(name).unwrap_or_else(|| panic!("{name} should parse"));
            assert_eq!(theme.name(), name);
        }
    }
}
