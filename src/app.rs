use nix::libc::{ioctl, setsid, TIOCSCTTY};
use nix::pty::openpty;
use nix::unistd::{read, write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

#[cfg(target_os = "macos")]
use crate::macos_ui;

#[cfg(not(target_os = "macos"))]
use crate::pty::{apply_resize, PtyEvent};
#[cfg(not(target_os = "macos"))]
use crate::ui::TerminalUI;

#[cfg(target_os = "macos")]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let pty_result = openpty(None, None)?;
    let master_fd = pty_result.master;
    let slave_fd = pty_result.slave;
    let shell_pgid = spawn_shell(&slave_fd);

    let (tx_pty_output, rx_pty_output) = channel::<Vec<u8>>();
    let (tx_pty_input, rx_pty_input) = channel::<Vec<u8>>();

    let master_read = master_fd.try_clone().expect("master fd clone failed");
    let master_write = master_fd.try_clone().expect("master fd clone failed");
    let master_ui = master_fd;
    let slave_ui = slave_fd.try_clone().expect("slave fd clone failed");

    spawn_pty_threads(master_read, master_write, tx_pty_output, rx_pty_input);

    macos_ui::run_native(rx_pty_output, tx_pty_input, master_ui, slave_ui, shell_pgid)?;

    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    use eframe::egui;
    use egui::IconData;
    use image::GenericImageView;
    use std::fs;
    use std::sync::Arc;

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

#[cfg(not(target_os = "macos"))]
fn configure_visuals(cc: &eframe::CreationContext<'_>) {
    use eframe::egui;
    cc.egui_ctx.set_visuals(egui::Visuals::dark());
    cc.egui_ctx.style_mut(|style| {
        style.visuals.panel_fill = egui::Color32::BLACK;
    });
}

#[cfg(not(target_os = "macos"))]
fn configure_fonts(cc: &eframe::CreationContext<'_>) {
    use eframe::egui;
    use std::fs;
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

// PTY threads for the macOS AppKit path: no PtyEvent, AppKit timer drives rendering and
// resize is handled directly in the view's setFrameSize handler.
#[cfg(target_os = "macos")]
fn spawn_pty_threads(
    master_read: OwnedFd,
    master_write: OwnedFd,
    tx_pty_output: Sender<Vec<u8>>,
    rx_pty_input: Receiver<Vec<u8>>,
) {
    thread::spawn(move || loop {
        let mut buffer = [0u8; 8192];
        match read(master_read.as_fd(), &mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                if tx_pty_output.send(buffer[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    });

    thread::spawn(move || {
        while let Ok(bytes) = rx_pty_input.recv() {
            if write(master_write.as_fd(), &bytes).is_err() {
                break;
            }
        }
    });
}

// PTY threads for the egui fallback path: PtyEvent carries both input bytes and resize
// signals, and ctx.request_repaint() wakes the egui loop on new output.
#[cfg(not(target_os = "macos"))]
fn spawn_pty_threads(
    master_read: OwnedFd,
    master_write: OwnedFd,
    tx_pty_output: Sender<Vec<u8>>,
    rx_pty_input: Receiver<PtyEvent>,
    ctx: eframe::egui::Context,
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
