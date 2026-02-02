#![cfg(target_os = "macos")]

use crate::keymap;
use crate::terminal::grid::TerminalGrid;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType,
    NSBezierPath, NSColor, NSEvent, NSFont, NSResponder, NSStringDrawing, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, NSAttributedStringKey, NSDictionary, NSMutableDictionary, NSPoint, NSRect, NSSize,
    NSString,
};

use std::mem;
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
    _shell_pgid: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let mtm = MainThreadMarker::new().ok_or("must be on main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    let delegate = ShittyAppDelegate::new(mtm, rx_output, tx_input);
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
    ) -> Retained<Self> {
        let view_state = Self::terminal_state_from_channels(rx_output, tx_input);
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars::new(mtm, view_state));
        unsafe { msg_send![super(this), init] }
    }

    fn terminal_state_from_channels(
        rx_output: Receiver<Vec<u8>>,
        tx_input: Sender<Vec<u8>>,
    ) -> TerminalViewState {
        let font = NSFont::userFixedPitchFontOfSize(14.0).unwrap();
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
        }
    }

    fn default_terminal_state() -> TerminalViewState {
        let (tx_output, rx_output) = std::sync::mpsc::channel();
        let (tx_input, _rx_input) = std::sync::mpsc::channel();
        drop(tx_output);
        Self::terminal_state_from_channels(rx_output, tx_input)
    }
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

            let default_bg = to_nscolor(state.grid.default_bg_color());
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

                    let (fg, bg) = resolve_cell_colors(
                        attrs,
                        state.grid.default_fg_color(),
                        state.grid.default_bg_color(),
                    );

                    let run_text = cell_text;

                    let y_pos = origin.y + bounds.size.height - ((row + 1) as f64 * cell_h);

                    let text_y_pos = y_pos
                        + (cell_h
                            - (state.font.ascender() as f64)
                            + (state.font.descender() as f64))
                            / 2.0;

                    let rect = NSRect::new(
                        NSPoint::new(origin.x + col as f64 * cell_w, y_pos),
                        NSSize::new(run_width as f64 * cell_w, cell_h),
                    );

                    if bg != default_bg {
                        bg.set();
                        NSBezierPath::fillRect(rect);
                    }

                    if !run_text.is_empty() && run_text != " " {
                        let text = NSString::from_str(run_text);
                        let text_pos = NSPoint::new(origin.x + col as f64 * cell_w, text_y_pos);

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
        view
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
) -> (Retained<NSColor>, Retained<NSColor>) {
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

    (to_nscolor(final_fg), to_nscolor(final_bg))
}

fn ansi_palette_color(index: u8) -> SrgbaTuple {
    let idx = index as usize;
    let clamped = if idx < ANSI_PALETTE.len() { idx } else { 0 };
    ANSI_PALETTE[clamped]
}

const ANSI_PALETTE: [SrgbaTuple; 16] = [
    SrgbaTuple(0.0, 0.0, 0.0, 1.0),
    SrgbaTuple(0.5, 0.0, 0.0, 1.0),
    SrgbaTuple(0.0, 0.5, 0.0, 1.0),
    SrgbaTuple(0.5, 0.5, 0.0, 1.0),
    SrgbaTuple(0.0, 0.0, 0.5, 1.0),
    SrgbaTuple(0.5, 0.0, 0.5, 1.0),
    SrgbaTuple(0.0, 0.5, 0.5, 1.0),
    SrgbaTuple(0.75, 0.75, 0.75, 1.0),
    SrgbaTuple(0.5, 0.5, 0.5, 1.0),
    SrgbaTuple(1.0, 0.0, 0.0, 1.0),
    SrgbaTuple(0.0, 1.0, 0.0, 1.0),
    SrgbaTuple(1.0, 1.0, 0.0, 1.0),
    SrgbaTuple(0.0, 0.0, 1.0, 1.0),
    SrgbaTuple(1.0, 0.0, 1.0, 1.0),
    SrgbaTuple(0.0, 1.0, 1.0, 1.0),
    SrgbaTuple(1.0, 1.0, 1.0, 1.0),
];
