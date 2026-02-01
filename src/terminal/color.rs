use eframe::egui;

pub(crate) const DEFAULT_FG: egui::Color32 = egui::Color32::WHITE;
pub(crate) const DEFAULT_BG: egui::Color32 = egui::Color32::BLACK;

// Lookup table for RGB levels used in xterm 256 color mode
const RGB_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

#[inline]
pub(crate) fn ansi_16_color(index: u8) -> egui::Color32 {
    match index {
        0 => egui::Color32::from_rgb(0, 0, 0),
        1 => egui::Color32::from_rgb(128, 0, 0),
        2 => egui::Color32::from_rgb(0, 128, 0),
        3 => egui::Color32::from_rgb(128, 128, 0),
        4 => egui::Color32::from_rgb(0, 0, 128),
        5 => egui::Color32::from_rgb(128, 0, 128),
        6 => egui::Color32::from_rgb(0, 128, 128),
        7 => egui::Color32::from_rgb(192, 192, 192),
        8 => egui::Color32::from_rgb(128, 128, 128),
        9 => egui::Color32::from_rgb(255, 0, 0),
        10 => egui::Color32::from_rgb(0, 255, 0),
        11 => egui::Color32::from_rgb(255, 255, 0),
        12 => egui::Color32::from_rgb(0, 0, 255),
        13 => egui::Color32::from_rgb(255, 0, 255),
        14 => egui::Color32::from_rgb(0, 255, 255),
        _ => egui::Color32::from_rgb(255, 255, 255),
    }
}

#[inline]
pub(crate) fn xterm_256_color(index: u8) -> egui::Color32 {
    if index < 16 {
        return ansi_16_color(index);
    }
    if index < 232 {
        let idx = index - 16;
        let r = idx / 36;
        let g = (idx / 6) % 6;
        let b = idx % 6;
        return egui::Color32::from_rgb(
            RGB_LEVELS[r as usize],
            RGB_LEVELS[g as usize],
            RGB_LEVELS[b as usize],
        );
    }
    let gray = 8u8.saturating_add((index - 232).saturating_mul(10));
    egui::Color32::from_rgb(gray, gray, gray)
}
