use eframe::egui::{self};
use egui::IconData;
use image::GenericImageView;
use nix::libc::{ioctl, setsid, TIOCSCTTY};
use nix::pty::openpty;
use nix::unistd::{read, write};
use std::fs;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use crate::terminal::keymap::append_input_from_event;
use crate::terminal::pty::{apply_resize, PtyEvent};
use crate::terminal::grid::TerminalGrid;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pty_result = openpty(None, None)?;
    let master_fd = pty_result.master;
    let slave_fd = pty_result.slave;
    let shell_pgid = spawn_shell(&slave_fd);

    let icon_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
    let icon_data = if let Ok(img) = image::open(&icon_path) {
        let rgba = img.to_rgba8();
        let (width, height) = img.dimensions();
        Some(Arc::new(IconData {
            rgba: rgba.into_raw(),
            width,
            height,
        }))
    } else {
        None
    };

    let mut viewport = egui::ViewportBuilder::default();
    if let Some(icon) = icon_data {
        viewport = viewport.with_icon(icon);
    }

    eframe::run_native(
        "shitty",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| {
            configure_visuals(cc);
            configure_fonts(cc);

            let (tx_pty_output, rx_pty_output) = channel::<Vec<u8>>();
            let (tx_pty_input, rx_pty_input) = channel::<PtyEvent>();
            let ctx = cc.egui_ctx.clone();

            let master_read = master_fd.try_clone().expect("master fd clone failed");
            let master_write = master_fd;

            spawn_pty_threads(
                master_read,
                master_write,
                tx_pty_output,
                rx_pty_input,
                ctx,
                shell_pgid,
            );

            Ok(Box::new(TerminalUI::new(rx_pty_output, tx_pty_input)))
        }),
    )
    .map_err(Into::into)
}

fn configure_visuals(cc: &eframe::CreationContext<'_>) {
    cc.egui_ctx.set_visuals(egui::Visuals::dark());
    cc.egui_ctx.style_mut(|style| {
        style.visuals.panel_fill = egui::Color32::BLACK;
    });
}

fn configure_fonts(cc: &eframe::CreationContext<'_>) {
    let font_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/MonacoNerdFontMono-Regular.ttf");
    if let Ok(font_data) = fs::read(&font_path) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "monaco".to_string(),
            egui::FontData::from_owned(font_data).into(),
        );
        fonts
            .families
            .insert(egui::FontFamily::Monospace, vec!["monaco".to_string()]);
        cc.egui_ctx.set_fonts(fonts);
    }
}

fn spawn_shell(slave_fd: &OwnedFd) -> i32 {
    unsafe {
        let ctty_fd = slave_fd.try_clone().expect("slave fd clone failed");
        let mut child = Command::new("/bin/zsh")
            .stdin(slave_fd.try_clone().expect("slave fd clone failed"))
            .stdout(slave_fd.try_clone().expect("slave fd clone failed"))
            .stderr(slave_fd.try_clone().expect("slave fd clone failed"))
            .pre_exec(move || {
                let _ = setsid();
                let _ = ioctl(ctty_fd.as_raw_fd(), TIOCSCTTY as _, 0);
                Ok(())
            })
            .spawn()
            .expect("Failed to spawn shell");
        let pid = child.id() as i32;
        thread::spawn(move || {
            let _ = child.wait();
        });
        pid
    }
}

// PtyEvent carries both input bytes and resize signals; ctx.request_repaint()
// wakes the egui loop whenever new PTY output arrives.
fn spawn_pty_threads(
    master_read: OwnedFd,
    master_write: OwnedFd,
    tx_pty_output: Sender<Vec<u8>>,
    rx_pty_input: Receiver<PtyEvent>,
    ctx: egui::Context,
    shell_pgid: i32,
) {
    thread::spawn(move || loop {
        let mut buffer = [0u8; 8192];
        match read(master_read.as_fd(), &mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if tx_pty_output.send(buffer[..n].to_vec()).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
            Err(_) => break,
        }
    });

    thread::spawn(move || {
        while let Ok(event) = rx_pty_input.recv() {
            match event {
                PtyEvent::Input(bytes) => {
                    if write(master_write.as_fd(), &bytes).is_err() {
                        break;
                    }
                }
                PtyEvent::Resize { cols, rows } => {
                    apply_resize(master_write.as_raw_fd(), cols, rows, shell_pgid);
                }
            }
        }
    });
}

// Convert terminal Color32 to egui Color32
fn to_egui_color(c: crate::terminal::color::Color32) -> egui::Color32 {
    egui::Color32::from_rgba_premultiplied(c.r, c.g, c.b, c.a)
}

struct TerminalUI {
    rx_pty_output: Receiver<Vec<u8>>,
    tx_pty_input: Sender<PtyEvent>,
    grid: TerminalGrid,
    font_id: egui::FontId,
    cached_cell_size: Option<(f32, f32)>,
}

impl TerminalUI {
    fn new(rx_pty_output: Receiver<Vec<u8>>, tx_pty_input: Sender<PtyEvent>) -> Self {
        Self {
            rx_pty_output,
            tx_pty_input,
            grid: TerminalGrid::new(80, 24),
            font_id: egui::FontId::monospace(14.0),
            cached_cell_size: None,
        }
    }

    fn cell_size(&mut self, ctx: &egui::Context) -> (f32, f32) {
        if let Some(size) = self.cached_cell_size {
            return size;
        }
        let size = ctx.fonts_mut(|fonts| {
            (
                fonts.glyph_width(&self.font_id, 'W'),
                fonts.row_height(&self.font_id),
            )
        });
        self.cached_cell_size = Some(size);
        size
    }
}

fn grid_to_screen(
    origin: egui::Pos2,
    cell_w: f32,
    cell_h: f32,
    row: usize,
    col: usize,
) -> egui::Pos2 {
    egui::pos2(
        origin.x + col as f32 * cell_w,
        origin.y + row as f32 * cell_h,
    )
}

impl eframe::App for TerminalUI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut needs_repaint = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(to_egui_color(self.grid.default_bg())))
            .show(ctx, |ui| {
                let (cell_w, cell_h) = self.cell_size(ctx);
                let available = ui.available_size();
                let cols = ((available.x / cell_w).floor() as usize).max(1);
                let rows = ((available.y / cell_h).floor() as usize).max(1);

                if self.grid.resize(cols, rows) {
                    let _ = self.tx_pty_input.send(PtyEvent::Resize {
                        cols: cols as u16,
                        rows: rows as u16,
                    });
                    needs_repaint = true;
                }

                let mut received_data = false;
                while let Ok(bytes) = self.rx_pty_output.try_recv() {
                    self.grid.process_pty_bytes(&bytes);
                    received_data = true;
                }

                if received_data && self.grid.has_changes() {
                    needs_repaint = true;
                }

                let (rect, _response) = ui.allocate_at_least(available, egui::Sense::click());

                let mut input_bytes = Vec::new();
                ctx.input(|input| {
                    let mods = input.modifiers;
                    for event in &input.events {
                        append_input_from_event(event, mods, &mut input_bytes);
                    }
                });
                if !input_bytes.is_empty() {
                    let _ = self.tx_pty_input.send(PtyEvent::Input(input_bytes));
                }

                let painter = ui.painter_at(rect);
                let origin = rect.min;

                painter.rect_filled(rect, 0.0, to_egui_color(self.grid.default_bg()));

                let default_bg = self.grid.default_bg();
                let font_id = &self.font_id;

                for row in 0..rows {
                    let mut col = 0;
                    while col < cols {
                        let cell = self.grid.get_cell(row, col);

                        if cell.as_ref().is_some_and(|c| c.wide_continuation) {
                            col += 1;
                            continue;
                        }

                        let (text, fg, bg, underline, col_span) = if let Some(cell) = &cell {
                            let (fg, bg) = self.grid.resolve_cell_colors(cell);
                            let span = if cell.wide { 2 } else { 1 };
                            (cell.text.as_str(), fg, bg, cell.underline, span)
                        } else {
                            ("", crate::terminal::color::Color32::WHITE, default_bg, false, 1)
                        };

                        let pos = grid_to_screen(origin, cell_w, cell_h, row, col);
                        let rect = egui::Rect::from_min_size(
                            pos,
                            egui::vec2(cell_w * col_span as f32, cell_h),
                        );

                        if bg != default_bg {
                            painter.rect_filled(rect, 0.0, to_egui_color(bg));
                        }

                        if !text.is_empty() && text != " " {
                            painter.text(pos, egui::Align2::LEFT_TOP, text, font_id.clone(), to_egui_color(fg));
                        }

                        if underline {
                            let y = pos.y + cell_h - 1.0;
                            let underline_rect = egui::Rect::from_min_size(
                                egui::pos2(pos.x, y),
                                egui::vec2(cell_w * col_span as f32, 1.0),
                            );
                            painter.rect_filled(underline_rect, 0.0, to_egui_color(fg));
                        }

                        col += col_span;
                    }
                }

                if self.grid.cursor_visible() {
                    let (cursor_row, cursor_col) = self.grid.cursor_pos();
                    let cursor_cell = self.grid.get_cell(cursor_row, cursor_col);
                    let (cell_fg, cell_bg) = cursor_cell
                        .as_ref()
                        .map(|cell| self.grid.resolve_cell_colors(cell))
                        .unwrap_or((crate::terminal::color::Color32::WHITE, self.grid.default_bg()));
                    let cursor_pos = grid_to_screen(origin, cell_w, cell_h, cursor_row, cursor_col);
                    let cursor_rect =
                        egui::Rect::from_min_size(cursor_pos, egui::vec2(cell_w, cell_h));
                    let cursor_bg = self.grid.cursor_color().unwrap_or_else(|| {
                        if cell_fg == cell_bg {
                            crate::terminal::color::Color32::WHITE
                        } else {
                            cell_fg
                        }
                    });
                    let cursor_fg = if cursor_bg == cell_bg {
                        cell_fg
                    } else {
                        cell_bg
                    };
                    painter.rect_filled(cursor_rect, 0.0, to_egui_color(cursor_bg));
                    painter.text(
                        cursor_pos,
                        egui::Align2::LEFT_TOP,
                        cursor_cell
                            .as_ref()
                            .map(|cell| cell.text.as_str())
                            .unwrap_or(" "),
                        font_id.clone(),
                        to_egui_color(cursor_fg),
                    );
                }

                self.grid.mark_rendered();
            });

        if needs_repaint {
            ctx.request_repaint();
        }
    }
}
