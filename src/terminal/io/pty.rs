use nix::libc::{
    SIGHUP, SIGWINCH, TIOCSCTTY, TIOCSWINSZ, getpwuid, getuid, ioctl, killpg, pid_t, setsid,
    tcgetpgrp, winsize,
};
use nix::pty::openpty;
use nix::unistd::{read, write};
use std::error::Error;
use std::ffi::{CStr, OsStr, OsString};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
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

/// `TERM` advertised to programs in the shell. libghostty-vt implements the
/// xterm-256color feature set, and every macOS ships its terminfo entry.
const TERM: &str = "xterm-256color";
/// Locale used when the app inherits none (launched from Finder/Dock, launchd
/// sets no `LANG`); without it zsh treats UTF-8 as raw bytes.
const FALLBACK_LANG: &str = "en_US.UTF-8";

/// The user's home directory: `$HOME`, else the passwd entry.
fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        return Some(PathBuf::from(home));
    }
    unsafe {
        let pw = getpwuid(getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = CStr::from_ptr((*pw).pw_dir).to_bytes();
        Some(PathBuf::from(OsStr::from_bytes(dir)))
    }
}

/// Variables set on the shell regardless of how the app was launched.
/// `lang` is the inherited `LANG`, if any; it is kept when present.
fn shell_env(lang: Option<OsString>) -> Vec<(&'static str, OsString)> {
    let lang = lang
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| FALLBACK_LANG.into());
    vec![
        ("TERM", TERM.into()),
        ("COLORTERM", "truecolor".into()),
        ("TERM_PROGRAM", env!("CARGO_PKG_NAME").into()),
        ("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION").into()),
        ("LANG", lang),
    ]
}

/// Spawn `/bin/zsh` as a login shell in the user's home directory, as the
/// controlling process of the PTY; returns its pid.
fn spawn_shell(slave_fd: &OwnedFd) -> pid_t {
    let ctty_fd = slave_fd.try_clone().expect("slave fd clone failed");
    let mut command = Command::new("/bin/zsh");
    // A leading `-` in argv[0] makes zsh a login shell, as Terminal.app does,
    // so `.zprofile` (Homebrew's PATH setup, …) runs even when launched from
    // the Dock with launchd's minimal environment.
    command
        .arg0("-zsh")
        .envs(shell_env(std::env::var_os("LANG")));
    if let Some(home) = home_dir() {
        command.current_dir(home);
    }
    let mut child = unsafe {
        command
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(env: &'a [(&str, OsString)], key: &str) -> Option<&'a OsString> {
        env.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    #[test]
    fn shell_env_sets_term_and_fallback_lang() {
        let env = shell_env(None);
        assert_eq!(lookup(&env, "TERM").unwrap(), "xterm-256color");
        assert_eq!(lookup(&env, "LANG").unwrap(), FALLBACK_LANG);
        assert_eq!(
            lookup(&shell_env(Some("".into())), "LANG").unwrap(),
            FALLBACK_LANG
        );
    }

    #[test]
    fn shell_env_keeps_inherited_lang() {
        let env = shell_env(Some("zh_CN.UTF-8".into()));
        assert_eq!(lookup(&env, "LANG").unwrap(), "zh_CN.UTF-8");
    }

    #[test]
    fn home_dir_is_absolute() {
        assert!(home_dir().unwrap().is_absolute());
    }
}
