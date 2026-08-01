use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::NSFont;
use objc2_app_kit::NSStringDrawing;
use objc2_core_foundation::{CFArray, CFError, CFRetained, CFString, CFType, CFURL};
use objc2_core_text::{
    CTFontDescriptor, CTFontManagerCreateFontDescriptorsFromURL, CTFontManagerRegisterFontsForURL,
    CTFontManagerScope, kCTFontNameAttribute,
};
use objc2_foundation::{NSAttributedStringKey, NSDictionary, NSString, ns_string};
use std::collections::HashSet;
use std::sync::OnceLock;

use super::cast_any_object;

pub(super) fn load_terminal_font(size: f64) -> Retained<NSFont> {
    // Best-effort: make the bundled font usable even when not installed system-wide.
    let embedded_postscript_names = embedded_monaco_postscript_names();

    // Prefer the requested display name first.
    let preferred = ns_string!("Monaco Nerd Font Mono");
    if let Some(font) = NSFont::fontWithName_size(preferred, size) {
        return font;
    }

    // Fall back to internal/PostScript names extracted from the bundled fonts.
    for ps_name in embedded_postscript_names {
        let name = NSString::from_str(ps_name);
        if let Some(font) = NSFont::fontWithName_size(&name, size) {
            return font;
        }
    }

    NSFont::userFixedPitchFontOfSize(size).unwrap()
}

pub(super) fn measure_cell_size(font: &NSFont) -> (f64, f64) {
    let ascender = font.ascender();
    let descender = font.descender();
    let leading = font.leading();
    let cell_height = (ascender - descender + leading).ceil();

    let w_char = ns_string!("W");
    let attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
        NSDictionary::dictionaryWithObject_forKey(
            cast_any_object(font),
            ProtocolObject::from_ref(objc2_app_kit::NSFontAttributeName),
        )
    };
    let w_size = unsafe { w_char.sizeWithAttributes(Some(&*attrs)) };
    (w_size.width, cell_height)
}

static EMBEDDED_MONACO_POSTSCRIPT_NAMES: OnceLock<Vec<String>> = OnceLock::new();

fn embedded_monaco_postscript_names() -> &'static [String] {
    EMBEDDED_MONACO_POSTSCRIPT_NAMES.get_or_init(|| {
        let font_paths = [
            "assets/MonacoNerdFontMono-Regular.ttf",
            "assets/MonacoNerdFontMono-Bold.ttf",
            "assets/MonacoNerdFontMono-Italic.ttf",
            "assets/MonacoNerdFontMono-BoldItalic.ttf",
        ];

        let mut seen = HashSet::<String>::new();
        let mut postscript_names = Vec::<String>::new();

        for rel_path in font_paths {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel_path);
            if !path.exists() {
                continue;
            }

            let Some(url) = CFURL::from_file_path(&path) else {
                continue;
            };

            // Register the fonts for the current process (best-effort). If this fails due to
            // being already registered, we'll still try to extract PostScript names below.
            let mut error: *mut CFError = std::ptr::null_mut();
            unsafe {
                let _ =
                    CTFontManagerRegisterFontsForURL(&url, CTFontManagerScope::Process, &mut error);
            }

            for ps_name in monaco_postscript_names_from_url(&url) {
                if seen.insert(ps_name.clone()) {
                    postscript_names.push(ps_name);
                }
            }
        }

        postscript_names
    })
}

fn monaco_postscript_names_from_url(url: &CFURL) -> Vec<String> {
    let Some(descriptors) = (unsafe { CTFontManagerCreateFontDescriptorsFromURL(url) }) else {
        return Vec::new();
    };
    let descriptors: CFRetained<CFArray<CTFontDescriptor>> =
        unsafe { CFRetained::cast_unchecked(descriptors) };

    let mut out = Vec::<String>::new();
    for idx in 0..descriptors.len() {
        // SAFETY: `idx` is in bounds, and `descriptors` isn't mutated.
        let desc = unsafe { descriptors.get_unchecked(idx as isize) };
        if let Some(ps_name) = postscript_name_from_descriptor(desc) {
            out.push(ps_name);
        }
    }
    out
}

fn postscript_name_from_descriptor(desc: &CTFontDescriptor) -> Option<String> {
    let value: CFRetained<CFType> = unsafe { desc.attribute(kCTFontNameAttribute) }?;
    let value: CFRetained<CFString> = value.downcast().ok()?;
    Some(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_default_terminal_font_prefers_monaco_nerd_font_mono() {
        let font = load_terminal_font(14.0);
        let name = font.fontName().to_string();
        assert!(
            name.contains("Monaco Nerd Font Mono")
                || name.contains("MonacoNerdFontMono")
                || name.contains("MonacoNFM"),
            "expected Monaco Nerd Font Mono, got: {name}"
        );
    }
}
