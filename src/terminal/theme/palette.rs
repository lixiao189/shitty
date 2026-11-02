//! Catppuccin palettes. Values match the official Alacritty port.

use super::ColorScheme;
use super::color::Color32;

/// Palettes from the official Catppuccin port for Alacritty
/// (<https://github.com/catppuccin/alacritty>). Normal and bright colors are
/// identical for the accent hues; only black and white differ.
pub(super) const CATPPUCCIN_LATTE: ColorScheme = scheme(
    0x4c4f69,
    0xeff1f5,
    [
        0xbcc0cc, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0x5c5f77, 0xacb0be,
        0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0x6c6f85,
    ],
    0xdc8a78,
);
pub(super) const CATPPUCCIN_FRAPPE: ColorScheme = scheme(
    0xc6d0f5,
    0x303446,
    [
        0x51576d, 0xe78284, 0xa6d189, 0xe5c890, 0x8caaee, 0xf4b8e4, 0x81c8be, 0xb5bfe2, 0x626880,
        0xe78284, 0xa6d189, 0xe5c890, 0x8caaee, 0xf4b8e4, 0x81c8be, 0xa5adce,
    ],
    0xf2d5cf,
);
pub(super) const CATPPUCCIN_MACCHIATO: ColorScheme = scheme(
    0xcad3f5,
    0x24273a,
    [
        0x494d64, 0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xb8c0e0, 0x5b6078,
        0xed8796, 0xa6da95, 0xeed49f, 0x8aadf4, 0xf5bde6, 0x8bd5ca, 0xa5adcb,
    ],
    0xf4dbd6,
);
pub(super) const CATPPUCCIN_MOCHA: ColorScheme = scheme(
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
pub(super) const fn scheme(fg: u32, bg: u32, ansi: [u32; 16], selection: u32) -> ColorScheme {
    let mut colors = [Color32::BLACK; 16];
    let mut i = 0;
    while i < ansi.len() {
        colors[i] = rgb(ansi[i]);
        i += 1;
    }
    ColorScheme {
        fg: rgb(fg),
        bg: rgb(bg),
        ansi: colors,
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

#[cfg(test)]
mod tests {
    use super::super::Theme;
    use super::*;

    #[test]
    fn catppuccin_mocha_uses_its_published_palette() {
        let scheme = Theme::CatppuccinMocha.scheme();
        assert_eq!(scheme.bg, Color32::from_rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(scheme.fg, Color32::from_rgb(0xcd, 0xd6, 0xf4));
        assert_eq!(scheme.ansi(1), Some(Color32::from_rgb(0xf3, 0x8b, 0xa8)));
        assert_eq!(scheme.ansi(16), None);
    }
}
