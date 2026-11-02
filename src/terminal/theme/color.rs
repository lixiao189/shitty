// Platform-independent color type (RGBA)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Color32 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color32 {
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn from_rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const BLACK: Self = Self::from_rgb(0, 0, 0);
    pub const WHITE: Self = Self::from_rgb(255, 255, 255);
}

// Lookup table for RGB levels used in xterm 256 color mode
const RGB_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// VGA colors for indexes 0–15. Also the unthemed ANSI palette.
pub(crate) const ANSI_16: [Color32; 16] = [
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
];

#[inline]
pub(crate) fn ansi_16_color(index: u8) -> Color32 {
    ANSI_16
        .get(index as usize)
        .copied()
        .unwrap_or(Color32::WHITE)
}

#[inline]
pub(crate) fn xterm_256_color(index: u8) -> Color32 {
    if index < 16 {
        return ansi_16_color(index);
    }
    if index < 232 {
        let idx = index - 16;
        let r = idx / 36;
        let g = (idx / 6) % 6;
        let b = idx % 6;
        return Color32::from_rgb(
            RGB_LEVELS[r as usize],
            RGB_LEVELS[g as usize],
            RGB_LEVELS[b as usize],
        );
    }
    let gray = 8u8.saturating_add((index - 232).saturating_mul(10));
    Color32::from_rgb(gray, gray, gray)
}
