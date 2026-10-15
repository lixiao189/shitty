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

use crate::terminal::color::Color32;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

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
            Self::CatppuccinLatte => CATPPUCCIN_LATTE,
            Self::CatppuccinFrappe => CATPPUCCIN_FRAPPE,
            Self::CatppuccinMacchiato => CATPPUCCIN_MACCHIATO,
            Self::CatppuccinMocha => CATPPUCCIN_MOCHA,
        }
    }
}

impl fmt::Display for Theme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
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
            ansi: [
                Color32::from_rgb(0, 0, 0),
                Color32::from_rgb(128, 0, 0),
                Color32::from_rgb(0, 128, 0),
                Color32::from_rgb(128, 128, 0),
                Color32::from_rgb(0, 0, 128),
                Color32::from_rgb(128, 0, 128),
                Color32::from_rgb(0, 128, 128),
                Color32::from_rgb(192, 192, 192),
                Color32::from_rgb(128, 128, 128),
                Color32::from_rgb(255, 0, 0),
                Color32::from_rgb(0, 255, 0),
                Color32::from_rgb(255, 255, 0),
                Color32::from_rgb(0, 0, 255),
                Color32::from_rgb(255, 0, 255),
                Color32::from_rgb(0, 255, 255),
                Color32::WHITE,
            ],
            selection: Color32::from_rgba(70, 110, 220, 110),
        }
    }

    /// ANSI color `index` (0–15), or `None` for the cube and grayscale ramp.
    pub(crate) fn ansi(&self, index: u8) -> Option<Color32> {
        self.ansi.get(index as usize).copied()
    }
}

/// Palettes from the official Catppuccin port for Alacritty
/// (<https://github.com/catppuccin/alacritty>). Normal and bright colors are
/// identical for the accent hues; only black and white differ.
const CATPPUCCIN_LATTE: ColorScheme = scheme(
    0x4c4f69,
    0xeff1f5,
    [
        0xbcc0cc, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0x5c5f77, 0xacb0be,
        0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0x6c6f85,
    ],
    0xdc8a78,
);
const CATPPUCCIN_FRAPPE: ColorScheme = scheme(
    0xc6d0f5,
    0x303446,
    [
        0x51576d, 0xe78284, 0xa6d189, 0xe5c890, 0x8caaee, 0xf4b8e4, 0x81c8be, 0xb5bfe2, 0x626880,
        0xe78284, 0xa6d189, 0xe5c890, 0x8caaee, 0xf4b8e4, 0x81c8be, 0xa5adce,
    ],
    0xf2d5cf,
);
const CATPPUCCIN_MACCHIATO: ColorScheme = scheme(
    0xcad3f5,
    0x24273a,
    [
        0x494d64, 0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xb8c0e0, 0x5b6078,
        0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xa5adcb,
    ],
    0xf4dbd6,
);
const CATPPUCCIN_MOCHA: ColorScheme = scheme(
    0xcdd6f4,
    0x1e1e2e,
    [
        0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70,
        0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
    ],
    0xf5e0dc,
);

/// Build a scheme from packed RGB values. Selection uses the theme's own
/// selection color at a fixed translucency so text underneath stays readable.
const fn scheme(fg: u32, bg: u32, ansi: [u32; 16], selection: u32) -> ColorScheme {
    ColorScheme {
        fg: rgb(fg),
        bg: rgb(bg),
        ansi: [
            rgb(ansi[0]),
            rgb(ansi[1]),
            rgb(ansi[2]),
            rgb(ansi[3]),
            rgb(ansi[4]),
            rgb(ansi[5]),
            rgb(ansi[6]),
            rgb(ansi[7]),
            rgb(ansi[8]),
            rgb(ansi[9]),
            rgb(ansi[10]),
            rgb(ansi[11]),
            rgb(ansi[12]),
            rgb(ansi[13]),
            rgb(ansi[14]),
            rgb(ansi[15]),
        ],
        selection: rgba(selection, 140),
    }
}

const fn rgb(packed: u32) -> Color32 {
    Color32::from_rgb((packed >> 16) as u8, (packed >> 8) as u8, packed as u8)
}

const fn rgba(packed: u32, alpha: u8) -> Color32 {
    Color32::from_rgba(
        (packed >> 16) as u8,
        (packed >> 8) as u8,
        packed as u8,
        alpha,
    )
}

/// Collapse a theme name to lowercase letters so `Catppuccin Mocha`,
/// `catppuccin_mocha`, and `catppuccin-mocha` compare equal.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

/// Theme selected by the environment and config file. See the module docs.
pub(crate) fn configured_theme() -> Theme {
    if let Some(theme) = env_theme("SHITTY_THEME") {
        return theme;
    }
    config_file_theme(&config_path()).unwrap_or_default()
}

/// `SHITTY_THEME`, when it names a known theme. An empty value is ignored so
/// unsetting the variable in a shell that exports it blank still works.
fn env_theme(key: &str) -> Option<Theme> {
    let value = std::env::var(key).ok()?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    match Theme::parse(value) {
        Some(theme) => Some(theme),
        None => {
            eprintln!("shitty: unknown theme '{value}' in {key}; using the default theme");
            Some(Theme::Default)
        }
    }
}

/// Remember `theme` in the config file, replacing an existing root `theme`
/// entry or appending one. Other lines, comments included, are kept. A failed
/// write is reported and otherwise ignored: the running session already
/// switched.
pub(crate) fn save_theme(theme: Theme) {
    let path = config_path();
    if let Err(error) = write_theme(&path, theme) {
        eprintln!(
            "shitty: could not save theme to {}: {error}",
            path.display()
        );
    }
}

fn write_theme(path: &Path, theme: Theme) -> Result<(), std::io::Error> {
    let existing = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let updated = set_theme_entry(&existing, theme);
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, updated)
}

/// Replace the root `theme` assignment, or append one. A `theme` key nested
/// in another table is left untouched, matching [`theme_from_config`].
fn set_theme_entry(text: &str, theme: Theme) -> String {
    let assignment = format!("theme = \"{}\"", theme.name());
    let mut in_root = true;
    let mut replaced = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|raw| {
            let line = strip_comment(raw).trim();
            if line.starts_with('[') {
                in_root = line == "[theme]" || is_empty_table(line);
            }
            let is_theme_key = in_root
                && line
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim() == "theme");
            if is_theme_key && !replaced {
                replaced = true;
                assignment.clone()
            } else {
                raw.to_owned()
            }
        })
        .collect();
    if !replaced {
        if text.ends_with('\n') || text.is_empty() {
            lines.push(assignment);
        } else {
            // The file has no trailing newline, so the entry would glue onto
            // the last line without one of its own.
            lines.push(String::new());
            lines.push(assignment);
        }
    }
    let mut updated = lines.join("\n");
    updated.push('\n');
    updated
}

/// `theme` from the config file, or `None` when the file is absent or has no
/// theme entry. A file that exists but cannot be read or parsed is reported
/// and treated as unconfigured.
fn config_file_theme(path: &Path) -> Option<Theme> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            eprintln!("shitty: could not read {}: {error}", path.display());
            return None;
        }
    };
    match theme_from_config(&text) {
        Ok(theme) => theme,
        Err(error) => {
            eprintln!("shitty: {}: {error}", path.display());
            None
        }
    }
}

/// Path of the config file: `$SHITTY_CONFIG`, else `~/.config/shitty/config.toml`.
fn config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SHITTY_CONFIG") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    let mut path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    path.push(".config");
    path.push("shitty");
    path.push("config.toml");
    path
}

/// Read the `theme` entry out of a TOML document.
///
/// Only the one key is needed, so this is a small scanner rather than a TOML
/// parser: it skips comments and other assignments, and understands the
/// `[table]` headers well enough to ignore a `theme` key nested inside one.
pub(crate) fn theme_from_config(text: &str) -> Result<Option<Theme>, String> {
    let mut in_root = true;
    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_root = line == "[theme]" || is_empty_table(line);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "theme" || !in_root {
            continue;
        }
        let value = value.trim();
        // `[theme]` as a table is a future extension; a bare `theme =` is not.
        if value.is_empty() {
            return Err("`theme` has no value".to_owned());
        }
        let name =
            unquote(value).ok_or_else(|| format!("`theme` must be a string, got {value}"))?;
        return Theme::parse(name)
            .ok_or_else(|| format!("unknown theme '{name}'"))
            .map(Some);
    }
    Ok(None)
}

/// `[table]` headers other than `[theme]` leave the root table.
fn is_empty_table(header: &str) -> bool {
    header == "[]"
}

/// Drop a `#` comment, keeping `#` that sits inside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

/// A TOML basic string: bare words are rejected so `theme = mocha` (a typo for
/// the quoted form) is reported instead of silently accepted.
fn unquote(value: &str) -> Option<&str> {
    let value = value.trim().trim_end_matches(',').trim();
    value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
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

    #[test]
    fn config_theme_entry_is_read_from_the_root_table() {
        let text = "font_size = 14\ntheme = \"catppuccin-mocha\"\n";
        assert_eq!(
            theme_from_config(text).unwrap(),
            Some(Theme::CatppuccinMocha)
        );
    }

    #[test]
    fn comments_and_nested_tables_do_not_set_the_theme() {
        let text = "# theme = \"latte\"\n[colors]\ntheme = \"mocha\"\n";
        assert_eq!(theme_from_config(text).unwrap(), None);
    }

    #[test]
    fn an_unknown_config_theme_is_an_error() {
        let error = theme_from_config("theme = \"nope\"").unwrap_err();
        assert!(error.contains("nope"), "{error}");
    }

    #[test]
    fn an_unquoted_theme_is_an_error() {
        assert!(theme_from_config("theme = mocha").is_err());
    }

    #[test]
    fn a_missing_config_file_leaves_the_theme_unset() {
        let path = std::env::temp_dir().join("shitty-missing-config.toml");
        let _ = fs::remove_file(&path);
        assert_eq!(config_file_theme(&path), None);
    }

    #[test]
    fn saving_a_theme_replaces_the_root_entry_and_keeps_the_rest() {
        let text =
            "font_size = 14\ntheme = \"default\" # was default\n[colors]\ntheme = \"latte\"\n";
        let updated = set_theme_entry(text, Theme::CatppuccinMocha);
        assert_eq!(
            theme_from_config(&updated).unwrap(),
            Some(Theme::CatppuccinMocha)
        );
        assert!(updated.contains("[colors]\ntheme = \"latte\""));
        assert!(!updated.contains("# was default"));
    }

    #[test]
    fn saving_a_theme_appends_an_entry_when_the_file_has_none() {
        let updated = set_theme_entry("font_size = 14\n", Theme::CatppuccinLatte);
        assert_eq!(
            theme_from_config(&updated).unwrap(),
            Some(Theme::CatppuccinLatte)
        );
        assert!(updated.starts_with("font_size = 14\n"));
    }

    #[test]
    fn catppuccin_mocha_uses_its_published_palette() {
        let scheme = Theme::CatppuccinMocha.scheme();
        assert_eq!(scheme.bg, Color32::from_rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(scheme.fg, Color32::from_rgb(0xcd, 0xd6, 0xf4));
        assert_eq!(scheme.ansi(1), Some(Color32::from_rgb(0xf3, 0x8b, 0xa8)));
        assert_eq!(scheme.ansi(16), None);
    }
}
