use eframe::egui::{self};
use nix::libc::{ioctl, killpg, pid_t, tcgetpgrp, winsize, SIGWINCH, TIOCSWINSZ};
use std::os::fd::AsRawFd;
use std::sync::mpsc::{Receiver, Sender};

use crate::keymap::append_input_from_event;
use crate::terminal::TerminalGrid;

pub(crate) struct TerminalUI {
    rx: Receiver<Vec<u8>>,
    tx_input: Sender<Vec<u8>>,
    grid: TerminalGrid,
    font_id: egui::FontId,
    master_fd: std::os::fd::OwnedFd,
    slave_fd: std::os::fd::OwnedFd,
    shell_pgid: pid_t,
    // Cache cell size to avoid recalculating every frame
    cached_cell_size: Option<(f32, f32)>,
}

impl TerminalUI {
    pub(crate) fn new(
        rx: Receiver<Vec<u8>>,
        tx_input: Sender<Vec<u8>>,
        master_fd: std::os::fd::OwnedFd,
        slave_fd: std::os::fd::OwnedFd,
        shell_pgid: pid_t,
    ) -> Self {
        Self {
            rx,
            tx_input,
            grid: TerminalGrid::new(80, 24),
            font_id: egui::FontId::monospace(14.0),
            master_fd,
            slave_fd,
            shell_pgid,
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

impl eframe::App for TerminalUI {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Track if we need to request a repaint
        let mut needs_repaint = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.grid.default_bg()))
            .show(ctx, |ui| {
                let (cell_w, cell_h) = self.cell_size(ctx);
                let available = ui.available_size();
                let cols = (available.x / cell_w).floor() as usize;
                let rows = (available.y / cell_h).floor() as usize;
                let cols = cols.max(1);
                let rows = rows.max(1);

                // Check for resize
                if self.grid.resize(cols, rows) {
                    set_winsize_raw(self.master_fd.as_raw_fd(), cols as u16, rows as u16);
                    let pgid = unsafe { tcgetpgrp(self.slave_fd.as_raw_fd()) };
                    let target_pgid = if pgid > 0 { pgid } else { self.shell_pgid };
                    unsafe {
                        let _ = killpg(target_pgid, SIGWINCH);
                    }
                    needs_repaint = true;
                }

                // Process incoming data from PTY
                let mut received_data = false;
                while let Ok(bytes) = self.rx.try_recv() {
                    self.grid.write_bytes(&bytes);
                    received_data = true;
                }

                // Check if terminal content has changed
                if received_data && self.grid.has_changes() {
                    needs_repaint = true;
                }

                let (rect, response) = ui.allocate_at_least(available, egui::Sense::click());
                if response.clicked() {
                    ui.memory_mut(|memory| memory.request_focus(response.id));
                }

                let mut input_bytes = Vec::new();
                ctx.input(|input| {
                    let mods = input.modifiers;
                    for event in &input.events {
                        append_input_from_event(event, mods, &mut input_bytes);
                    }
                });
                if !input_bytes.is_empty() {
                    let _ = self.tx_input.send(input_bytes);
                }

                let painter = ui.painter_at(rect);
                let origin = rect.min;

                painter.rect_filled(rect, 0.0, self.grid.default_bg());

                let default_bg = self.grid.default_bg();

                // Cache font_id reference to avoid cloning in loop
                let font_id = &self.font_id;

                // Render all cells
                for row in 0..rows {
                    for col in 0..cols {
                        let cell = self.grid.get_cell(row, col);
                        let (text, fg, bg) = if let Some(cell) = &cell {
                            let (fg, bg) = self.grid.resolve_cell_colors(cell);
                            (cell.text.as_str(), fg, bg)
                        } else {
                            ("", egui::Color32::WHITE, default_bg)
                        };

                        let pos = grid_to_screen(origin, cell_w, cell_h, row, col);
                        let rect = egui::Rect::from_min_size(pos, egui::vec2(cell_w, cell_h));

                        if bg != default_bg {
                            painter.rect_filled(rect, 0.0, bg);
                        }

                        if !text.is_empty() && text != " " {
                            painter.text(pos, egui::Align2::LEFT_TOP, text, font_id.clone(), fg);
                        }

                        if let Some(cell) = &cell {
                            if self.grid.cell_underline(cell) {
                                let y = pos.y + cell_h - 1.0;
                                let rect = egui::Rect::from_min_size(
                                    egui::pos2(pos.x, y),
                                    egui::vec2(cell_w, 1.0),
                                );
                                painter.rect_filled(rect, 0.0, fg);
                            }
                        }
                    }
                }

                // Render cursor
                if self.grid.cursor_visible() {
                    let (cursor_row, cursor_col) = self.grid.cursor_pos();
                    let cursor_cell = self.grid.get_cell(cursor_row, cursor_col);
                    let (cell_fg, cell_bg) = cursor_cell
                        .as_ref()
                        .map(|cell| self.grid.resolve_cell_colors(cell))
                        .unwrap_or((egui::Color32::WHITE, self.grid.default_bg()));
                    let cursor_pos = grid_to_screen(origin, cell_w, cell_h, cursor_row, cursor_col);
                    let cursor_rect =
                        egui::Rect::from_min_size(cursor_pos, egui::vec2(cell_w, cell_h));
                    let cursor_bg = self.grid.cursor_color().unwrap_or_else(|| {
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
                    painter.rect_filled(cursor_rect, 0.0, cursor_bg);
                    painter.text(
                        cursor_pos,
                        egui::Align2::LEFT_TOP,
                        cursor_cell
                            .as_ref()
                            .map(|cell| cell.text.as_str())
                            .unwrap_or(" "),
                        font_id.clone(),
                        cursor_fg,
                    );
                }

                // Mark this render as complete
                self.grid.mark_rendered();
            });

        // Only request repaint when there are actual changes
        if needs_repaint {
            ctx.request_repaint();
        }
    }
}
