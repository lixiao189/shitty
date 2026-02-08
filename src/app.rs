use eframe::egui;
use egui::IconData;
use image::GenericImageView;
use nix::libc::{TIOCSCTTY, ioctl, setsid};
use nix::pty::openpty;
use nix::unistd::{read, write};
use std::fs;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::thread;

use crate::pty::{PtyEvent, apply_resize};
use crate::ui::TerminalUI;

pub fn run() -> eframe::Result<()> {
    let pty_result = openpty(None, None)
        .map_err(|e| eframe::Error::AppCreation(format!("openpty failed: {e}").into()))?;

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
}

fn configure_visuals(cc: &eframe::CreationContext<'_>) {
    cc.egui_ctx.set_visuals(egui::Visuals::dark());
    cc.egui_ctx.style_mut(|style| {
        style.visuals.panel_fill = egui::Color32::BLACK;
    });
}

fn configure_fonts(cc: &eframe::CreationContext<'_>) {
    let font_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("assets/JetBrainsMonoNerdFontMono-Regular.ttf");
    if let Ok(font_data) = fs::read(&font_path) {
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "jbmono".to_string(),
            egui::FontData::from_owned(font_data).into(),
        );
        // Only set monospace font family since this is a terminal
        fonts
            .families
            .insert(egui::FontFamily::Monospace, vec!["jbmono".to_string()]);
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

fn spawn_pty_threads(
    master_read: OwnedFd,
    master_write: OwnedFd,
    tx_pty_output: std::sync::mpsc::Sender<Vec<u8>>,
    rx_pty_input: std::sync::mpsc::Receiver<PtyEvent>,
    ctx: egui::Context,
    shell_pgid: i32,
) {
    // Pty receive thread
    thread::spawn(move || {
        loop {
            // Increased buffer size from 2048 to 8192 for better throughput
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
        }
    });

    // Pty send thread
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
