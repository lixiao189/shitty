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

use std::mem;
use std::os::fd::AsRawFd;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{Receiver, Sender};
use termwiz::cell::{CellAttributes, Intensity};
use termwiz::color::{ColorAttribute, SrgbaTuple};

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
            unsafe { NSApplication::sharedApplication(self.mtm()).terminate(None) };
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

            let default_bg = to_nscolor(state.grid.default_bg_color());
            let default_bg_srgba = state.grid.default_bg_color();
            default_bg.set();
            NSBezierPath::fillRect(bounds);

            let lines = state.grid.screen_lines();

            for (row, line) in lines.iter().enumerate() {
                let mut col = 0;
                while col < state.grid.cols() {
                    let cell_ref = line.get_cell(col).map(|cell| cell.as_cell());
                    let (cell_text, attrs, run_width) = if let Some(cell) = &cell_ref {
                        (cell.str(), cell.attrs(), cell.width().max(1) as usize)
                    } else {
                        ("", &CellAttributes::default(), 1)
                    };

                    let (fg_srgba, bg_srgba) = resolve_cell_colors(
                        attrs,
                        state.grid.default_fg_color(),
                        state.grid.default_bg_color(),
                    );
                    let fg = to_nscolor(fg_srgba);
                    let bg = to_nscolor(bg_srgba);

                    let run_text = cell_text;

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

                    if bg_srgba != default_bg_srgba {
                        bg.set();
                        NSBezierPath::fillRect(rect);
                    }

                    if !run_text.is_empty() && run_text != " " {
                        let text = NSString::from_str(run_text);
                        let text_pos = NSPoint::new(x0, snap_to_pixel(text_y_pos, scale));

                        let text_attributes: Retained<
                            NSDictionary<NSAttributedStringKey, AnyObject>,
                        > = unsafe {
                            NSDictionary::dictionaryWithObject_forKey(
                                unsafe { cast_any_object(&*fg) },
                                ProtocolObject::from_ref(
                                    objc2_app_kit::NSForegroundColorAttributeName,
                                ),
                            )
                        };
                        let text_attributes = unsafe {
                            NSMutableDictionary::dictionaryWithDictionary(&text_attributes)
                        };
                        unsafe {
                            text_attributes.setObject_forKey(
                                unsafe { cast_any_object(&*state.font) },
                                ProtocolObject::from_ref(objc2_app_kit::NSFontAttributeName),
                            )
                        };
                        unsafe {
                            text.drawAtPoint_withAttributes(text_pos, Some(&text_attributes))
                        };
                    }

                    col = col.saturating_add(run_width.max(1));
                }
            }

            if state.grid.cursor_visible() {
                let (cursor_row, cursor_col) = state.grid.cursor_pos();
                if let Some(line) = lines.get(cursor_row) {
                    let cell = line
                        .get_cell(cursor_col)
                        .map(|cell| cell.as_cell());
                    let (cell_text, attrs) = if let Some(cell) = &cell {
                        (cell.str(), cell.attrs())
                    } else {
                        (" ", &CellAttributes::default())
                    };
                    let (fg_srgba, bg_srgba) = resolve_cell_colors(
                        attrs,
                        state.grid.default_fg_color(),
                        state.grid.default_bg_color(),
                    );
                    let cursor_bg_srgba = state
                        .grid
                        .cursor_color()
                        .map(color32_to_srgba)
                        .unwrap_or_else(|| {
                            if fg_srgba == bg_srgba {
                                SrgbaTuple(1.0, 1.0, 1.0, 1.0)
                            } else {
                                fg_srgba
                            }
                        });
                    let cursor_fg_srgba = if cursor_bg_srgba == bg_srgba {
                        fg_srgba
                    } else {
                        bg_srgba
                    };
                    let cursor_x0 = snap_to_pixel(origin.x + cursor_col as f64 * cell_w, scale);
                    let cursor_x1 =
                        snap_to_pixel(origin.x + (cursor_col + 1) as f64 * cell_w, scale);
                    let cursor_y0 = snap_to_pixel(
                        origin.y + bounds.size.height - ((cursor_row + 1) as f64 * cell_h),
                        scale,
                    );
                    let cursor_y1 =
                        snap_to_pixel(origin.y + bounds.size.height - (cursor_row as f64 * cell_h), scale);
                    let cursor_rect = NSRect::new(
                        NSPoint::new(cursor_x0, cursor_y0),
                        NSSize::new(
                            (cursor_x1 - cursor_x0).max(0.0),
                            (cursor_y1 - cursor_y0).max(0.0),
                        ),
                    );
                    let cursor_bg = to_nscolor(cursor_bg_srgba);
                    let cursor_fg = to_nscolor(cursor_fg_srgba);
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
                                unsafe { cast_any_object(&*cursor_fg) },
                                ProtocolObject::from_ref(
                                    objc2_app_kit::NSForegroundColorAttributeName,
                                ),
                            )
                        };
                    let text_attributes =
                        unsafe { NSMutableDictionary::dictionaryWithDictionary(&text_attributes) };
                    unsafe {
                        text_attributes.setObject_forKey(
                            unsafe { cast_any_object(&*state.font) },
                            ProtocolObject::from_ref(objc2_app_kit::NSFontAttributeName),
                        )
                    };
                    unsafe { text.drawAtPoint_withAttributes(text_pos, Some(&text_attributes)) };
                }
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

// Helper to convert termwiz colors to NSColor.
fn to_nscolor(c: SrgbaTuple) -> Retained<NSColor> {
    let (r, g, b, a) = c.to_srgb_u8();
    NSColor::colorWithSRGBRed_green_blue_alpha(
        r as f64 / 255.0,
        g as f64 / 255.0,
        b as f64 / 255.0,
        a as f64 / 255.0,
    )
}

// Helper to resolve cell attributes to foreground and background NSColors.
fn resolve_cell_colors(
    attrs: &CellAttributes,
    default_fg: SrgbaTuple,
    default_bg: SrgbaTuple,
) -> (SrgbaTuple, SrgbaTuple) {
    let mut final_fg = match attrs.foreground() {
        ColorAttribute::Default => default_fg,
        ColorAttribute::PaletteIndex(i) => ansi_palette_color(i),
        ColorAttribute::TrueColorWithDefaultFallback(c) => c.into(),
        ColorAttribute::TrueColorWithPaletteFallback(c, _) => c.into(),
    };

    let mut final_bg = match attrs.background() {
        ColorAttribute::Default => default_bg,
        ColorAttribute::PaletteIndex(i) => ansi_palette_color(i),
        ColorAttribute::TrueColorWithDefaultFallback(c) => c.into(),
        ColorAttribute::TrueColorWithPaletteFallback(c, _) => c.into(),
    };

    if attrs.intensity() == Intensity::Bold {
        if let ColorAttribute::PaletteIndex(idx @ 0..=7) = attrs.foreground() {
            final_fg = ansi_palette_color(idx + 8);
        }
    }

    if attrs.reverse() {
        mem::swap(&mut final_fg, &mut final_bg);
    }

    (final_fg, final_bg)
}

fn color32_to_srgba(color: egui::Color32) -> SrgbaTuple {
    let r = color.r() as f32 / 255.0;
    let g = color.g() as f32 / 255.0;
    let b = color.b() as f32 / 255.0;
    let a = color.a() as f32 / 255.0;
    SrgbaTuple(r, g, b, a)
}

fn snap_to_pixel(value: f64, scale: f64) -> f64 {
    if scale <= 0.0 {
        return value;
    }
    (value * scale).round() / scale
}

fn ansi_palette_color(index: u8) -> SrgbaTuple {
    if index < 16 {
        return ansi_16_srgba(index);
    }
    if index < 232 {
        let idx = index - 16;
        let r = idx / 36;
        let g = (idx / 6) % 6;
        let b = idx % 6;
        return SrgbaTuple(
            RGB_LEVELS[r as usize] as f32 / 255.0,
            RGB_LEVELS[g as usize] as f32 / 255.0,
            RGB_LEVELS[b as usize] as f32 / 255.0,
            1.0,
        );
    }
    let gray = 8u8.saturating_add((index - 232).saturating_mul(10));
    let channel = gray as f32 / 255.0;
    SrgbaTuple(channel, channel, channel, 1.0)
}

fn ansi_16_srgba(index: u8) -> SrgbaTuple {
    match index {
        0 => SrgbaTuple(0.0, 0.0, 0.0, 1.0),
        1 => SrgbaTuple(0.5, 0.0, 0.0, 1.0),
        2 => SrgbaTuple(0.0, 0.5, 0.0, 1.0),
        3 => SrgbaTuple(0.5, 0.5, 0.0, 1.0),
        4 => SrgbaTuple(0.0, 0.0, 0.5, 1.0),
        5 => SrgbaTuple(0.5, 0.0, 0.5, 1.0),
        6 => SrgbaTuple(0.0, 0.5, 0.5, 1.0),
        7 => SrgbaTuple(0.75, 0.75, 0.75, 1.0),
        8 => SrgbaTuple(0.5, 0.5, 0.5, 1.0),
        9 => SrgbaTuple(1.0, 0.0, 0.0, 1.0),
        10 => SrgbaTuple(0.0, 1.0, 0.0, 1.0),
        11 => SrgbaTuple(1.0, 1.0, 0.0, 1.0),
        12 => SrgbaTuple(0.0, 0.0, 1.0, 1.0),
        13 => SrgbaTuple(1.0, 0.0, 1.0, 1.0),
        14 => SrgbaTuple(0.0, 1.0, 1.0, 1.0),
        _ => SrgbaTuple(1.0, 1.0, 1.0, 1.0),
    }
}

const RGB_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
