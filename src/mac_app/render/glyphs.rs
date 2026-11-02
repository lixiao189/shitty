//! Shaped, cell-anchored glyph lookup for the renderer.
//!
//! The terminal paints every glyph at the column its characters occupy, never
//! at the x the font's layout happens to produce. That distinction is what
//! makes the grid authoritative: characters the bundled font does not cover
//! are drawn by whichever face Core Text falls back to — CJK, half-width
//! katakana, emoji, symbol fonts — and those faces advance by their own
//! metrics, never by exactly the number of columns the grid reserved. Laid out
//! as one string, such a run slides away from the grid and from the cursor the
//! longer it gets.
//!
//! So the string is still *shaped* here, because shaping is what joins a base
//! letter to its combining mark, applies an emoji's variation selector, and
//! turns `ffi` into a ligature when the font has one — but only the glyphs are
//! kept. The positions Core Text computed are discarded, and the renderer
//! re-derives each glyph's column from the grid.
//!
//! Results are memoized per string, so repeated frames cost nothing and only a
//! cache miss pays for a Core Text round trip.

use objc2_app_kit::NSFont;
use objc2_core_foundation::{CFRange, CFRetained, CFString, CFType};
use objc2_core_graphics::CGGlyph;
use objc2_core_text::{CTFont, CTLine, CTRun, kCTFontAttributeName, kCTFontPostScriptNameKey};
use objc2_foundation::{NSAttributedString, NSAttributedStringKey, NSDictionary, NSString};
use std::collections::HashMap;
use std::ptr::NonNull;
use std::rc::Rc;

use objc2::runtime::AnyObject;

/// Attributes handed to Core Text while shaping.
pub(in crate::mac_app) type TextAttributes = NSDictionary<NSAttributedStringKey, AnyObject>;

/// Index of a face inside a [`GlyphCache`]: the base font is always face 0 and
/// fallback faces are appended in first-use order.
pub(in crate::mac_app) type FaceId = usize;

/// Number of shaped strings kept before the cache is dropped wholesale; a
/// terminal screen holds far fewer distinct runs than this, so the cache
/// normally only grows to the size of the screen's text.
const MAX_SHAPED_TEXTS: usize = 4096;

/// One glyph of a shaped string.
#[derive(Clone, Copy, Debug)]
pub(in crate::mac_app) struct ShapedGlyph {
    /// The face that draws this glyph.
    pub(in crate::mac_app) face: FaceId,
    pub(in crate::mac_app) id: CGGlyph,
    /// UTF-16 index of the first character the glyph covers; the glyph covers
    /// everything up to the next glyph's index.
    pub(in crate::mac_app) start: usize,
    /// The face's own advance for the glyph in points. Only used to decide how
    /// the glyph fits the columns its characters were given.
    pub(in crate::mac_app) advance: f64,
}

/// The glyphs of one shaped string, in visual order.
pub(in crate::mac_app) struct ShapedText {
    pub(in crate::mac_app) glyphs: Vec<ShapedGlyph>,
    /// Length of the shaped string in UTF-16 code units.
    pub(in crate::mac_app) utf16_len: usize,
}

/// Shapes strings and remembers which face Core Text picks for each glyph.
pub(in crate::mac_app) struct GlyphCache {
    /// Face 0 is the base font; the rest are whatever the fallback picked.
    faces: Vec<CFRetained<CTFont>>,
    /// Face lookup for fallbacks, keyed by PostScript name so that two weights
    /// of one family stay distinct.
    face_ids: HashMap<String, FaceId>,
    shaped: HashMap<String, Rc<ShapedText>>,
}

impl GlyphCache {
    /// Build a cache whose base face is `font` — the same face the cell
    /// metrics were measured from.
    pub(in crate::mac_app) fn new(font: &NSFont) -> Self {
        // NSFont and CTFont are toll-free bridged, so this really is the face
        // `measure_cell_size` measured rather than a lookalike by name.
        let base: CFRetained<CTFont> = unsafe {
            let ptr: *const NSFont = font;
            CFRetained::retain(NonNull::new_unchecked(ptr as *mut CTFont))
        };
        // Seed the base face so shaped runs map back onto face 0 instead of
        // registering the same face again.
        let mut face_ids = HashMap::new();
        face_ids.insert(face_key(&base), 0);
        Self {
            faces: vec![base],
            face_ids,
            shaped: HashMap::new(),
        }
    }

    /// The face at `id`, for drawing a batch of glyphs it shaped.
    pub(in crate::mac_app) fn face(&self, id: FaceId) -> &CTFont {
        &self.faces[id]
    }

    /// Shape `text` with `attributes`, reusing the result for repeated text.
    ///
    /// The attributes carry the font to shape with; their colors are ignored,
    /// since the renderer paints each run in the color it sets on the context.
    pub(in crate::mac_app) fn shape(
        &mut self,
        text: &str,
        attributes: &TextAttributes,
    ) -> Rc<ShapedText> {
        if let Some(cached) = self.shaped.get(text) {
            return Rc::clone(cached);
        }
        let shaped = Rc::new(self.shape_uncached(text, attributes));
        if self.shaped.len() >= MAX_SHAPED_TEXTS {
            self.shaped.clear();
        }
        self.shaped.insert(text.to_owned(), Rc::clone(&shaped));
        shaped
    }

    fn shape_uncached(&mut self, text: &str, attributes: &TextAttributes) -> ShapedText {
        let string = NSString::from_str(text);
        let attributed = unsafe { NSAttributedString::new_with_attributes(&string, attributes) };
        // NSAttributedString and CFAttributedString are toll-free bridged.
        let attributed: CFRetained<objc2_core_foundation::CFAttributedString> = unsafe {
            CFRetained::retain(NonNull::new_unchecked(
                objc2::rc::Retained::as_ptr(&attributed)
                    as *mut objc2_core_foundation::CFAttributedString,
            ))
        };
        let line = unsafe { CTLine::with_attributed_string(&attributed) };
        let runs: CFRetained<objc2_core_foundation::CFArray<CTRun>> =
            unsafe { CFRetained::cast_unchecked(line.glyph_runs()) };

        let mut glyphs = Vec::new();
        for run in runs.iter() {
            let face = self.face_of(&run);
            let count = unsafe { run.glyph_count() } as usize;
            if count == 0 {
                continue;
            }
            let whole = CFRange {
                location: 0,
                length: 0,
            };
            let mut ids = vec![0 as CGGlyph; count];
            let mut advances = vec![objc2_core_foundation::CGSize::new(0.0, 0.0); count];
            let mut indices = vec![0isize; count];
            unsafe {
                run.glyphs(whole, NonNull::new_unchecked(ids.as_mut_ptr()));
                run.advances(whole, NonNull::new_unchecked(advances.as_mut_ptr()));
                run.string_indices(whole, NonNull::new_unchecked(indices.as_mut_ptr()));
            }
            glyphs.extend((0..count).map(|i| ShapedGlyph {
                face,
                id: ids[i],
                start: indices[i].max(0) as usize,
                advance: advances[i].width,
            }));
        }

        ShapedText {
            glyphs,
            utf16_len: string.length(),
        }
    }

    /// Register the face a shaped run was laid out with; fallback faces arrive
    /// here instead of being resolved by hand.
    fn face_of(&mut self, run: &CTRun) -> FaceId {
        let attributes: CFRetained<objc2_core_foundation::CFDictionary<CFString, CFType>> =
            unsafe { CFRetained::cast_unchecked(run.attributes()) };
        let Some(font) = attributes
            .get(unsafe { kCTFontAttributeName })
            .and_then(|value| value.downcast::<CTFont>().ok())
        else {
            return 0;
        };
        let key = face_key(&font);
        match self.face_ids.get(&key) {
            Some(&existing) => existing,
            None => {
                let id = self.faces.len();
                self.faces.push(font);
                self.face_ids.insert(key, id);
                id
            }
        }
    }
}

/// Identity of a face for batching purposes. The PostScript name separates
/// weights and styles that a family name would collapse into one; the family
/// name is only a fallback for faces that do not publish one.
fn face_key(font: &CTFont) -> String {
    let postscript = unsafe {
        font.font_descriptor()
            .attribute(kCTFontPostScriptNameKey)
            .and_then(|value| value.downcast::<CFString>().ok())
            .map(|name| name.to_string())
    };
    match postscript {
        Some(name) if !name.is_empty() => name,
        _ => unsafe { font.family_name() }.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac_app::cast_any_object;
    use crate::mac_app::font::{load_terminal_font, measure_cell_size};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::NSFontAttributeName;
    use objc2_foundation::NSMutableDictionary;

    fn attributes_for(
        font: &NSFont,
    ) -> Retained<NSMutableDictionary<NSAttributedStringKey, AnyObject>> {
        let font_only: Retained<TextAttributes> = unsafe {
            NSDictionary::dictionaryWithObject_forKey(
                cast_any_object(font),
                ProtocolObject::from_ref(NSFontAttributeName),
            )
        };
        NSMutableDictionary::dictionaryWithDictionary(&font_only)
    }

    fn cache() -> (GlyphCache, Retained<NSFont>) {
        let font = load_terminal_font(14.0);
        let attributes = attributes_for(&font);
        let mut cache = GlyphCache::new(&font);
        // Keep the attributes alive for the caller by shape()ing through them.
        let shaped = cache.shape(" ", &attributes);
        assert_eq!(shaped.glyphs.len(), 1);
        (cache, font)
    }

    #[test]
    fn ascii_shapes_to_one_base_font_glyph_per_character() {
        let (mut cache, font) = cache();
        let attributes = attributes_for(&font);
        let shaped = cache.shape("abc", &attributes);
        assert_eq!(shaped.utf16_len, 3);
        assert_eq!(shaped.glyphs.len(), 3);
        for (i, glyph) in shaped.glyphs.iter().enumerate() {
            assert_eq!(glyph.face, 0, "ascii must come from the base font");
            assert_eq!(glyph.start, i);
        }
        // A monospace face advances by exactly one cell, so the glyph fills
        // its column and needs no fitting.
        let (cell_width, _) = measure_cell_size(&font);
        assert!((shaped.glyphs[0].advance - cell_width).abs() < 0.001);
    }

    #[test]
    fn cjk_shapes_to_fallback_glyphs_that_advance_one_em() {
        let (mut cache, font) = cache();
        let attributes = attributes_for(&font);
        let shaped = cache.shape("中文", &attributes);
        assert_eq!(shaped.glyphs.len(), 2);
        assert_eq!(shaped.glyphs[0].start, 0);
        assert_eq!(shaped.glyphs[1].start, 1);
        let (cell_width, _) = measure_cell_size(&font);
        for glyph in &shaped.glyphs {
            assert_ne!(glyph.face, 0, "CJK comes from a fallback face");
            // One em, i.e. narrower than the two columns the grid gives it.
            assert!(glyph.advance < 2.0 * cell_width);
        }
        // Both characters share one face, so a row of CJK is one draw call.
        assert_eq!(shaped.glyphs[0].face, shaped.glyphs[1].face);
    }

    /// Everything a TUI draws — box drawing, corner pieces, blocks, shades and
    /// Nerd Font icons — is part of the bundled face, so it advances by exactly
    /// one cell and the renderer's fit policy is a no-op for it. That is what
    /// keeps nvim, tmux and htop pixel-identical whatever the policy is.
    #[test]
    fn cell_filling_glyphs_advance_exactly_their_cell() {
        let (mut cache, font) = cache();
        let attributes = attributes_for(&font);
        let (cell_width, _) = measure_cell_size(&font);
        for text in [
            "W", "\u{2500}", // light horizontal
            "\u{250c}", // light down and right
            "\u{2510}", "\u{2588}", // full block
            "\u{2580}", // upper half block
            "\u{2591}", // light shade
            "\u{f015}", // Nerd Font icon
        ] {
            let shaped = cache.shape(text, &attributes);
            assert_eq!(shaped.glyphs.len(), 1, "{text:?}");
            let glyph = shaped.glyphs[0];
            assert_eq!(glyph.face, 0, "{text:?} must come from the bundled face");
            assert!(
                (glyph.advance - cell_width).abs() < 0.002,
                "{text:?} advances {}pt but a cell is {cell_width}pt: the fit policy would move it",
                glyph.advance
            );
        }
    }

    #[test]
    fn shaped_strings_are_cached_by_text() {
        let (mut cache, font) = cache();
        let attributes = attributes_for(&font);
        let first = cache.shape("hello", &attributes);
        let second = cache.shape("hello", &attributes);
        assert!(Rc::ptr_eq(&first, &second), "same text must be reused");
        assert_eq!(cache.shaped.len(), 2); // the warm-up space plus "hello"
    }

    #[test]
    fn ligature_glyphs_span_the_characters_they_replace() {
        // Hoefler Text ligates "ffi" into a single glyph; if the mechanism
        // cannot express that, the renderer would place the ligature in one
        // column and stretch it over one cell.
        let Some(font) = NSFont::fontWithName_size(&NSString::from_str("Hoefler Text"), 14.0)
        else {
            panic!("Hoefler Text ships with macOS");
        };
        let attributes = attributes_for(&font);
        let mut cache = GlyphCache::new(&font);
        let shaped = cache.shape("ffi", &attributes);
        assert_eq!(shaped.glyphs.len(), 1, "expected one ligature glyph");
        // The ligature starts at the first character and therefore covers all
        // three columns until the end of the string.
        assert_eq!(shaped.glyphs[0].start, 0);
        assert_eq!(shaped.utf16_len, 3);
        assert!(shaped.glyphs[0].advance > 0.0);
    }

    #[test]
    fn combining_marks_are_absorbed_into_their_base_character() {
        // Shaping decides this; the renderer only needs the glyph to keep
        // pointing at the first character it covers.
        let (mut cache, font) = cache();
        let attributes = attributes_for(&font);
        let shaped = cache.shape("e\u{301}", &attributes);
        assert_eq!(shaped.utf16_len, 2);
        assert_eq!(
            shaped.glyphs.len(),
            1,
            "a precomposed font must merge base and mark"
        );
        assert_eq!(shaped.glyphs[0].start, 0);
    }

    #[test]
    fn emoji_need_fitting_but_still_resolve_to_a_real_glyph() {
        let (mut cache, font) = cache();
        let attributes = attributes_for(&font);
        let (cell_width, _) = measure_cell_size(&font);
        for (text, columns) in [("\u{1f600}", 2.0), ("\u{2764}\u{fe0f}", 1.0)] {
            let shaped = cache.shape(text, &attributes);
            assert_eq!(shaped.glyphs.len(), 1, "{text:?}");
            let glyph = shaped.glyphs[0];
            assert_ne!(glyph.face, 0, "emoji come from a fallback face");
            assert!(
                glyph.advance > columns * cell_width,
                "{text:?} advances {}pt, wider than its {columns} column(s)",
                glyph.advance
            );
        }
    }
}
