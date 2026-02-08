use eframe::egui::{self};
use std::sync::mpsc::{Receiver, Sender};

use crate::keymap::append_input_from_event;
use crate::pty::PtyEvent;
use crate::terminal::TerminalGrid;

pub(crate) struct TerminalUI {
    rx_pty_output: Receiver<Vec<u8>>,
    tx_pty_input: Sender<PtyEvent>,
    grid: TerminalGrid,
    font_id: egui::FontId,
    // Cache cell size to avoid recalculating every frame
    cached_cell_size: Option<(f32, f32)>,
}

impl TerminalUI {
    pub(crate) fn new(rx_pty_output: Receiver<Vec<u8>>, tx_pty_input: Sender<PtyEvent>) -> Self {
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
        // Track if we need to request a repaint
        let mut needs_repaint = false;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.grid.default_bg()))
            .show(ctx, |ui| {
                let (cell_w, cell_h) = self.cell_size(ctx);
                let available = ui.available_size();
                let cols = ((available.x / cell_w).floor() as usize).max(1);
                let rows = ((available.y / cell_h).floor() as usize).max(1);

                // Check for resize
                if self.grid.resize(cols, rows) {
                    let _ = self.tx_pty_input.send(PtyEvent::Resize {
                        cols: cols as u16,
                        rows: rows as u16,
                    });
                    needs_repaint = true;
                }

                // Process incoming data from PTY
                let mut received_data = false;
                while let Ok(bytes) = self.rx_pty_output.try_recv() {
                    self.grid.process_pty_bytes(&bytes);
                    received_data = true;
                }

                // Check if terminal content has changed
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

                        if let Some(cell) = &cell
                            && self.grid.cell_underline(cell)
                        {
                            let y = pos.y + cell_h - 1.0;
                            let rect = egui::Rect::from_min_size(
                                egui::pos2(pos.x, y),
                                egui::vec2(cell_w, 1.0),
                            );
                            painter.rect_filled(rect, 0.0, fg);
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
