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

    pub const BLACK: Self = Self::from_rgb(0, 0, 0);
    pub const WHITE: Self = Self::from_rgb(255, 255, 255);
}

pub(crate) const DEFAULT_FG: Color32 = Color32::from_rgb(204, 204, 204);
pub(crate) const DEFAULT_BG: Color32 = Color32::BLACK;

// Lookup table for RGB levels used in xterm 256 color mode
const RGB_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

#[inline]
pub(crate) fn ansi_16_color(index: u8) -> Color32 {
    match index {
        0 => Color32::from_rgb(0, 0, 0),
        1 => Color32::from_rgb(128, 0, 0),
        2 => Color32::from_rgb(0, 128, 0),
        3 => Color32::from_rgb(128, 128, 0),
        4 => Color32::from_rgb(0, 0, 128),
        5 => Color32::from_rgb(128, 0, 128),
        6 => Color32::from_rgb(0, 128, 128),
        7 => Color32::from_rgb(192, 192, 192),
        8 => Color32::from_rgb(128, 128, 128),
        9 => Color32::from_rgb(255, 0, 0),
        10 => Color32::from_rgb(0, 255, 0),
        11 => Color32::from_rgb(255, 255, 0),
        12 => Color32::from_rgb(0, 0, 255),
        13 => Color32::from_rgb(255, 0, 255),
        14 => Color32::from_rgb(0, 255, 255),
        _ => Color32::from_rgb(255, 255, 255),
    }
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
