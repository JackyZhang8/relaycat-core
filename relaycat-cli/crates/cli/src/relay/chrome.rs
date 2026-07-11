use super::*;

pub(crate) const DISCONNECT_TITLE_HINT: &str = "relaycat: disconnected, reconnecting...";

pub(crate) type SharedTerminalStatusBar = Arc<TerminalStatusBar>;

#[derive(Debug, Default)]
pub(crate) struct TerminalStatusBar {
    pub(crate) last_child_title: Mutex<Option<Vec<u8>>>,
    pub(crate) hint_active: AtomicBool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TerminalChromeTitleContext {
    pub(crate) project_name: Option<String>,
    pub(crate) session_kind: Option<String>,
}

impl TerminalChromeTitleContext {
    #[cfg(test)]
    pub(crate) fn new(project_name: Option<&str>, session_kind: Option<&str>) -> Self {
        Self {
            project_name: normalized_title_part(project_name),
            session_kind: normalized_title_part(session_kind),
        }
    }

    pub(crate) fn for_target(target: &TargetCommand) -> Self {
        Self {
            project_name: project_name_for_terminal_chrome(target.cwd.as_deref()),
            session_kind: normalized_title_part(Some(target.session_kind.as_str())),
        }
    }

    #[cfg(test)]
    pub(crate) fn fallback() -> Self {
        Self::default()
    }

    pub(crate) fn display_name(&self) -> String {
        match (self.session_kind.as_deref(), self.project_name.as_deref()) {
            (Some(session_kind), Some(project_name)) if session_kind != project_name => {
                format!("{session_kind}·{project_name}")
            }
            (Some(session_kind), _) => session_kind.to_string(),
            (None, Some(project_name)) => project_name.to_string(),
            (None, None) => "relaycat".to_string(),
        }
    }
}

impl TerminalStatusBar {
    pub(crate) fn observe_child_output(&self, bytes: &[u8]) {
        let Some(seq) = extract_latest_osc_title(bytes) else {
            return;
        };
        if let Ok(mut guard) = self.last_child_title.lock() {
            *guard = Some(seq);
        }
    }

    pub(crate) fn mode_title_sequence(
        &self,
        mode: PtyWorkModeKind,
        host_size: PtySize,
        title_context: &TerminalChromeTitleContext,
    ) -> Option<Vec<u8>> {
        if self.hint_active.load(Ordering::Acquire) {
            return None;
        }
        Some(terminal_chrome_sequence(mode, host_size, title_context))
    }

    pub(crate) fn set_mode_title(
        &self,
        mode: PtyWorkModeKind,
        host_size: PtySize,
        title_context: &TerminalChromeTitleContext,
    ) {
        let Some(sequence) = self.mode_title_sequence(mode, host_size, title_context) else {
            return;
        };
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(&sequence);
        let _ = stdout.flush();
    }

    pub(crate) fn clear_hint_for_mode_title(&self) {
        self.hint_active.store(false, Ordering::Release);
    }

    pub(crate) fn force_set_mode_title(
        &self,
        mode: PtyWorkModeKind,
        host_size: PtySize,
        title_context: &TerminalChromeTitleContext,
    ) {
        self.clear_hint_for_mode_title();
        let sequence = terminal_chrome_sequence(mode, host_size, title_context);
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(&sequence);
        let _ = stdout.flush();
    }

    pub(crate) fn set_hint(&self, hint: &str) {
        if self.hint_active.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut stdout = io::stdout().lock();
        let _ = write!(stdout, "\x1b]2;{hint}\x07");
        let _ = stdout.flush();
    }

    pub(crate) fn clear_hint(&self) {
        if !self.hint_active.swap(false, Ordering::AcqRel) {
            return;
        }
        let restore = self
            .last_child_title
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let mut stdout = io::stdout().lock();
        match restore {
            Some(seq) => {
                let _ = stdout.write_all(&seq);
            }
            None => {
                let _ = stdout.write_all(b"\x1b]2;\x07");
            }
        }
        let _ = stdout.flush();
    }
}

pub(crate) fn local_content_pty_size(host_size: PtySize) -> PtySize {
    let (cols, rows) = bounded_terminal_size(host_size.cols, host_size.rows);
    PtySize {
        cols,
        rows,
        pixel_width: host_size.pixel_width,
        pixel_height: host_size.pixel_height,
    }
}

#[cfg(test)]
pub(crate) fn terminal_chrome_line(mode: PtyWorkModeKind, cols: u16) -> String {
    terminal_chrome_line_for_language(
        mode,
        cols,
        &TerminalChromeTitleContext::fallback(),
        CliLanguage::from_system_locale(),
    )
}

pub(crate) fn terminal_chrome_line_for_language(
    mode: PtyWorkModeKind,
    cols: u16,
    title_context: &TerminalChromeTitleContext,
    language: CliLanguage,
) -> String {
    let mode_label = match mode {
        PtyWorkModeKind::Remote => language.t("Remote/App on", "远程/App开"),
        PtyWorkModeKind::Local => language.t("Local/App off", "本地/App关"),
    };
    let toggle_hint = language.t("Ctrl+G to toggle", "按 Ctrl+G 切换");
    let display_name = title_context.display_name();
    format!("{display_name} 【{mode_label}】{toggle_hint}")
        .chars()
        .take(usize::from(cols.saturating_sub(1).max(1)))
        .collect()
}

pub(crate) fn terminal_chrome_sequence(
    mode: PtyWorkModeKind,
    host_size: PtySize,
    title_context: &TerminalChromeTitleContext,
) -> Vec<u8> {
    let top = terminal_chrome_line_for_language(
        mode,
        host_size.cols,
        title_context,
        CliLanguage::from_system_locale(),
    );
    format!("\x1b]2;{top}\x07").into_bytes()
}

pub(crate) fn init_terminal_chrome_sequence(
    mode: PtyWorkModeKind,
    host_size: PtySize,
    title_context: &TerminalChromeTitleContext,
) -> Vec<u8> {
    let top = terminal_chrome_line_for_language(
        mode,
        host_size.cols,
        title_context,
        CliLanguage::from_system_locale(),
    );
    format!("\x1b]2;{top}\x07").into_bytes()
}

pub(crate) fn reset_terminal_chrome_sequence() -> Vec<u8> {
    b"\x1b]2;relaycat\x07".to_vec()
}

pub(crate) fn configure_child_terminal_env(
    command: &mut CommandBuilder,
    session_kind: &SessionKind,
) {
    // Set TERM explicitly so the shell always gets correct cursor-movement and
    // line-editing escape sequences regardless of how relaycat was launched
    // (IDE, SSH session, launchd service, etc. may leave TERM unset or wrong).
    // Without this, readline/zle can't output backspace sequences and cursor
    // positioning breaks, causing characters to overlay instead of replace.
    command.env("TERM", "xterm-256color");
    // Stale COLUMNS/LINES inherited from whatever launched this process (e.g.
    // the shell that started the desktop GUI) override the child PTY's real
    // winsize in programs that consult them, so the TUI lays out at the old
    // desktop width even after the PTY is resized to the phone grid.
    command.env_remove("COLUMNS");
    command.env_remove("LINES");

    // Suppress escape sequences that apps without a full VT emulator can't render:
    // - PROMPT_EOL_MARK: zsh's reverse-video "%" shown at end of partial lines
    // - TERM_PROGRAM / ITERM_SESSION_ID: iTerm2/terminal shell integration (OSC 7, OSC 133, etc.)
    // - DISABLE_AUTO_TITLE: oh-my-zsh title-setting escape sequences
    // - TERM_SESSION_ID: macOS Terminal.app shell integration
    command.env("PROMPT_EOL_MARK", "");
    command.env("TERM_PROGRAM", "");
    command.env("TERM_SESSION_ID", "");
    command.env("ITERM_SESSION_ID", "");
    command.env("ITERM_SHELL_INTEGRATION_INSTALLED", "");
    command.env("DISABLE_AUTO_TITLE", "true");
    if session_kind.as_str() == "codex" {
        command.env_remove("RELAYCAT_TERMINAL_THEME");
        command.env_remove("COLORFGBG");
    }
    #[cfg(windows)]
    if session_kind.is_shell() {
        command.env("PROMPT", "$P$G");
    }
}

pub(crate) fn init_terminal_chrome(
    mode: PtyWorkModeKind,
    host_size: PtySize,
    title_context: &TerminalChromeTitleContext,
) {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(&init_terminal_chrome_sequence(
        mode,
        host_size,
        title_context,
    ));
    let _ = stdout.flush();
}

pub(crate) fn render_terminal_chrome_for_status_bar(
    mode: PtyWorkModeKind,
    status_bar: &TerminalStatusBar,
    fallback_size: PtySize,
    title_context: &TerminalChromeTitleContext,
) {
    let size = terminal_chrome_render_size(current_terminal_size(), fallback_size);
    status_bar.set_mode_title(mode, size, title_context);
}

pub(crate) fn force_render_terminal_chrome_for_status_bar(
    mode: PtyWorkModeKind,
    status_bar: &TerminalStatusBar,
    fallback_size: PtySize,
    title_context: &TerminalChromeTitleContext,
) {
    let size = terminal_chrome_render_size(current_terminal_size(), fallback_size);
    status_bar.force_set_mode_title(mode, size, title_context);
}

pub(crate) fn terminal_chrome_render_size(
    current_size: Option<PtySize>,
    fallback_size: PtySize,
) -> PtySize {
    current_size.unwrap_or(fallback_size)
}

/// RemoteMode clamps the child PTY to `min(app, host)`, leaving the host
/// terminal cells right of / below the clamped grid showing whatever the child
/// drew at the old size. Full-screen children repaint only the clamped region
/// after the resize, so reset attributes and clear the host screen first; the
/// leftover margin then renders as the default background.
pub(crate) fn clear_host_terminal_for_remote_clamp(
    pty_cols: u16,
    pty_rows: u16,
    host: Option<PtySize>,
) {
    if !should_clear_host_for_remote_clamp(pty_cols, pty_rows, host) {
        return;
    }
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(b"\x1b[0m\x1b[2J\x1b[H");
    let _ = stdout.flush();
}

pub(crate) fn should_clear_host_for_remote_clamp(
    pty_cols: u16,
    pty_rows: u16,
    host: Option<PtySize>,
) -> bool {
    let Some(host) = host else {
        return false;
    };
    host.cols > pty_cols || host.rows > pty_rows
}

pub(crate) fn clear_terminal_chrome() {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(&reset_terminal_chrome_sequence());
    let _ = stdout.flush();
}

pub(crate) fn extract_latest_osc_title(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut latest: Option<Vec<u8>> = None;
    for seq in terminal_string_control_sequences(bytes) {
        let Some(body) = seq.strip_prefix(b"\x1b]") else {
            continue;
        };
        let body = trim_string_control_terminator(body);
        let Some(semicolon) = body.iter().position(|byte| *byte == b';') else {
            continue;
        };
        let code = &body[..semicolon];
        if matches!(code, b"0" | b"1" | b"2") {
            latest = Some(seq.to_vec());
        }
    }
    latest
}
