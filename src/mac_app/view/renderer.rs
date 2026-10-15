//! Painting of the terminal surface: cells, cursor, selection, and the
//! in-progress input-method text.

use crate::terminal::color::Color32;
use crate::terminal::grid::{CursorInfo, display_cells};
use crate::terminal::selection::Selection;
use crate::terminal::session::TerminalSession;
use alacritty_terminal::vte::ansi::CursorShape;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSGraphicsContext,
};
use objc2_core_foundation::{CGAffineTransform, CGPoint};
use objc2_core_graphics::{CGContext, CGGlyph};
use objc2_foundation::{
    NSAttributedStringKey, NSDictionary, NSMutableDictionary, NSPoint, NSRect, NSSize,
};
use std::ptr::NonNull;
use std::sync::OnceLock;

use super::geometry::terminal_bounds;
use super::glyphs::{FaceId, GlyphCache, ShapedGlyph, ShapedText, TextAttributes};
use super::state::TerminalViewState;
use crate::mac_app::cast_any_object;

/// Paint the active session: background, selection, cells, cursor, and the
/// marked text of an in-progress composition.
pub(super) fn paint_terminal(state: &TerminalViewState, bounds: NSRect, scale: f64) {
    let terminal = terminal_bounds(bounds, state.show_tab_bar());
    let session = state.active();

    to_nscolor(session.grid.default_bg()).set();
    NSBezierPath::fillRect(terminal);

    if let Some(selection) = &session.selection {
        paint_selection_overlay(
            session,
            selection,
            terminal,
            scale,
            state.cell_width,
            state.cell_height,
        );
    }

    // Text is drawn as glyphs, which need the context Core Text draws into.
    let context = NSGraphicsContext::currentContext().map(|context| context.CGContext());
    // One attribute set for the whole frame: shaping only needs the font, and
    // each run's color is set on the context as it is painted.
    let attributes = text_attributes(&state.font, &to_nscolor(Color32::WHITE));
    let mut glyphs = state.glyphs.borrow_mut();
    paint_cells(
        state,
        &mut glyphs,
        &attributes,
        context.as_deref(),
        terminal,
        scale,
    );

    if let Some(cursor) = session.grid.cursor() {
        paint_cursor(
            state,
            &mut glyphs,
            &attributes,
            context.as_deref(),
            &cursor,
            terminal,
            scale,
        );
    }

    if state.has_marked_text() {
        paint_marked_text(
            state,
            &mut glyphs,
            &attributes,
            context.as_deref(),
            terminal,
            scale,
        );
    }
}

/// Paint one run per same-styled group of cells.
///
/// Every glyph is anchored to the columns its characters occupy, never to the
/// advance the font reports: a face the system falls back to (CJK, emoji,
/// symbol fonts) advances by its own metrics, which is not the number of
/// columns the grid reserved, so one span of text would slide out of step with
/// the grid the further right it reaches.
fn paint_cells(
    state: &TerminalViewState,
    glyphs: &mut GlyphCache,
    attributes: &TextAttributes,
    context: Option<&CGContext>,
    bounds: NSRect,
    scale: f64,
) {
    let origin = bounds.origin;
    let session = state.active();
    let default_bg = session.grid.default_bg();
    let font_height = state.font.ascender() - state.font.descender();

    for row in 0..session.grid.rows() {
        let y0 = snap_to_pixel(
            origin.y + bounds.size.height - ((row + 1) as f64 * state.cell_height),
            scale,
        );
        let y1 = snap_to_pixel(
            origin.y + bounds.size.height - (row as f64 * state.cell_height),
            scale,
        );
        let rect_h = (y1 - y0).max(0.0);
        // Top of the line box; glyphs hang from the baseline below it.
        let text_y = snap_to_pixel(y0 + (rect_h - font_height) / 2.0, scale);

        for run in session.grid.row_runs(row) {
            let x0 = snap_to_pixel(origin.x + run.col as f64 * state.cell_width, scale);
            let x1 = snap_to_pixel(
                origin.x + (run.col + run.width) as f64 * state.cell_width,
                scale,
            );
            let rect_w = (x1 - x0).max(0.0);
            let rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, rect_h));

            if run.bg != default_bg {
                to_nscolor(run.bg).set();
                NSBezierPath::fillRect(rect);
            }

            if !run.text.is_empty() && !run.text.trim().is_empty() {
                to_nscolor(run.fg).set();
                TextPainter {
                    state,
                    glyphs,
                    attributes,
                    context,
                    scale,
                    origin_x: origin.x,
                    text_y,
                }
                .draw(&run.text, run.col);
            }

            if run.underline {
                // y0 is the cell bottom in NSView coords (y-up), so the underline sits there.
                let underline_rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, 1.0));
                to_nscolor(run.fg).set();
                NSBezierPath::fillRect(underline_rect);
            }
        }
    }
}

/// How a glyph that does not fill the columns it was given is placed in them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlyphFit {
    /// Left edge on the cell's left edge: the terminal default.
    Left,
    /// Split the leftover space evenly, like the glyph's own em box would.
    Center,
    /// Widen the glyph until it fills its columns exactly.
    Stretch,
}

/// What happens to a glyph wider than the columns it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlyphOverflow {
    /// Squeeze it horizontally so it cannot cover the neighbouring cell.
    Clamp,
    /// Draw it at its natural width, overlapping whatever is to its right.
    Allow,
}

/// Placement policy for glyphs whose advance is not exactly their columns.
///
/// The bundled font is monospace, so this only shows up for text the system
/// falls back on: CJK and half-width kana (narrower than the columns the grid
/// gave them) and emoji (wider). A glyph that advances exactly one cell — all
/// of ASCII, box drawing, blocks and Nerd Font icons — is never touched, so a
/// TUI looks the same whatever this is set to.
///
/// The default is `Left`, which is what the reference terminals do: kitty puts
/// CJK ink on the column boundary (measured here: starting at 2.008, 4.024,
/// 6.024 columns, ink 1.42-1.54 cells wide), and alacritty and xterm are the
/// same. It also keeps a Chinese character's left edge level with the ASCII
/// and box drawing next to it, because every glyph then starts where its
/// column starts.
///
/// `SHITTY_GLYPH_FIT=center|stretch` and `SHITTY_GLYPH_OVERFLOW=allow` switch
/// the alternatives on for a run of the process, without a rebuild.
fn glyph_policy() -> (GlyphFit, GlyphOverflow) {
    static POLICY: OnceLock<(GlyphFit, GlyphOverflow)> = OnceLock::new();
    *POLICY.get_or_init(|| {
        let fit = match std::env::var("SHITTY_GLYPH_FIT").as_deref() {
            Ok("center") => GlyphFit::Center,
            Ok("stretch") => GlyphFit::Stretch,
            _ => GlyphFit::Left,
        };
        let overflow = match std::env::var("SHITTY_GLYPH_OVERFLOW").as_deref() {
            Ok("allow") => GlyphOverflow::Allow,
            _ => GlyphOverflow::Clamp,
        };
        (fit, overflow)
    })
}

/// Offset and horizontal scale that place a glyph of `advance` points inside a
/// box of `box_width` points — the columns the grid gave its characters.
fn fit_glyph(fit: GlyphFit, overflow: GlyphOverflow, advance: f64, box_width: f64) -> (f64, f64) {
    if advance <= 0.0 || box_width <= 0.0 {
        // Zero-width: a combining mark standing in its own cell. Keep the
        // face's own scale and let it sit on the character it marks.
        return (0.0, 1.0);
    }
    let wide = advance > box_width * (1.0 + OVERFLOW_TOLERANCE);
    let scale = if fit == GlyphFit::Stretch || (wide && overflow == GlyphOverflow::Clamp) {
        box_width / advance
    } else {
        1.0
    };
    let offset = match fit {
        GlyphFit::Left => 0.0,
        // Centring uses only the space left over; a glyph wider than its box
        // grows to the right rather than over the cell before it.
        GlyphFit::Center | GlyphFit::Stretch => (box_width - advance * scale).max(0.0) / 2.0,
    };
    (offset, scale)
}

/// Draws text with every glyph anchored to the columns its characters occupy.
///
/// Core Text still shapes the text — that is what joins a base letter to its
/// combining mark, applies an emoji's variation selector, and forms a ligature
/// when the font has one — but only the glyphs are kept: their positions are
/// re-derived from the grid, so no font metric can move text off the columns
/// the grid assigned it.
struct TextPainter<'a> {
    state: &'a TerminalViewState,
    glyphs: &'a mut GlyphCache,
    /// Attributes to shape with; only their font matters.
    attributes: &'a TextAttributes,
    context: Option<&'a CGContext>,
    scale: f64,
    /// Left edge of the terminal area, in view coordinates.
    origin_x: f64,
    /// Top of the line box: where a string would be drawn from.
    text_y: f64,
}

impl TextPainter<'_> {
    /// Draw `text` starting at column `start_col`.
    ///
    /// Consecutive glyphs drawn by the same face at the same scale share one
    /// call, so an ASCII line costs a single draw, and a line of CJK costs one
    /// draw for its base font plus one per fallback face — a whole screen of
    /// CJK is dozens of calls rather than thousands.
    fn draw(&mut self, text: &str, start_col: usize) {
        // Glyphs hang from the baseline, which sits a descender below the line
        // box that a string would be drawn from.
        let baseline = self.text_y - self.state.font.descender();
        let cell_width = self.state.cell_width;
        let shaped = self.glyphs.shape(text, self.attributes);
        let columns = self.columns(text, start_col);
        let mut batch = GlyphBatch::default();

        for (glyph, (start, end)) in shaped.glyphs.iter().zip(glyph_spans(&shaped)) {
            let from = columns[start.min(columns.len() - 1)];
            let to = columns[end.min(columns.len() - 1)].max(from);
            let box_width = (to - from) as f64 * cell_width;
            let x = snap_to_pixel(self.origin_x + from as f64 * cell_width, self.scale);
            batch.push(self.glyphs, self.context, glyph, x, box_width, baseline);
        }

        batch.flush(self.glyphs, self.context);
    }

    /// The column of every UTF-16 boundary in `text`, counting from
    /// `start_col` — the same widths the grid used to place the cells.
    fn columns(&self, text: &str, start_col: usize) -> Vec<usize> {
        let mut columns = Vec::with_capacity(text.encode_utf16().count() + 1);
        let mut col = start_col;
        for ch in text.chars() {
            for _ in 0..ch.len_utf16() {
                columns.push(col);
            }
            col += display_cells(ch);
        }
        columns.push(col);
        columns
    }
}

/// The range of UTF-16 indexes each glyph covers, in the order Core Text
/// returned the glyphs.
///
/// Right-to-left runs come back in visual order, so "the next glyph in the
/// list" is the one *before* it in the text: taking the span from the sorted
/// indexes keeps a ligature or a joined form stretched over its own characters
/// instead of over everything up to the end of the run.
fn glyph_spans(shaped: &ShapedText) -> Vec<(usize, usize)> {
    let mut starts: Vec<usize> = shaped.glyphs.iter().map(|glyph| glyph.start).collect();
    starts.sort_unstable();
    shaped
        .glyphs
        .iter()
        .map(|glyph| {
            let end = starts
                .binary_search(&glyph.start)
                .ok()
                .and_then(|index| starts.get(index + 1).copied())
                .unwrap_or(shaped.utf16_len);
            (glyph.start, end.max(glyph.start))
        })
        .collect()
}

/// Overflow below this fraction of a box is left alone. A glyph a third of a
/// pixel too wide is invisible, and squeezing it would only soften the glyph
/// and split it into a scaled batch of its own — which is what a fallback face
/// that advances 8.429pt instead of 8.401pt would otherwise cost.
const OVERFLOW_TOLERANCE: f64 = 0.01;

/// The neutral text matrix. Spelled out because `CGAffineTransform::default()`
/// in objc2-core-foundation is all zeros, which would make text vanish.
const IDENTITY_TRANSFORM: CGAffineTransform = CGAffineTransform {
    a: 1.0,
    b: 0.0,
    c: 0.0,
    d: 1.0,
    tx: 0.0,
    ty: 0.0,
};

/// Glyphs waiting to be drawn, all from one face at one scale: Core Text draws
/// a batch with a single font, so a change of either draws what came before it
/// first.
#[derive(Default)]
struct GlyphBatch {
    face: FaceId,
    /// Horizontal scale to draw the queued glyphs with; 1.0 is the face's own.
    scale: f64,
    ids: Vec<CGGlyph>,
    positions: Vec<CGPoint>,
    open: bool,
}

impl GlyphBatch {
    /// Queue `glyph`, fitted into a box of `box_width` starting at `x`.
    fn push(
        &mut self,
        glyphs: &GlyphCache,
        context: Option<&CGContext>,
        glyph: &ShapedGlyph,
        x: f64,
        box_width: f64,
        baseline: f64,
    ) {
        let (fit, overflow) = glyph_policy();
        let (offset, scale) = fit_glyph(fit, overflow, glyph.advance, box_width);
        if self.open && (self.face != glyph.face || self.scale != scale) {
            self.flush(glyphs, context);
        }
        self.face = glyph.face;
        self.scale = scale;
        self.open = true;
        self.ids.push(glyph.id);
        self.positions.push(CGPoint::new(x + offset, baseline));
    }

    /// Draw everything queued so far.
    fn flush(&mut self, glyphs: &GlyphCache, context: Option<&CGContext>) {
        if self.ids.is_empty() {
            return;
        }
        if let Some(context) = context {
            // Core Text adds the context's text position to the positions given
            // here, and string drawing (`drawAtPoint:`, used by the tab bar)
            // leaves that position advanced by the width it drew. Reset it, or
            // glyphs land at the wrong x.
            CGContext::set_text_position(Some(context), 0.0, 0.0);
            let scaled = self.scale != 1.0;
            if scaled {
                // The matrix stretches what is drawn; positions live in the
                // same space, so undo it on x and the left edge stays put.
                for position in &mut self.positions {
                    position.x /= self.scale;
                }
                CGContext::set_text_matrix(
                    Some(context),
                    CGAffineTransform {
                        a: self.scale,
                        ..IDENTITY_TRANSFORM
                    },
                );
            }
            unsafe {
                glyphs.face(self.face).draw_glyphs(
                    NonNull::new_unchecked(self.ids.as_mut_ptr()),
                    NonNull::new_unchecked(self.positions.as_mut_ptr()),
                    self.ids.len(),
                    context,
                )
            };
            if scaled {
                // Note: `CGAffineTransform::default()` is the zero matrix, not
                // the identity — using it here would erase every glyph drawn
                // after this batch.
                CGContext::set_text_matrix(Some(context), IDENTITY_TRANSFORM);
            }
        }
        self.ids.clear();
        self.positions.clear();
    }
}

/// Paint the cursor in the shape the shell selected with DECSCUSR.
fn paint_cursor(
    state: &TerminalViewState,
    glyphs: &mut GlyphCache,
    attributes: &TextAttributes,
    context: Option<&CGContext>,
    cursor: &CursorInfo,
    bounds: NSRect,
    scale: f64,
) {
    let origin = bounds.origin;
    // A wide character is covered by two columns, so a block cursor over it has
    // to be two columns wide: one column would hide only half the character.
    let cursor_cols = if cursor.wide { 2 } else { 1 };
    let cursor_x0 = snap_to_pixel(origin.x + cursor.col as f64 * state.cell_width, scale);
    let cursor_x1 = snap_to_pixel(
        origin.x + (cursor.col + cursor_cols) as f64 * state.cell_width,
        scale,
    );
    let cursor_y0 = snap_to_pixel(
        origin.y + bounds.size.height - ((cursor.row + 1) as f64 * state.cell_height),
        scale,
    );
    let cursor_y1 = snap_to_pixel(
        origin.y + bounds.size.height - (cursor.row as f64 * state.cell_height),
        scale,
    );
    let cell_w = (cursor_x1 - cursor_x0).max(0.0);
    let cell_h = (cursor_y1 - cursor_y0).max(0.0);
    to_nscolor(cursor.bg).set();

    match cursor.shape {
        CursorShape::Block => {
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(cell_w, cell_h),
            ));
            if !cursor.text.is_empty() {
                let font_height = state.font.ascender() - state.font.descender();
                let text_y = snap_to_pixel(cursor_y0 + (cell_h - font_height) / 2.0, scale);
                to_nscolor(cursor.fg).set();
                TextPainter {
                    state,
                    glyphs,
                    attributes,
                    context,
                    scale,
                    origin_x: origin.x,
                    text_y,
                }
                .draw(&cursor.text, cursor.col);
            }
        }
        CursorShape::Beam => {
            // The bar marks the leading edge of the cell, so it stays one cell
            // thick even when the cursor covers a wide character.
            let bar_width = snap_to_pixel((state.cell_width / 6.0).max(1.0), scale);
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(bar_width, cell_h),
            ));
        }
        CursorShape::Underline => {
            let bar_height = snap_to_pixel((cell_h / 8.0).max(1.0), scale);
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(cell_w, bar_height),
            ));
        }
        CursorShape::HollowBlock => {
            let thickness = snap_to_pixel(1.0, scale);
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(cell_w, thickness),
            ));
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y1 - thickness),
                NSSize::new(cell_w, thickness),
            ));
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x0, cursor_y0),
                NSSize::new(thickness, cell_h),
            ));
            NSBezierPath::fillRect(NSRect::new(
                NSPoint::new(cursor_x1 - thickness, cursor_y0),
                NSSize::new(thickness, cell_h),
            ));
        }
        CursorShape::Hidden => {}
    }
}

/// Draw the composition text at the cursor with an underline.
fn paint_marked_text(
    state: &TerminalViewState,
    glyphs: &mut GlyphCache,
    attributes: &TextAttributes,
    context: Option<&CGContext>,
    bounds: NSRect,
    scale: f64,
) {
    let origin = bounds.origin;
    let (cursor_row, cursor_col) = state.active().grid.cursor_pos();
    let marked_y0 = snap_to_pixel(
        origin.y + bounds.size.height - ((cursor_row + 1) as f64 * state.cell_height),
        scale,
    );
    let marked_y1 = snap_to_pixel(
        origin.y + bounds.size.height - (cursor_row as f64 * state.cell_height),
        scale,
    );
    let marked_rect_h = (marked_y1 - marked_y0).max(0.0);
    let font_height = state.font.ascender() - state.font.descender();
    let marked_text_y = snap_to_pixel(marked_y0 + (marked_rect_h - font_height) / 2.0, scale);
    let marked_fg = NSColor::colorWithSRGBRed_green_blue_alpha(0.7, 0.7, 0.7, 1.0);

    // Draw the composition the same way the grid rows are drawn: each glyph
    // anchored to its column, so composing CJK cannot slide away from the
    // cursor as the preedit grows. Combining marks stay on the cell they mark.
    marked_fg.set();
    TextPainter {
        state,
        glyphs,
        attributes,
        context,
        scale,
        origin_x: origin.x,
        text_y: marked_text_y,
    }
    .draw(state.marked_text(), cursor_col);
    let marked_cols = state
        .marked_text()
        .chars()
        .map(display_cells)
        .sum::<usize>()
        .max(1);
    let marked_width = marked_cols as f64 * state.cell_width;
    let underline_rect = NSRect::new(
        NSPoint::new(
            snap_to_pixel(origin.x + cursor_col as f64 * state.cell_width, scale),
            marked_y0,
        ),
        NSSize::new(marked_width, 1.0),
    );
    marked_fg.set();
    NSBezierPath::fillRect(underline_rect);
}

/// Fill the cells covered by the selection, one horizontal span per row.
fn paint_selection_overlay(
    session: &TerminalSession,
    selection: &Selection,
    bounds: NSRect,
    scale: f64,
    cell_width: f64,
    cell_height: f64,
) {
    let color = to_nscolor(session.grid.selection_bg());
    color.set();
    for row in 0..session.grid.rows() {
        let mut col = 0;
        while col < session.grid.cols() {
            if selection.contains(row, col) {
                let start_col = col;
                while col < session.grid.cols() && selection.contains(row, col) {
                    col += 1;
                }
                let x0 = snap_to_pixel(bounds.origin.x + start_col as f64 * cell_width, scale);
                let x1 = snap_to_pixel(bounds.origin.x + col as f64 * cell_width, scale);
                let y0 = snap_to_pixel(
                    bounds.origin.y + bounds.size.height - ((row + 1) as f64 * cell_height),
                    scale,
                );
                let y1 = snap_to_pixel(
                    bounds.origin.y + bounds.size.height - (row as f64 * cell_height),
                    scale,
                );
                let rect = NSRect::new(
                    NSPoint::new(x0, y0),
                    NSSize::new((x1 - x0).max(0.0), (y1 - y0).max(0.0)),
                );
                NSBezierPath::fillRect(rect);
            } else {
                col += 1;
            }
        }
    }
}

/// Build the foreground-color + font attribute dictionary used by every text draw.
pub(super) fn text_attributes(
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSMutableDictionary<NSAttributedStringKey, AnyObject>> {
    let fg_attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
        NSDictionary::dictionaryWithObject_forKey(
            cast_any_object(color),
            ProtocolObject::from_ref(NSForegroundColorAttributeName),
        )
    };
    let attrs = NSMutableDictionary::dictionaryWithDictionary(&fg_attrs);
    unsafe {
        attrs.setObject_forKey(
            cast_any_object(font),
            ProtocolObject::from_ref(NSFontAttributeName),
        )
    };
    attrs
}

/// Map a terminal color onto an sRGB AppKit color.
pub(super) fn to_nscolor(c: Color32) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c.r as f64 / 255.0,
        c.g as f64 / 255.0,
        c.b as f64 / 255.0,
        c.a as f64 / 255.0,
    )
}

/// Snap a coordinate to the backing-store pixel grid so edges stay crisp.
pub(super) fn snap_to_pixel(value: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mac_app::cast_any_object;
    use crate::mac_app::font::load_terminal_font;
    use objc2::AnyThread;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{
        NSBitmapImageRep, NSDeviceRGBColorSpace, NSFontAttributeName, NSGraphicsContext,
    };

    /// A bitmap context to draw glyphs into, with its graphics context made
    /// current for the duration of the test.
    struct Canvas {
        context: Option<Retained<NSGraphicsContext>>,
        _rep: Retained<NSBitmapImageRep>,
    }

    impl Canvas {
        fn new() -> Self {
            let rep = unsafe {
                NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                    NSBitmapImageRep::alloc(),
                    std::ptr::null_mut(),
                    64,
                    32,
                    8,
                    4,
                    true,
                    false,
                    NSDeviceRGBColorSpace,
                    0,
                    0,
                )
            }
            .expect("bitmap");
            let graphics = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep).expect("ctx");
            let previous = NSGraphicsContext::currentContext();
            NSGraphicsContext::setCurrentContext(Some(&graphics));
            Self {
                context: previous,
                _rep: rep,
            }
        }

        fn cg_context(&self) -> Retained<CGContext> {
            NSGraphicsContext::currentContext()
                .expect("current context")
                .CGContext()
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            NSGraphicsContext::setCurrentContext(self.context.as_deref());
        }
    }

    fn glyph_attributes() -> Retained<NSMutableDictionary<NSAttributedStringKey, AnyObject>> {
        let font = load_terminal_font(14.0);
        let font_only: Retained<TextAttributes> = unsafe {
            NSDictionary::dictionaryWithObject_forKey(
                cast_any_object(&*font),
                ProtocolObject::from_ref(NSFontAttributeName),
            )
        };
        NSMutableDictionary::dictionaryWithDictionary(&font_only)
    }

    fn shaped_with_starts(starts: &[usize], utf16_len: usize) -> ShapedText {
        ShapedText {
            glyphs: starts
                .iter()
                .map(|&start| ShapedGlyph {
                    face: 0,
                    id: 1,
                    start,
                    advance: 8.4,
                })
                .collect(),
            utf16_len,
        }
    }

    #[test]
    fn glyph_spans_follow_the_text_for_left_to_right_glyphs() {
        let shaped = shaped_with_starts(&[0, 1, 2, 5], 7);
        assert_eq!(glyph_spans(&shaped), vec![(0, 1), (1, 2), (2, 5), (5, 7)]);
    }

    /// Core Text hands right-to-left glyphs back in visual order, so the
    /// neighbour in the list is the character *after* in the text.
    #[test]
    fn glyph_spans_follow_the_text_for_right_to_left_glyphs() {
        let shaped = shaped_with_starts(&[2, 1, 0], 3);
        assert_eq!(glyph_spans(&shaped), vec![(2, 3), (1, 2), (0, 1)]);
    }

    /// A batch that scales its glyphs must put the text matrix back, or every
    /// glyph drawn after it in the same frame disappears. (The zero matrix is
    /// what `CGAffineTransform::default()` returns, which is how that bug
    /// looked in practice.)
    #[test]
    fn a_scaled_batch_leaves_an_identity_text_matrix_behind() {
        let canvas = Canvas::new();
        let context = canvas.cg_context();
        let font = load_terminal_font(14.0);
        let attributes = glyph_attributes();
        let mut glyphs = GlyphCache::new(&font);
        let shaped = glyphs.shape("\u{1f600}", &attributes);
        let glyph = shaped.glyphs[0];

        // A box narrower than the emoji forces the clamped scale.
        let mut batch = GlyphBatch::default();
        batch.push(&glyphs, Some(&context), &glyph, 4.0, 8.0, 20.0);
        batch.flush(&glyphs, Some(&context));

        let matrix = CGContext::text_matrix(Some(&context));
        assert_eq!(
            (matrix.a, matrix.b, matrix.c, matrix.d),
            (1.0, 0.0, 0.0, 1.0),
            "the text matrix must be the identity again"
        );

        // A following, cell-wide glyph is drawn unscaled, which is only true
        // while the matrix is the identity.
        let shaped = glyphs.shape("W", &attributes);
        let mut batch = GlyphBatch::default();
        batch.push(&glyphs, Some(&context), &shaped.glyphs[0], 40.0, 8.4, 20.0);
        assert_eq!(batch.scale, 1.0, "a cell-wide glyph is never scaled");
        batch.flush(&glyphs, Some(&context));
        let matrix = CGContext::text_matrix(Some(&context));
        assert_eq!((matrix.a, matrix.d), (1.0, 1.0));
    }

    /// A glyph that advances exactly its box is left alone by every policy:
    /// that is the whole of ASCII in a monospace font, so the policies only
    /// ever touch text the system falls back on.
    #[test]
    fn glyphs_that_fit_their_columns_are_never_touched() {
        for fit in [GlyphFit::Left, GlyphFit::Center, GlyphFit::Stretch] {
            for overflow in [GlyphOverflow::Clamp, GlyphOverflow::Allow] {
                assert_eq!(fit_glyph(fit, overflow, 8.4, 8.4), (0.0, 1.0), "{fit:?}");
            }
        }
    }

    /// CJK: one em (14pt) inside the two columns (16.8pt) the grid gives it.
    #[test]
    fn wide_characters_are_placed_inside_their_two_columns() {
        let (box_width, advance) = (16.8, 14.0);
        assert_eq!(
            fit_glyph(GlyphFit::Left, GlyphOverflow::Clamp, advance, box_width),
            (0.0, 1.0),
            "left-aligned keeps the full 0.2em gap before the cursor"
        );
        let (offset, scale) = fit_glyph(GlyphFit::Center, GlyphOverflow::Clamp, advance, box_width);
        assert!(
            (offset - 1.4).abs() < 1e-12 && scale == 1.0,
            "centred splits the 0.2em gap evenly: {offset}"
        );
        assert_eq!(
            fit_glyph(GlyphFit::Stretch, GlyphOverflow::Clamp, advance, box_width),
            (0.0, 16.8 / 14.0),
            "stretched fills the two columns exactly"
        );
    }

    /// Emoji: 19pt inside two columns, so it must be squeezed to a fifth of a
    /// cell less, or it covers whatever the grid put in the next column.
    #[test]
    fn wider_than_their_columns_glyphs_are_clamped_or_left_alone() {
        let (box_width, advance) = (16.8, 19.0);
        let (offset, scale) = fit_glyph(GlyphFit::Center, GlyphOverflow::Clamp, advance, box_width);
        assert!((scale - 16.8 / 19.0).abs() < 1e-12);
        assert!(offset.abs() < 1e-12, "clamping leaves nothing to centre");
        assert!(
            advance * scale <= box_width + 1e-12,
            "the clamped glyph fits its columns"
        );

        let (offset, scale) = fit_glyph(GlyphFit::Center, GlyphOverflow::Allow, advance, box_width);
        assert_eq!(
            (offset, scale),
            (0.0, 1.0),
            "allowing overflow leaves the glyph at its natural size, growing right"
        );
    }

    /// A fallback face that advances a hair over one cell must not be
    /// squeezed: 0.33% is a third of a pixel, and scaling it would only soften
    /// the glyph for no visible gain.
    #[test]
    fn sub_pixel_overflow_is_not_worth_squeezing() {
        let menlo = 8.429;
        let cell = 8.4014;
        assert_eq!(
            fit_glyph(GlyphFit::Left, GlyphOverflow::Clamp, menlo, cell),
            (0.0, 1.0)
        );
        // A visible overflow, like an emoji, is clamped.
        let (offset, scale) = fit_glyph(GlyphFit::Left, GlyphOverflow::Clamp, 19.0, 16.8027);
        assert!(offset.abs() < 1e-12);
        assert!((scale - 16.8027 / 19.0).abs() < 1e-12);
    }

    /// A combining mark in a cell of its own has no columns to fill.
    #[test]
    fn zero_width_glyphs_keep_their_own_scale() {
        assert_eq!(
            fit_glyph(GlyphFit::Stretch, GlyphOverflow::Clamp, 5.0, 0.0),
            (0.0, 1.0)
        );
        assert_eq!(
            fit_glyph(GlyphFit::Stretch, GlyphOverflow::Clamp, 0.0, 8.4),
            (0.0, 1.0)
        );
    }
}
