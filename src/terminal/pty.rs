use nix::libc::{SIGWINCH, TIOCSCTTY, TIOCSWINSZ, ioctl, killpg, pid_t, setsid, tcgetpgrp, winsize};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::thread;

#[allow(dead_code)]
pub(crate) enum PtyEvent {
    Input(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

pub(crate) fn apply_resize(fd: i32, cols: u16, rows: u16, shell_pgid: pid_t) {
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

pub(crate) fn spawn_shell(slave_fd: &OwnedFd) -> i32 {
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
