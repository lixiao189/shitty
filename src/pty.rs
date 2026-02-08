use nix::libc::{SIGWINCH, TIOCSWINSZ, ioctl, killpg, pid_t, tcgetpgrp, winsize};

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
