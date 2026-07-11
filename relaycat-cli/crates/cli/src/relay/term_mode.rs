use super::*;

#[cfg(unix)]
pub(crate) struct LocalTerminalModeGuard {
    pub(crate) original: Option<libc::termios>,
}

#[cfg(unix)]
impl LocalTerminalModeGuard {
    pub(crate) fn new() -> Self {
        let original = enable_local_raw_terminal_mode();
        disable_local_focus_reporting();
        Self { original }
    }
}

#[cfg(unix)]
impl Drop for LocalTerminalModeGuard {
    fn drop(&mut self) {
        if let Some(original) = self.original.take() {
            restore_local_terminal_mode(original);
        }
        disable_local_focus_reporting();
    }
}

#[cfg(not(unix))]
pub(crate) struct LocalTerminalModeGuard {
    pub(crate) raw_mode_enabled: bool,
}

#[cfg(not(unix))]
impl LocalTerminalModeGuard {
    pub(crate) fn new() -> Self {
        let raw_mode_enabled = enable_local_raw_terminal_mode();
        disable_local_focus_reporting();
        Self { raw_mode_enabled }
    }
}

#[cfg(not(unix))]
impl Drop for LocalTerminalModeGuard {
    fn drop(&mut self) {
        if self.raw_mode_enabled {
            restore_local_terminal_mode();
        }
        disable_local_focus_reporting();
    }
}

#[cfg(not(unix))]
pub(crate) fn enable_local_raw_terminal_mode() -> bool {
    if !io::stdin().is_terminal() {
        return false;
    }
    match enable_raw_mode() {
        Ok(()) => true,
        Err(err) => {
            relaycat_log(
                "WARN",
                format!("failed to enable local terminal raw mode: {err}"),
            );
            false
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn restore_local_terminal_mode() {
    if let Err(err) = disable_raw_mode() {
        relaycat_log(
            "WARN",
            format!("failed to restore local terminal mode: {err}"),
        );
    }
}

#[cfg(unix)]
pub(crate) fn enable_local_raw_terminal_mode() -> Option<libc::termios> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return None;
    }
    let fd = stdin.as_raw_fd();
    let mut original = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: fd is stdin and original points to valid writable memory for termios.
    if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: tcgetattr succeeded and initialized original.
    let original = unsafe { original.assume_init() };
    let raw = raw_terminal_mode_from(original);
    // SAFETY: fd is stdin and raw is a valid termios value derived from tcgetattr.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
        return None;
    }
    Some(original)
}

#[cfg(unix)]
pub(crate) fn restore_local_terminal_mode(original: libc::termios) {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return;
    }
    // SAFETY: fd is stdin and original is a termios value previously read from tcgetattr.
    let _ = unsafe { libc::tcsetattr(stdin.as_raw_fd(), libc::TCSANOW, &original) };
}

pub(crate) fn disable_local_focus_reporting() {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(b"\x1b[?1004l");
    let _ = stdout.flush();
}

#[cfg(unix)]
pub(crate) fn current_terminal_size() -> Option<PtySize> {
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        return None;
    }
    let mut size = MaybeUninit::<libc::winsize>::zeroed();
    // SAFETY: fd is stdout and size points to valid writable memory for winsize.
    if unsafe { libc::ioctl(stdout.as_raw_fd(), libc::TIOCGWINSZ, size.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: ioctl succeeded and initialized size.
    let size = unsafe { size.assume_init() };
    if size.ws_row == 0 || size.ws_col == 0 {
        return None;
    }
    Some(PtySize {
        rows: size.ws_row,
        cols: size.ws_col,
        pixel_width: size.ws_xpixel,
        pixel_height: size.ws_ypixel,
    })
}

#[cfg(not(unix))]
pub(crate) fn current_terminal_size() -> Option<PtySize> {
    // No controlling tty here (Windows GUI pipe bridge / plain Windows CLI). On
    // the bridge the GUI host publishes its terminal size, which we treat as the
    // "host" terminal so the app-size clamp and Ctrl-G local resize use the real
    // desktop dimensions; otherwise this stays `None` as before.
    crate::gui_bridge::bridge_terminal_size().map(|(cols, rows)| PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })
}

#[cfg(unix)]
pub(crate) fn raw_terminal_mode_from(original: libc::termios) -> libc::termios {
    let mut raw = original;
    // SAFETY: cfmakeraw mutates a valid termios value in place.
    unsafe { libc::cfmakeraw(&mut raw) };
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    raw
}
