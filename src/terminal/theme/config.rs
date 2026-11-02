//! Reading and writing the `theme` entry in the config file.

use super::Theme;
use std::fs;
use std::path::{Path, PathBuf};

/// Collapse a theme name to lowercase letters so `Catppuccin Mocha`,
/// `catppuccin_mocha`, and `catppuccin-mocha` compare equal.
pub(super) fn normalize(name: &str) -> String {
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
    let mut updated = String::with_capacity(text.len() + assignment.len() + 1);
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.starts_with('[') {
            // `[]` is still the root table; every other header leaves it.
            in_root = line == "[theme]" || line == "[]";
        }
        let is_theme_key = in_root
            && line
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "theme");
        if is_theme_key && !replaced {
            replaced = true;
            updated.push_str(&assignment);
        } else {
            updated.push_str(raw);
        }
        updated.push('\n');
    }
    if !replaced {
        if !(text.ends_with('\n') || text.is_empty()) {
            // The file has no trailing newline, so the entry would glue onto
            // the last line without one of its own.
            updated.push('\n');
        }
        updated.push_str(&assignment);
        updated.push('\n');
    }
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
            in_root = line == "[theme]" || line == "[]";
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
    use super::super::Theme;
    use super::*;

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
}
