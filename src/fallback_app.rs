use crate::terminal::grid::TerminalGrid;
use crate::terminal::keymap::append_input_from_event;
use crate::terminal::pty::{Pty, PtyEvent};
use eframe::egui::{self, IconData};
use image::GenericImageView;
use std::fs;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pty = Pty::spawn()?;
    let (tx_pty_output, rx_pty_output) = channel::<Vec<u8>>();
    let (tx_pty_input, rx_pty_input) = channel::<PtyEvent>();

    let mut viewport = egui::ViewportBuilder::default();
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }

    eframe::run_native(
        "shitty",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| {
            configure_ui(cc);

            let ctx = cc.egui_ctx.clone();
            pty.spawn_threads(tx_pty_output, rx_pty_input, move || ctx.request_repaint());

            Ok(Box::new(TerminalUI::new(rx_pty_output, tx_pty_input)))
        }),
    )
    .map_err(Into::into)
}

fn load_icon() -> Option<Arc<IconData>> {
    let icon_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/icon.png");
    let img = image::open(&icon_path).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = img.dimensions();
    Some(Arc::new(IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    }))
}

fn configure_ui(cc: &eframe::CreationContext<'_>) {
    cc.egui_ctx.set_visuals(egui::Visuals::dark());
    cc.egui_ctx.style_mut(|style| {
        style.visuals.panel_fill = egui::Color32::BLACK;
    });
    load_terminal_font(cc);
}

fn load_terminal_font(cc: &eframe::CreationContext<'_>) {
    let font_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/MonacoNerdFontMono-Regular.ttf");
    let Ok(font_data) = fs::read(&font_path) else {
        return;
    };
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

struct TerminalUI {
    rx_pty_output: Receiver<Vec<u8>>,
    tx_pty_input: Sender<PtyEvent>,
    grid: TerminalGrid,
    font_id: egui::FontId,
    cached_cell_size: Option<(f32, f32)>,
    needs_repaint: bool,
}

impl TerminalUI {
    fn new(rx_pty_output: Receiver<Vec<u8>>, tx_pty_input: Sender<PtyEvent>) -> Self {
        Self {
            rx_pty_output,
            tx_pty_input,
            grid: TerminalGrid::new(80, 24),
            font_id: egui::FontId::monospace(14.0),
            cached_cell_size: None,
            needs_repaint: false,
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

    fn resize_grid(&mut self, available: egui::Vec2, cell_w: f32, cell_h: f32) {
        let cols = ((available.x / cell_w).floor() as usize).max(1);
        let rows = ((available.y / cell_h).floor() as usize).max(1);
        if !self.grid.resize(cols, rows) {
            return;
        }
        let _ = self.tx_pty_input.send(PtyEvent::Resize {
            cols: cols as u16,
            rows: rows as u16,
        });
        self.needs_repaint = true;
    }

    fn drain_pty_output(&mut self) {
        let mut received_data = false;
        while let Ok(bytes) = self.rx_pty_output.try_recv() {
            self.grid.process_pty_bytes(&bytes);
            received_data = true;
        }
        if received_data && self.grid.has_changes() {
            self.needs_repaint = true;
        }
    }

    fn send_input_events(&mut self, ctx: &egui::Context) {
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
    }

    fn paint(&self, ui: &mut egui::Ui, cell_w: f32, cell_h: f32) {
        let (rect, _response) = ui.allocate_at_least(ui.available_size(), egui::Sense::click());
        let painter = ui.painter_at(rect);
        let origin = rect.min;
        let default_bg = self.grid.default_bg();

        painter.rect_filled(rect, 0.0, default_bg);

        for row in 0..self.grid.rows() {
            for run in self.grid.row_runs(row) {
                let pos = grid_to_screen(origin, cell_w, cell_h, row, run.col);
                let run_rect =
                    egui::Rect::from_min_size(pos, egui::vec2(cell_w * run.width as f32, cell_h));

                if run.bg != default_bg {
                    painter.rect_filled(run_rect, 0.0, run.bg);
                }

                if !run.text.is_empty() && !run.text.trim().is_empty() {
                    painter.text(
                        pos,
                        egui::Align2::LEFT_TOP,
                        &run.text,
                        self.font_id.clone(),
                        run.fg.into(),
                    );
                }

                if run.underline {
                    let underline_rect = egui::Rect::from_min_size(
                        egui::pos2(pos.x, pos.y + cell_h - 1.0),
                        egui::vec2(cell_w * run.width as f32, 1.0),
                    );
                    painter.rect_filled(underline_rect, 0.0, run.fg);
                }
            }
        }

        if let Some(cursor) = self.grid.cursor() {
            let pos = grid_to_screen(origin, cell_w, cell_h, cursor.row, cursor.col);
            let cursor_rect = egui::Rect::from_min_size(pos, egui::vec2(cell_w, cell_h));
            painter.rect_filled(cursor_rect, 0.0, cursor.bg);
            painter.text(
                pos,
                egui::Align2::LEFT_TOP,
                &cursor.text,
                self.font_id.clone(),
                cursor.fg.into(),
            );
        }
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
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(self.grid.default_bg().into()))
            .show(ctx, |ui| {
                let (cell_w, cell_h) = self.cell_size(ctx);
                self.resize_grid(ui.available_size(), cell_w, cell_h);
                self.drain_pty_output();
                self.send_input_events(ctx);
                self.paint(ui, cell_w, cell_h);

                self.grid.mark_rendered();
            });

        if self.needs_repaint {
            ctx.request_repaint();
            self.needs_repaint = false;
        }
    }
}
