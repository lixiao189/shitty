use crate::terminal::color::Color32;
use crate::terminal::grid::TerminalGrid;
use crate::terminal::keymap;
use crate::terminal::pty::PtyEvent;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName, NSForegroundColorAttributeName,
    NSStringDrawing, NSTextInputClient, NSView,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSAttributedStringKey, NSDictionary, NSMutableDictionary,
    NSNotFound, NSPoint, NSRange, NSRect, NSSize, NSString, NSTimer, NSUInteger,
};
use std::sync::mpsc::{Receiver, Sender};

use super::cast_any_object;

/// Refresh rate for the terminal rendering timer (in Hz).
const RENDER_REFRESH_RATE_HZ: f64 = 120.0;

/// Rust-side state shared by the terminal view and its render timer.
pub(super) struct TerminalViewState {
    grid: TerminalGrid,
    rx_output: Receiver<Vec<u8>>,
    tx_input: Sender<PtyEvent>,
    font: Retained<NSFont>,
    cell_width: f64,
    cell_height: f64,
    timer: Option<Retained<NSTimer>>,
    marked_text: String,
    ime_composing: bool,
    had_composition: bool,
    skip_next_command: bool,
}

impl TerminalViewState {
    pub(super) fn new(
        rx_output: Receiver<Vec<u8>>,
        tx_input: Sender<PtyEvent>,
        font: Retained<NSFont>,
        cell_width: f64,
        cell_height: f64,
    ) -> Self {
        Self {
            grid: TerminalGrid::new(80, 24),
            rx_output,
            tx_input,
            font,
            cell_width,
            cell_height,
            timer: None,
            marked_text: String::new(),
            ime_composing: false,
            had_composition: false,
            skip_next_command: false,
        }
    }

    fn send_input(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.tx_input.send(PtyEvent::Input(bytes));
        }
    }

    fn send_resize(&self, cols: usize, rows: usize) {
        let _ = self.tx_input.send(PtyEvent::Resize {
            cols: cols as u16,
            rows: rows as u16,
        });
    }

    fn cursor_cell_rect(&self, bounds: NSRect) -> NSRect {
        let origin = bounds.origin;
        let (cell_w, cell_h) = (self.cell_width, self.cell_height);
        let (cursor_row, cursor_col) = self.grid.cursor_pos();
        let x0 = origin.x + cursor_col as f64 * cell_w;
        let y0 = origin.y + bounds.size.height - ((cursor_row + 1) as f64 * cell_h);
        NSRect::new(NSPoint::new(x0, y0), NSSize::new(cell_w, cell_h))
    }

    fn marked_text_range(&self) -> NSRange {
        if self.marked_text.is_empty() {
            return NSRange::new(NSNotFound as NSUInteger, 0);
        }
        let ns = NSString::from_str(&self.marked_text);
        NSRange::new(0, ns.length())
    }
}

impl Drop for TerminalViewState {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = TerminalViewIvars]
    pub(super) struct ShittyTerminalView;

    impl ShittyTerminalView {
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, new_size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: new_size] };
            self.update_grid_for_size(new_size);
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let state_ptr = self.ivars().state;
            if let Some(state) = unsafe { state_ptr.as_mut() } {
                state.had_composition = state.ime_composing;
                state.marked_text.clear();
                state.skip_next_command = false;
            }
            let events = NSArray::from_slice(&[event]);
            self.interpretKeyEvents(&events);
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let state_ptr = self.ivars().state;
            if state_ptr.is_null() {
                return;
            }
            let state = unsafe { &*state_ptr };

            let bounds = self.bounds();
            let origin = bounds.origin;
            let scale = self
                .window()
                .map(|window| window.backingScaleFactor())
                .unwrap_or(1.0);
            let default_bg = state.grid.default_bg();

            to_nscolor(default_bg).set();
            NSBezierPath::fillRect(bounds);

            for row in 0..state.grid.rows() {
                for run in state.grid.row_runs(row) {
                    let x0 = snap_to_pixel(origin.x + run.col as f64 * state.cell_width, scale);
                    let x1 = snap_to_pixel(
                        origin.x + (run.col + run.width) as f64 * state.cell_width,
                        scale,
                    );
                    let y0 = snap_to_pixel(
                        origin.y + bounds.size.height - ((row + 1) as f64 * state.cell_height),
                        scale,
                    );
                    let y1 = snap_to_pixel(
                        origin.y + bounds.size.height - (row as f64 * state.cell_height),
                        scale,
                    );
                    let rect_w = (x1 - x0).max(0.0);
                    let rect_h = (y1 - y0).max(0.0);
                    let rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, rect_h));

                    if run.bg != default_bg {
                        to_nscolor(run.bg).set();
                        NSBezierPath::fillRect(rect);
                    }

                    if !run.text.is_empty() && !run.text.trim().is_empty() {
                        let font_height = state.font.ascender() - state.font.descender();
                        let text_pos = NSPoint::new(
                            x0,
                            snap_to_pixel(y0 + (rect_h - font_height) / 2.0, scale),
                        );
                        let text = NSString::from_str(&run.text);
                        let text_attrs = text_attributes(&state.font, &to_nscolor(run.fg));
                        unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attrs)) };
                    }

                    if run.underline {
                        // y0 is the cell bottom in NSView coords (y-up), so the underline sits there.
                        let underline_rect =
                            NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, 1.0));
                        to_nscolor(run.fg).set();
                        NSBezierPath::fillRect(underline_rect);
                    }
                }
            }

            if let Some(cursor) = state.grid.cursor() {
                let cursor_x0 = snap_to_pixel(origin.x + cursor.col as f64 * state.cell_width, scale);
                let cursor_x1 = snap_to_pixel(
                    origin.x + (cursor.col + 1) as f64 * state.cell_width,
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
                let cursor_rect = NSRect::new(
                    NSPoint::new(cursor_x0, cursor_y0),
                    NSSize::new(
                        (cursor_x1 - cursor_x0).max(0.0),
                        (cursor_y1 - cursor_y0).max(0.0),
                    ),
                );
                let cursor_bg_color = to_nscolor(cursor.bg);
                let cursor_fg_color = to_nscolor(cursor.fg);
                cursor_bg_color.set();
                NSBezierPath::fillRect(cursor_rect);

                let cursor_rect_h = (cursor_y1 - cursor_y0).max(0.0);
                let font_height = state.font.ascender() - state.font.descender();
                let text_pos = NSPoint::new(
                    cursor_x0,
                    snap_to_pixel(cursor_y0 + (cursor_rect_h - font_height) / 2.0, scale),
                );
                let text = NSString::from_str(&cursor.text);
                let text_attrs = text_attributes(&state.font, &cursor_fg_color);
                unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attrs)) };
            }

            if !state.marked_text.is_empty() {
                let (cursor_row, cursor_col) = state.grid.cursor_pos();
                let marked_x0 = snap_to_pixel(origin.x + cursor_col as f64 * state.cell_width, scale);
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
                let marked_text_pos = NSPoint::new(
                    marked_x0,
                    snap_to_pixel(marked_y0 + (marked_rect_h - font_height) / 2.0, scale),
                );
                let marked_fg = NSColor::colorWithSRGBRed_green_blue_alpha(0.7, 0.7, 0.7, 1.0);
                let marked_str = NSString::from_str(&state.marked_text);
                let marked_attrs = text_attributes(&state.font, &marked_fg);
                unsafe { marked_str.drawAtPoint_withAttributes(marked_text_pos, Some(&marked_attrs)) };

                let underline_rect = NSRect::new(
                    NSPoint::new(marked_x0, marked_y0),
                    NSSize::new(
                        (state.marked_text.len() as f64 * state.cell_width).max(state.cell_width),
                        1.0,
                    ),
                );
                marked_fg.set();
                NSBezierPath::fillRect(underline_rect);
            }
        }
    }

    unsafe impl NSTextInputClient for ShittyTerminalView {
        #[unsafe(method(insertText:replacementRange:))]
        fn insert_text_replacement_range(&self, string: &AnyObject, _replacement_range: NSRange) {
            let Some(text) = ns_object_to_string(string) else {
                return;
            };
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            let had_composition = state.had_composition;
            state.marked_text.clear();
            state.ime_composing = false;
            state.had_composition = false;

            let mut input_bytes = Vec::new();
            keymap::append_input_from_text(&text, &mut input_bytes);
            state.send_input(input_bytes);

            if had_composition {
                state.skip_next_command = true;
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command_by_selector(&self, selector: Sel) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            if state.skip_next_command {
                state.skip_next_command = false;
                return;
            }

            let mut input_bytes = Vec::new();
            if keymap::append_input_from_selector(selector, &mut input_bytes) {
                state.send_input(input_bytes);
            }
        }

        #[unsafe(method(setMarkedText:selectedRange:replacementRange:))]
        fn set_marked_text_selected_range_replacement_range(
            &self,
            string: &AnyObject,
            _selected_range: NSRange,
            _replacement_range: NSRange,
        ) {
            let text = ns_object_to_string(string).unwrap_or_default();
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            state.marked_text = text;
            state.ime_composing = !state.marked_text.is_empty();
            state.had_composition = false;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_mut() }) else {
                return;
            };

            state.marked_text.clear();
            state.ime_composing = false;
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(selectedRange))]
        fn selected_range(&self) -> NSRange {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return NSRange::new(NSNotFound as NSUInteger, 0);
            };
            state.marked_text_range()
        }

        #[unsafe(method(markedRange))]
        fn marked_range(&self) -> NSRange {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return NSRange::new(NSNotFound as NSUInteger, 0);
            };
            state.marked_text_range()
        }

        #[unsafe(method(hasMarkedText))]
        fn has_marked_text(&self) -> Bool {
            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return Bool::NO;
            };
            (!state.marked_text.is_empty()).into()
        }

        #[unsafe(method_id(attributedSubstringForProposedRange:actualRange:))]
        fn attributed_substring_for_proposed_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> Option<Retained<NSAttributedString>> {
            None
        }

        #[unsafe(method_id(validAttributesForMarkedText))]
        fn valid_attributes_for_marked_text(&self) -> Retained<NSArray<NSAttributedStringKey>> {
            NSArray::new()
        }

        #[unsafe(method(firstRectForCharacterRange:actualRange:))]
        fn first_rect_for_character_range_actual_range(
            &self,
            _range: NSRange,
            _actual_range: *mut NSRange,
        ) -> NSRect {
            let Some(window) = self.window() else {
                return NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            };

            let state_ptr = self.ivars().state;
            let Some(state) = (unsafe { state_ptr.as_ref() }) else {
                return NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            };

            let view_rect = state.cursor_cell_rect(self.bounds());
            window.convertRectToScreen(view_rect)
        }

        #[unsafe(method(characterIndexForPoint:))]
        fn character_index_for_point(&self, _point: NSPoint) -> NSUInteger {
            0
        }
    }
);

impl ShittyTerminalView {
    pub(super) fn new(mtm: MainThreadMarker, state: TerminalViewState) -> Retained<Self> {
        let state_ptr = Box::into_raw(Box::new(state));
        let this = Self::alloc(mtm).set_ivars(TerminalViewIvars { state: state_ptr });
        let view: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0))
            ]
        };
        let timer_target = TimerTarget::new(mtm, &view);
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0 / RENDER_REFRESH_RATE_HZ,
                &timer_target,
                sel!(onTimerTick:),
                None,
                true,
            )
        };
        unsafe {
            if let Some(state) = state_ptr.as_mut() {
                state.timer = Some(timer);
            }
        }
        view.update_grid_for_size(view.bounds().size);
        view
    }

    fn update_grid_for_size(&self, size: NSSize) {
        let state_ptr = self.ivars().state;
        if state_ptr.is_null() {
            return;
        }
        let state = unsafe { &mut *state_ptr };
        let cols = ((size.width / state.cell_width).floor() as usize).max(1);
        let rows = ((size.height / state.cell_height).floor() as usize).max(1);
        if !state.grid.resize(cols, rows) {
            return;
        }
        state.send_resize(cols, rows);
        self.setNeedsDisplay(true);
    }
}

pub(super) struct TerminalViewIvars {
    state: *mut TerminalViewState,
}

// Drop impl ensures the boxed TerminalViewState is freed when the ObjC view
// is deallocated by ARC, which also triggers TerminalViewState::drop() which
// invalidates the NSTimer and breaks the retain cycle.
impl Drop for TerminalViewIvars {
    fn drop(&mut self) {
        if !self.state.is_null() {
            unsafe { drop(Box::from_raw(self.state)) };
            self.state = std::ptr::null_mut();
        }
    }
}

define_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = TimerTargetIvars]
    struct TimerTarget;

    impl TimerTarget {
        #[unsafe(method(onTimerTick:))]
        fn on_timer_tick(&self, _timer: &NSTimer) {
            let state_ptr = self.ivars().view.ivars().state;
            if state_ptr.is_null() {
                return;
            }
            let state = unsafe { &mut *state_ptr };

            let mut received_data = false;
            while let Ok(bytes) = state.rx_output.try_recv() {
                state.grid.process_pty_bytes(&bytes);
                received_data = true;
            }

            if received_data && state.grid.has_changes() {
                self.ivars().view.setNeedsDisplay(true);
                state.grid.mark_rendered();
            }
        }
    }
);

struct TimerTargetIvars {
    view: Retained<ShittyTerminalView>,
}

impl TimerTarget {
    fn new(mtm: MainThreadMarker, view: &Retained<ShittyTerminalView>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TimerTargetIvars { view: view.clone() });
        unsafe { msg_send![super(this), init] }
    }
}

/// Build the foreground-color + font attribute dictionary used by every text draw.
fn text_attributes(
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

fn ns_object_to_string(obj: &AnyObject) -> Option<String> {
    autoreleasepool(|pool| {
        if let Some(string) = obj.downcast_ref::<NSString>() {
            return Some(unsafe { string.to_str(pool).to_string() });
        }
        if let Some(string) = obj.downcast_ref::<NSAttributedString>() {
            let ns = string.string();
            return Some(unsafe { ns.to_str(pool).to_string() });
        }
        None
    })
}

fn to_nscolor(c: Color32) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        c.r as f64 / 255.0,
        c.g as f64 / 255.0,
        c.b as f64 / 255.0,
        c.a as f64 / 255.0,
    )
}

fn snap_to_pixel(value: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}
