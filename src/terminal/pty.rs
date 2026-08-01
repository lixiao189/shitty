use nix::libc::{
    SIGWINCH, TIOCSCTTY, TIOCSWINSZ, ioctl, killpg, pid_t, setsid, tcgetpgrp, winsize,
};
use nix::pty::openpty;
use nix::unistd::{read, write};
use std::error::Error;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

/// Commands sent from the UI to the PTY writer thread.
pub(crate) enum PtyEvent {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

/// A live shell session: the master side of a PTY plus the shell's process group.
pub(crate) struct Pty {
    master: OwnedFd,
    shell_pgid: pid_t,
}

impl Pty {
    /// Open a fresh PTY and start the login shell inside it.
    pub(crate) fn spawn() -> Result<Self, Box<dyn Error>> {
        let pty = openpty(None, None)?;
        let shell_pgid = spawn_shell(&pty.slave);
        Ok(Self {
            master: pty.master,
            shell_pgid,
        })
    }

    /// Pump data between the PTY and the UI:
    /// - a reader thread forwards shell output to `tx_output`, invoking `on_output` after each chunk;
    /// - a writer thread applies `PtyEvent`s received on `rx_input` to the master fd.
    pub(crate) fn spawn_threads(
        self,
        tx_output: Sender<Vec<u8>>,
        rx_input: Receiver<PtyEvent>,
        on_output: impl Fn() + Send + 'static,
    ) {
        let master_read = self.master.try_clone().expect("master fd clone failed");
        let master_write = self.master.try_clone().expect("master fd clone failed");
        let shell_pgid = self.shell_pgid;

        thread::spawn(move || {
            loop {
                let mut buffer = [0u8; 8192];
                match read(master_read.as_fd(), &mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx_output.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                        on_output();
                    }
                }
            }
        });

        thread::spawn(move || {
            while let Ok(event) = rx_input.recv() {
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
}

/// Tell the kernel about a new terminal size and notify the foreground process group.
fn apply_resize(fd: i32, cols: u16, rows: u16, shell_pgid: pid_t) {
    let ws = winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        let _ = ioctl(fd, TIOCSWINSZ, &ws);
        let pgid = tcgetpgrp(fd);
        let target_pgid = if pgid > 0 { pgid } else { shell_pgid };
        let _ = killpg(target_pgid, SIGWINCH);
    }
}

/// Spawn `/bin/zsh` as the controlling process of the PTY; returns its pid.
fn spawn_shell(slave_fd: &OwnedFd) -> pid_t {
    let ctty_fd = slave_fd.try_clone().expect("slave fd clone failed");
    let mut child = unsafe {
        Command::new("/bin/zsh")
            .stdin(slave_fd.try_clone().expect("slave fd clone failed"))
            .stdout(slave_fd.try_clone().expect("slave fd clone failed"))
            .stderr(slave_fd.try_clone().expect("slave fd clone failed"))
            .pre_exec(move || {
                let _ = setsid();
                let _ = ioctl(ctty_fd.as_raw_fd(), TIOCSCTTY as _, 0);
                Ok(())
            })
            .spawn()
    }
    .expect("Failed to spawn shell");
    let pid = child.id() as i32;
    thread::spawn(move || {
        let _ = child.wait();
    });
    pid
}
