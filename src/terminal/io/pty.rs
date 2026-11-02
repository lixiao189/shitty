use nix::libc::{
    SIGHUP, SIGWINCH, TIOCSCTTY, TIOCSWINSZ, ioctl, killpg, pid_t, setsid, tcgetpgrp, winsize,
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
    ///
    /// Takes `&self` so the owning session keeps the PTY alive and can
    /// terminate it when its tab closes.
    pub(crate) fn spawn_threads(
        &self,
        tx_output: Sender<Vec<u8>>,
        rx_input: Receiver<PtyEvent>,
        on_output: impl Fn() + Send + 'static,
        on_exit: impl Fn() + Send + 'static,
    ) {
        let master_read = self.master.try_clone().expect("master fd clone failed");
        let master_write = self.master.try_clone().expect("master fd clone failed");
        let shell_pgid = self.shell_pgid;

        thread::spawn(move || {
            loop {
                let mut buffer = [0u8; 8192];
                match read(master_read.as_fd(), &mut buffer) {
                    Ok(0) | Err(_) => {
                        // EOF means every process holding the slave side has
                        // exited (e.g. ^D closed the login shell), so the
                        // session is over.
                        on_exit();
                        break;
                    }
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
            // The last size applied to the PTY, so repeated/coalesced resize
            // events do not spam the shell with redundant SIGWINCH redraws.
            let mut last_resize: Option<(u16, u16)> = None;
            while let Ok(event) = rx_input.recv() {
                match event {
                    PtyEvent::Input(bytes) => {
                        if write(master_write.as_fd(), &bytes).is_err() {
                            break;
                        }
                    }
                    PtyEvent::Resize { cols, rows } => {
                        // Coalesce resize events that are already queued: only
                        // the latest size matters, while any input between them
                        // is still applied, in order.
                        let mut cols = cols;
                        let mut rows = rows;
                        let mut queued_inputs = Vec::new();
                        while let Ok(event) = rx_input.try_recv() {
                            match event {
                                PtyEvent::Resize { cols: c, rows: r } => {
                                    cols = c;
                                    rows = r;
                                }
                                PtyEvent::Input(bytes) => queued_inputs.push(bytes),
                            }
                        }
                        if last_resize != Some((cols, rows)) {
                            apply_resize(master_write.as_raw_fd(), cols, rows, shell_pgid);
                            last_resize = Some((cols, rows));
                        }
                        for bytes in queued_inputs {
                            if write(master_write.as_fd(), &bytes).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });
    }

    /// Hang up the shell and its foreground job so the reader thread sees EOF
    /// and no orphaned processes keep the slave side open.
    pub(crate) fn terminate(&self) {
        let fd = self.master.as_raw_fd();
        let foreground_pgid = unsafe { tcgetpgrp(fd) };
        for pgid in [self.shell_pgid, foreground_pgid] {
            if pgid > 0 {
                unsafe {
                    let _ = killpg(pgid, SIGHUP);
                }
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.terminate();
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
