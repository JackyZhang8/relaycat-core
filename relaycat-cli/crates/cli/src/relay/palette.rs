use super::*;

#[derive(Debug, Clone)]
pub(crate) struct LocalTerminalPalette {
    pub(crate) palette: PaletteState,
    pub(crate) color_query_palette: Option<PaletteState>,
}

impl LocalTerminalPalette {
    pub(crate) fn new(palette: PaletteState, color_query_palette: Option<PaletteState>) -> Self {
        Self {
            palette,
            color_query_palette,
        }
    }

    #[cfg(test)]
    pub(crate) fn answers_color_queries(&self) -> bool {
        self.color_query_palette.is_some()
    }
}

/// The dark palette the child and the phone-facing model use on the Windows GUI
/// pipe bridge. Matches the GUI terminal's fixed dark background (`#0f0b09`) and
/// foreground (`#fbf4ec`) so a TUI's own background, the model's unpainted cells,
/// and the desktop xterm container all render the same dark, seam-free color.
pub(crate) fn gui_bridge_terminal_palette() -> PaletteState {
    let mut palette = default_terminal_palette();
    palette.default_fg = TerminalColor::Rgb {
        r: 0xfb,
        g: 0xf4,
        b: 0xec,
    };
    palette.default_bg = TerminalColor::Rgb {
        r: 0x0f,
        g: 0x0b,
        b: 0x09,
    };
    palette.cursor = TerminalColor::Rgb {
        r: 0xf2,
        g: 0x78,
        b: 0x3f,
    };
    palette
}

pub(crate) fn local_terminal_palette() -> LocalTerminalPalette {
    // On the Windows GUI pipe bridge the "host terminal" is the GUI's xterm,
    // which always renders on a fixed dark palette (see relaycat-gui styles).
    // Windows exposes no queryable terminal palette, so without this both the
    // phone-facing model and the child default to a LIGHT palette: the phone
    // paints unpainted cells white and codex/opencode theme themselves for a
    // white terminal, producing the light background and light/dark mismatch
    // reported on the phone. Report the dark GUI palette for both the model and
    // the child's color queries so the desktop, the phone and the tools agree.
    if crate::gui_bridge::gui_bridge_pipe_mode() {
        let dark = gui_bridge_terminal_palette();
        return LocalTerminalPalette::new(dark.clone(), Some(dark));
    }
    let explicit_theme = terminal_palette_from_explicit_theme();
    let queried = query_local_terminal_palette();
    let colorfgbg = terminal_palette_from_colorfgbg();
    let palette = explicit_theme
        .or_else(|| queried.clone())
        .or(colorfgbg)
        .unwrap_or_else(default_terminal_palette);

    LocalTerminalPalette::new(palette, queried)
}

pub(crate) fn child_color_query_palette(
    local_palette: &LocalTerminalPalette,
    session_kind: &SessionKind,
) -> Option<PaletteState> {
    local_palette
        .color_query_palette
        .clone()
        .or_else(|| (session_kind.as_str() == "codex").then(light_terminal_palette))
}

pub(crate) fn terminal_palette_from_explicit_theme() -> Option<PaletteState> {
    if let Ok(theme) = env::var("RELAYCAT_TERMINAL_THEME") {
        match theme.trim().to_ascii_lowercase().as_str() {
            "dark" => return Some(dark_terminal_palette()),
            "light" => return Some(light_terminal_palette()),
            _ => {}
        }
    }
    None
}

pub(crate) fn terminal_palette_from_colorfgbg() -> Option<PaletteState> {
    let colorfgbg = env::var("COLORFGBG").ok()?;
    let parts: Vec<u16> = colorfgbg
        .split([';', ':'])
        .filter_map(|part| part.parse::<u16>().ok())
        .collect();
    let [.., fg, bg] = parts.as_slice() else {
        return None;
    };

    let mut palette = default_terminal_palette();
    palette.default_fg = xterm_indexed_color(*fg);
    palette.default_bg = xterm_indexed_color(*bg);
    palette.cursor = palette.default_fg.clone();
    Some(palette)
}

pub(crate) fn light_terminal_palette() -> PaletteState {
    let mut palette = default_terminal_palette();
    palette.default_fg = TerminalColor::Rgb { r: 0, g: 0, b: 0 };
    palette.default_bg = TerminalColor::Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    palette.cursor = TerminalColor::Rgb { r: 0, g: 0, b: 0 };
    palette
}

#[cfg(unix)]
pub(crate) fn query_local_terminal_palette() -> Option<PaletteState> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    if !stdin.is_terminal() || !stdout.is_terminal() {
        return None;
    }

    let fd = stdin.as_raw_fd();
    // SAFETY: fcntl reads flags for a valid stdin file descriptor.
    let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if original_flags < 0 {
        return None;
    }
    // SAFETY: fcntl updates flags for a valid stdin file descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }
    let _guard = FdFlagsGuard {
        fd,
        flags: original_flags,
    };

    {
        let mut handle = stdout.lock();
        if handle
            .write_all(&terminal_palette_query_sequence())
            .is_err()
        {
            return None;
        }
        if handle.flush().is_err() {
            return None;
        }
    }

    let mut response = Vec::new();
    let mut chunk = [0_u8; 512];
    let start = Instant::now();
    let mut last_read: Option<Instant> = None;
    while start.elapsed() < Duration::from_millis(180)
        && last_read.is_none_or(|last| last.elapsed() < Duration::from_millis(25))
    {
        // SAFETY: chunk points to valid writable memory and fd is stdin.
        let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if read > 0 {
            response.extend_from_slice(&chunk[..read as usize]);
            last_read = Some(Instant::now());
            continue;
        }
        if read == 0 {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        let err = io::Error::last_os_error();
        if matches!(
            err.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        break;
    }

    terminal_palette_from_osc_responses(&response)
}

#[cfg(not(unix))]
pub(crate) fn query_local_terminal_palette() -> Option<PaletteState> {
    None
}

#[cfg(unix)]
pub(crate) struct FdFlagsGuard {
    fd: i32,
    flags: i32,
}

#[cfg(unix)]
impl Drop for FdFlagsGuard {
    fn drop(&mut self) {
        // SAFETY: flags were read from this fd with F_GETFL before being changed.
        let _ = unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.flags) };
    }
}

#[cfg(unix)]
pub(crate) fn terminal_palette_query_sequence() -> Vec<u8> {
    let mut query = b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07".to_vec();
    for index in 0..16 {
        query.extend_from_slice(format!("\x1b]4;{index};?\x07").as_bytes());
    }
    query
}

#[cfg(unix)]
pub(crate) fn terminal_palette_from_osc_responses(bytes: &[u8]) -> Option<PaletteState> {
    let mut palette = default_terminal_palette();
    let mut found = false;
    for seq in terminal_string_control_sequences(bytes) {
        let Some(body) = seq.strip_prefix(b"\x1b]") else {
            continue;
        };
        let body = trim_string_control_terminator(body);
        if let Some(color) = body.strip_prefix(b"10;").and_then(parse_osc_rgb_color) {
            palette.default_fg = color;
            found = true;
            continue;
        }
        if let Some(color) = body.strip_prefix(b"11;").and_then(parse_osc_rgb_color) {
            palette.default_bg = color;
            found = true;
            continue;
        }
        if let Some(color) = body.strip_prefix(b"12;").and_then(parse_osc_rgb_color) {
            palette.cursor = color;
            found = true;
            continue;
        }
        if let Some(rest) = body.strip_prefix(b"4;") {
            let mut parts = rest.split(|byte| *byte == b';');
            while let (Some(index), Some(color)) = (parts.next(), parts.next()) {
                let Some(index) = ascii_u16(index) else {
                    continue;
                };
                let Some(color) = parse_osc_rgb_color(color) else {
                    continue;
                };
                if let Some(slot) = palette.ansi.get_mut(usize::from(index)) {
                    *slot = color;
                    found = true;
                }
            }
        }
    }
    found.then_some(palette)
}

#[cfg(unix)]
pub(crate) fn parse_osc_rgb_color(bytes: &[u8]) -> Option<TerminalColor> {
    let rest = bytes.strip_prefix(b"rgb:")?;
    let mut channels = rest.split(|byte| *byte == b'/');
    let r = parse_x_color_component(channels.next()?)?;
    let g = parse_x_color_component(channels.next()?)?;
    let b = parse_x_color_component(channels.next()?)?;
    if channels.next().is_some() {
        return None;
    }
    Some(TerminalColor::Rgb { r, g, b })
}

#[cfg(unix)]
pub(crate) fn parse_x_color_component(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() || bytes.len() > 4 || !bytes.iter().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let value = u16::from_str_radix(std::str::from_utf8(bytes).ok()?, 16).ok()?;
    match bytes.len() {
        1 => u8::try_from(value * 17).ok(),
        2 => u8::try_from(value).ok(),
        3 => u8::try_from(value >> 4).ok(),
        4 => u8::try_from(value >> 8).ok(),
        _ => None,
    }
}

pub(crate) fn is_host_terminal_string_query(sequence: &[u8]) -> bool {
    let Some(body) = sequence
        .strip_prefix(b"\x1b]")
        .or_else(|| sequence.strip_prefix(b"\x1bP"))
    else {
        return false;
    };

    body.windows(2).any(|window| window == b";?")
        || body.starts_with(b"+q")
        || body.starts_with(b"$q")
}

pub(crate) fn is_terminal_color_query(sequence: &[u8]) -> bool {
    let Some(body) = sequence.strip_prefix(b"\x1b]") else {
        return false;
    };
    let body = trim_string_control_terminator(body);
    matches!(body, b"10;?" | b"11;?" | b"12;?") || osc4_color_query_count(body) > 0
}

pub(crate) fn osc4_color_query_count(body: &[u8]) -> usize {
    let Some(rest) = body.strip_prefix(b"4;") else {
        return 0;
    };
    rest.split(|byte| *byte == b';')
        .collect::<Vec<_>>()
        .chunks_exact(2)
        .filter(|pair| pair[0].iter().all(|byte| byte.is_ascii_digit()) && pair[1] == b"?")
        .count()
}

pub(crate) fn terminal_string_control_sequences(bytes: &[u8]) -> Vec<&[u8]> {
    let mut sequences = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let remaining = &bytes[index..];
        if is_terminal_string_control_start(remaining) {
            match local_input_string_control_action(remaining) {
                LocalInputSequenceAction::Drop(len)
                | LocalInputSequenceAction::ToggleWorkMode(len)
                | LocalInputSequenceAction::Pass(len)
                | LocalInputSequenceAction::PassAndMirror(len, _) => {
                    sequences.push(&remaining[..len]);
                    index += len;
                    continue;
                }
                LocalInputSequenceAction::Pending => break,
            }
        }
        index += 1;
    }
    sequences
}

pub(crate) fn trim_string_control_terminator(bytes: &[u8]) -> &[u8] {
    if let Some(body) = bytes.strip_suffix(b"\x07") {
        return body;
    }
    if let Some(body) = bytes.strip_suffix(b"\x1b\\") {
        return body;
    }
    bytes
}

