#![cfg(target_os = "macos")]

use crate::keymap;
use crate::terminal::grid::TerminalGrid;
use eframe::egui;
use nix::libc::{ioctl, killpg, pid_t, tcgetpgrp, winsize, SIGWINCH, TIOCSWINSZ};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSAutoresizingMaskOptions,
    NSBackingStoreType, NSBezierPath, NSColor, NSEvent, NSFont, NSResponder, NSStringDrawing,
    NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSAttributedStringKey, NSDictionary, NSMutableDictionary, NSPoint, NSRect, NSSize,
    NSString,
};

use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{Receiver, Sender};

/// Holds the Rust-side state for our terminal view.
struct TerminalViewState {
    grid: TerminalGrid,
    rx_output: Receiver<Vec<u8>>,
    tx_input: Sender<Vec<u8>>,
    font: Retained<NSFont>,
    cell_width: f64,
    cell_height: f64,
    timer: Option<Retained<objc2_foundation::NSTimer>>,
    master_fd: Option<OwnedFd>,
    slave_fd: Option<OwnedFd>,
    shell_pgid: pid_t,
}

impl Drop for TerminalViewState {
    fn drop(&mut self) {
        if let Some(timer) = self.timer.take() {
            timer.invalidate();
        }
    }
}

// Main entry point for the native macOS application.
pub fn run_native(
    rx_output: Receiver<Vec<u8>>,
    tx_input: Sender<Vec<u8>>,
    master_fd: OwnedFd,
    slave_fd: OwnedFd,
    shell_pgid: pid_t,
) -> Result<(), Box<dyn std::error::Error>> {
    let mtm = MainThreadMarker::new().ok_or("must be on main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let delegate =
        ShittyAppDelegate::new(mtm, rx_output, tx_input, master_fd, slave_fd, shell_pgid);
    app.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*delegate)));

    app.run();
    Ok(())
}

define_class!(
    #[unsafe(super(NSResponder))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct ShittyAppDelegate;

    unsafe impl NSObjectProtocol for ShittyAppDelegate {}

    unsafe impl NSApplicationDelegate for ShittyAppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &AnyObject) {
            let mtm = self.mtm();

            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Miniaturizable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };

            window.center();
            window.setTitle(ns_string!("shitty (AppKit)"));
            unsafe { window.setReleasedWhenClosed(false) };

            let view = self
                .ivars()
                .terminal_view
                .borrow()
                .as_ref()
                .cloned()
                .unwrap_or_else(|| ShittyTerminalView::new(mtm, Self::default_terminal_state()));

            window.setContentView(Some(&view));
            view.setAutoresizingMask(
                NSAutoresizingMaskOptions::ViewWidthSizable
                    | NSAutoresizingMaskOptions::ViewHeightSizable,
            );
            window.makeKeyAndOrderFront(None);
            window.setAcceptsMouseMovedEvents(true);
            window.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(self)));

            self.ivars().window.replace(Some(window));
            self.ivars().terminal_view.replace(Some(view));
        }
    }

    unsafe impl NSWindowDelegate for ShittyAppDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &AnyObject) {
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl ShittyAppDelegate {
    fn new(
        mtm: MainThreadMarker,
        rx_output: Receiver<Vec<u8>>,
        tx_input: Sender<Vec<u8>>,
        master_fd: OwnedFd,
        slave_fd: OwnedFd,
        shell_pgid: pid_t,
    ) -> Retained<Self> {
        let view_state = Self::terminal_state_from_channels(
            rx_output, tx_input, master_fd, slave_fd, shell_pgid,
        );
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::new(mtm, view_state));
        unsafe { msg_send![super(this), init] }
    }

    fn terminal_state_from_channels(
        rx_output: Receiver<Vec<u8>>,
        tx_input: Sender<Vec<u8>>,
        master_fd: OwnedFd,
        slave_fd: OwnedFd,
        shell_pgid: pid_t,
    ) -> TerminalViewState {
        let font = load_terminal_font(14.0);
        let ascender = font.ascender();
        let descender = font.descender();
        let leading = font.leading();
        let cell_height = (ascender - descender + leading).ceil();

        let w_char = ns_string!("W");
        let attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
            NSDictionary::dictionaryWithObject_forKey(
                cast_any_object(&*font),
                ProtocolObject::from_ref(objc2_app_kit::NSFontAttributeName),
            )
        };
        let w_size = unsafe { w_char.sizeWithAttributes(Some(&*attrs)) };
        let cell_width = w_size.width;

        TerminalViewState {
            grid: TerminalGrid::new(80, 24),
            rx_output,
            tx_input,
            font,
            cell_width,
            cell_height,
            timer: None,
            master_fd: Some(master_fd),
            slave_fd: Some(slave_fd),
            shell_pgid,
        }
    }

    fn default_terminal_state() -> TerminalViewState {
        let (tx_output, rx_output) = std::sync::mpsc::channel();
        let (tx_input, _rx_input) = std::sync::mpsc::channel();
        drop(tx_output);
        let font = load_terminal_font(14.0);
        let ascender = font.ascender();
        let descender = font.descender();
        let leading = font.leading();
        let cell_height = (ascender - descender + leading).ceil();

        let w_char = ns_string!("W");
        let attrs: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = unsafe {
            NSDictionary::dictionaryWithObject_forKey(
                cast_any_object(&*font),
                ProtocolObject::from_ref(objc2_app_kit::NSFontAttributeName),
            )
        };
        let w_size = unsafe { w_char.sizeWithAttributes(Some(&*attrs)) };
        let cell_width = w_size.width;

        TerminalViewState {
            grid: TerminalGrid::new(80, 24),
            rx_output,
            tx_input,
            font,
            cell_width,
            cell_height,
            timer: None,
            master_fd: None,
            slave_fd: None,
            shell_pgid: 0,
        }
    }
}

fn load_terminal_font(size: f64) -> Retained<NSFont> {
    let font_name = ns_string!("MesloLGS NF");
    NSFont::fontWithName_size(font_name, size)
        .unwrap_or_else(|| NSFont::userFixedPitchFontOfSize(size).unwrap())
}

struct AppDelegateIvars {
    window: std::cell::RefCell<Option<Retained<NSWindow>>>,
    terminal_view: std::cell::RefCell<Option<Retained<ShittyTerminalView>>>,
}

impl AppDelegateIvars {
    fn new(mtm: MainThreadMarker, view_state: TerminalViewState) -> Self {
        Self {
            window: std::cell::RefCell::new(None),
            terminal_view: std::cell::RefCell::new(Some(ShittyTerminalView::new(mtm, view_state))),
        }
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = TerminalViewIvars]
    struct ShittyTerminalView;

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
            let mut input_bytes = Vec::new();
            keymap::append_input_from_nsevent(event, &mut input_bytes);
            if !input_bytes.is_empty() {
                let state_ptr = self.ivars().state;
                if let Some(state) = unsafe { state_ptr.as_ref() } {
                    let _ = state.tx_input.send(input_bytes);
                }
            }
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
            let (cell_w, cell_h) = (state.cell_width, state.cell_height);
            let scale = self
                .window()
                .map(|window| window.backingScaleFactor())
                .unwrap_or(1.0);

            let default_bg = state.grid.default_bg();
            to_nscolor(default_bg).set();
            NSBezierPath::fillRect(bounds);

            let rows = state.grid.rows();
            let cols = state.grid.cols();

            for row in 0..rows {
                let mut col = 0;
                while col < cols {
                    let cell = state.grid.get_cell(row, col);
                    let (cell_text, fg, bg, underline, run_width, skip_cell) =
                        if let Some(cell) = &cell {
                            (
                                cell.text.as_str(),
                                cell.fg,
                                cell.bg,
                                cell.underline,
                                if cell.wide { 2 } else { 1 },
                                cell.wide_continuation,
                            )
                        } else {
                            ("", egui::Color32::WHITE, default_bg, false, 1, false)
                        };

                    if skip_cell {
                        col = col.saturating_add(1);
                        continue;
                    }

                    let x0 = snap_to_pixel(origin.x + col as f64 * cell_w, scale);
                    let x1 = snap_to_pixel(
                        origin.x + (col + run_width) as f64 * cell_w,
                        scale,
                    );
                    let y0 = snap_to_pixel(
                        origin.y + bounds.size.height - ((row + 1) as f64 * cell_h),
                        scale,
                    );
                    let y1 = snap_to_pixel(
                        origin.y + bounds.size.height - (row as f64 * cell_h),
                        scale,
                    );
                    let rect_w = (x1 - x0).max(0.0);
                    let rect_h = (y1 - y0).max(0.0);

                    let font_height = state.font.ascender() as f64 - state.font.descender() as f64;
                    let text_y_pos = y0 + (rect_h - font_height) / 2.0;

                    let rect = NSRect::new(NSPoint::new(x0, y0), NSSize::new(rect_w, rect_h));

                    if bg != default_bg {
                        to_nscolor(bg).set();
                        NSBezierPath::fillRect(rect);
                    }

                    if !cell_text.is_empty() && cell_text != " " {
                        let text = NSString::from_str(cell_text);
                        let text_pos = NSPoint::new(x0, snap_to_pixel(text_y_pos, scale));
                        let fg_color = to_nscolor(fg);

                        let text_attributes: Retained<
                            NSDictionary<NSAttributedStringKey, AnyObject>,
                        > = unsafe {
                            NSDictionary::dictionaryWithObject_forKey(
                                cast_any_object(&*fg_color),
                                ProtocolObject::from_ref(
                                    objc2_app_kit::NSForegroundColorAttributeName,
                                ),
                            )
                        };
                        let text_attributes =
                            NSMutableDictionary::dictionaryWithDictionary(&text_attributes);
                        unsafe {
                            text_attributes.setObject_forKey(
                                cast_any_object(&*state.font),
                                ProtocolObject::from_ref(objc2_app_kit::NSFontAttributeName),
                            )
                        };
                        unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attributes)) };
                    }

                    if underline {
                        let underline_y = y0 + rect_h - 1.0;
                        let underline_rect = NSRect::new(
                            NSPoint::new(x0, underline_y),
                            NSSize::new(rect_w, 1.0),
                        );
                        to_nscolor(fg).set();
                        NSBezierPath::fillRect(underline_rect);
                    }

                    col = col.saturating_add(run_width.max(1));
                }
            }

            if state.grid.cursor_visible() {
                let (cursor_row, cursor_col) = state.grid.cursor_pos();
                let cursor_cell = state.grid.get_cell(cursor_row, cursor_col);
                let (cell_text, cell_fg, cell_bg) = cursor_cell
                    .as_ref()
                    .map(|cell| {
                        let (fg, bg) = state.grid.resolve_cell_colors(cell);
                        (cell.text.as_str(), fg, bg)
                    })
                    .unwrap_or((" ", egui::Color32::WHITE, default_bg));
                let cursor_bg = state.grid.cursor_color().unwrap_or_else(|| {
                    if cell_fg == cell_bg {
                        egui::Color32::WHITE
                    } else {
                        cell_fg
                    }
                });
                let cursor_fg = if cursor_bg == cell_bg {
                    cell_fg
                } else {
                    cell_bg
                };
                let cursor_x0 = snap_to_pixel(origin.x + cursor_col as f64 * cell_w, scale);
                let cursor_x1 =
                    snap_to_pixel(origin.x + (cursor_col + 1) as f64 * cell_w, scale);
                let cursor_y0 = snap_to_pixel(
                    origin.y + bounds.size.height - ((cursor_row + 1) as f64 * cell_h),
                    scale,
                );
                let cursor_y1 = snap_to_pixel(
                    origin.y + bounds.size.height - (cursor_row as f64 * cell_h),
                    scale,
                );
                let cursor_rect = NSRect::new(
                    NSPoint::new(cursor_x0, cursor_y0),
                    NSSize::new(
                        (cursor_x1 - cursor_x0).max(0.0),
                        (cursor_y1 - cursor_y0).max(0.0),
                    ),
                );
                let cursor_bg = to_nscolor(cursor_bg);
                let cursor_fg = to_nscolor(cursor_fg);
                cursor_bg.set();
                NSBezierPath::fillRect(cursor_rect);

                let cursor_rect_h = (cursor_y1 - cursor_y0).max(0.0);
                let font_height = state.font.ascender() as f64 - state.font.descender() as f64;
                let text_pos = NSPoint::new(
                    cursor_x0,
                    snap_to_pixel(cursor_y0 + (cursor_rect_h - font_height) / 2.0, scale),
                );
                let text = NSString::from_str(cell_text);
                let text_attributes: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> =
                    unsafe {
                        NSDictionary::dictionaryWithObject_forKey(
                            cast_any_object(&*cursor_fg),
                            ProtocolObject::from_ref(
                                objc2_app_kit::NSForegroundColorAttributeName,
                            ),
                        )
                    };
                let text_attributes =
                    NSMutableDictionary::dictionaryWithDictionary(&text_attributes);
                unsafe {
                    text_attributes.setObject_forKey(
                        cast_any_object(&*state.font),
                        ProtocolObject::from_ref(objc2_app_kit::NSFontAttributeName),
                    )
                };
                unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attributes)) };
            }
        }
    }
);

impl ShittyTerminalView {
    fn new(mtm: MainThreadMarker, state: TerminalViewState) -> Retained<Self> {
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
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0 / 60.0,
                &*timer_target,
                sel!(onTimerTick:),
                None,
                true,
            )
        };
        unsafe {
            if let Some(state) = (state_ptr as *mut TerminalViewState).as_mut() {
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
        let cols = (size.width / state.cell_width).floor() as usize;
        let rows = (size.height / state.cell_height).floor() as usize;
        let cols = cols.max(1);
        let rows = rows.max(1);
        if !state.grid.resize(cols, rows) {
            return;
        }
        if let Some(master_fd) = state.master_fd.as_ref() {
            set_winsize_raw(master_fd.as_raw_fd(), cols as u16, rows as u16);
        }
        if let Some(slave_fd) = state.slave_fd.as_ref() {
            let pgid = unsafe { tcgetpgrp(slave_fd.as_raw_fd()) };
            let target_pgid = if pgid > 0 { pgid } else { state.shell_pgid };
            if target_pgid > 0 {
                unsafe {
                    let _ = killpg(target_pgid, SIGWINCH);
                }
            }
        }
        self.setNeedsDisplay(true);
    }
}

struct TerminalViewIvars {
    state: *mut TerminalViewState,
}

define_class!(
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = objc2::MainThreadOnly]
    #[ivars = TimerTargetIvars]
    struct TimerTarget;

    impl TimerTarget {
        #[unsafe(method(onTimerTick:))]
        fn on_timer_tick(&self, _timer: &objc2_foundation::NSTimer) {
            let state_ptr = self.ivars().view.ivars().state;
            if state_ptr.is_null() {
                return;
            }
            let state = unsafe { &mut *state_ptr };

            let mut received_data = false;
            while let Ok(bytes) = state.rx_output.try_recv() {
                state.grid.write_bytes(&bytes);
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

fn cast_any_object<T: ?Sized>(obj: &T) -> &AnyObject {
    unsafe { &*(obj as *const T as *const AnyObject) }
}

fn set_winsize_raw(fd: i32, cols: u16, rows: u16) {
    let ws = winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        let _ = ioctl(fd, TIOCSWINSZ, &ws);
    }
}

// Helper to convert egui colors to NSColor.
fn to_nscolor(c: egui::Color32) -> Retained<NSColor> {
    let r = c.r();
    let g = c.g();
    let b = c.b();
    let a = c.a();
    NSColor::colorWithSRGBRed_green_blue_alpha(
        r as f64 / 255.0,
        g as f64 / 255.0,
        b as f64 / 255.0,
        a as f64 / 255.0,
    )
}

fn snap_to_pixel(value: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}
