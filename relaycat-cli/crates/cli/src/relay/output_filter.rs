use super::*;
use unicode_width::UnicodeWidthChar;
#[cfg(test)]
use unicode_width::UnicodeWidthStr;

const CODEX_WELCOME_TOP_LEFT: &[u8] = "╭".as_bytes();
const CODEX_WELCOME_BOTTOM_LEFT: &[u8] = "╰".as_bytes();
const CODEX_WELCOME_MAX_BYTES: usize = 32 * 1024;

#[derive(Debug)]
pub(crate) struct CodexWelcomeNormalizer {
    enabled: bool,
    marker_probe: Vec<u8>,
    candidate: Vec<u8>,
    in_candidate: bool,
    normal_line_prefix: Vec<u8>,
    normal_line_started_by_newline: bool,
    normalized_since_take: usize,
}

impl CodexWelcomeNormalizer {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            marker_probe: Vec::new(),
            candidate: Vec::new(),
            in_candidate: false,
            normal_line_prefix: Vec::new(),
            normal_line_started_by_newline: false,
            normalized_since_take: 0,
        }
    }

    pub(crate) fn filter(&mut self, bytes: &[u8], cols: u16) -> Vec<u8> {
        if !self.enabled {
            return bytes.to_vec();
        }

        let mut output = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if self.in_candidate {
                self.candidate.push(byte);
                if self.candidate.len() > CODEX_WELCOME_MAX_BYTES {
                    let original = std::mem::take(&mut self.candidate);
                    output.extend_from_slice(&original);
                    self.observe_normal_bytes(&original);
                    self.in_candidate = false;
                    continue;
                }
                if byte == b'\n' && contains_bytes(&self.candidate, CODEX_WELCOME_BOTTOM_LEFT) {
                    if contains_bytes(&self.candidate, b"OpenAI Codex") {
                        output.extend_from_slice(&normalize_codex_welcome_card(
                            &self.candidate,
                            cols,
                        ));
                        self.normalized_since_take += 1;
                    } else {
                        output.append(&mut self.candidate);
                    }
                    self.candidate.clear();
                    self.in_candidate = false;
                    self.normal_line_prefix.clear();
                    self.normal_line_started_by_newline = true;
                }
                continue;
            }

            self.marker_probe.push(byte);
            while !CODEX_WELCOME_TOP_LEFT.starts_with(&self.marker_probe) {
                let emitted = self.marker_probe.remove(0);
                output.push(emitted);
                self.observe_normal_byte(emitted);
            }
            if self.marker_probe == CODEX_WELCOME_TOP_LEFT {
                let visible_prefix = terminal_visible_text(&self.normal_line_prefix);
                let starts_after_newline =
                    self.normal_line_started_by_newline && visible_prefix.is_empty();
                let starts_after_terminal_home =
                    visible_prefix.is_empty() && line_was_explicitly_homed(&self.normal_line_prefix);
                if starts_after_newline || starts_after_terminal_home {
                    self.candidate.append(&mut self.marker_probe);
                    self.in_candidate = true;
                } else {
                    let marker = std::mem::take(&mut self.marker_probe);
                    output.extend_from_slice(&marker);
                    self.observe_normal_bytes(&marker);
                }
            }
        }
        output
    }

    pub(crate) fn finish(&mut self) -> Vec<u8> {
        let mut output = Vec::new();
        output.append(&mut self.marker_probe);
        output.append(&mut self.candidate);
        self.in_candidate = false;
        output
    }

    pub(crate) fn take_normalized_count(&mut self) -> usize {
        std::mem::take(&mut self.normalized_since_take)
    }

    fn observe_normal_bytes(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.observe_normal_byte(byte);
        }
    }

    fn observe_normal_byte(&mut self, byte: u8) {
        if byte == b'\n' {
            self.normal_line_prefix.clear();
            self.normal_line_started_by_newline = true;
            return;
        }
        if self.normal_line_prefix.len() < 4 * 1024 {
            self.normal_line_prefix.push(byte);
        }
    }
}

fn line_was_explicitly_homed(bytes: &[u8]) -> bool {
    let mut index = 0;
    let mut last_cursor_position_is_home = None;
    while index < bytes.len() {
        if let Some(len) = terminal_control_sequence_len(&bytes[index..]) {
            let sequence = &bytes[index..index + len];
            if matches!(sequence.last(), Some(b'H' | b'f')) && sequence.starts_with(b"\x1b[") {
                last_cursor_position_is_home = Some(cursor_position_is_home(sequence));
            }
            index += len;
        } else if let Some((_, len)) = next_utf8_char(&bytes[index..]) {
            index += len;
        } else {
            index += 1;
        }
    }
    last_cursor_position_is_home == Some(true)
}

fn cursor_position_is_home(sequence: &[u8]) -> bool {
    let Some(params) = sequence.get(2..sequence.len().saturating_sub(1)) else {
        return false;
    };
    let mut params = params.split(|byte| *byte == b';');
    let row = params.next().unwrap_or_default();
    let col = params.next().unwrap_or_default();
    params.next().is_none()
        && [row, col]
            .iter()
            .all(|param| param.is_empty() || *param == b"1")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn normalize_codex_welcome_card(card: &[u8], cols: u16) -> Vec<u8> {
    if cols < 4 {
        return card.to_vec();
    }
    let mut output = Vec::with_capacity(card.len());
    // Codex can position its first border cell at column 2 before writing the
    // welcome card. Start the rebuilt card at column 1 so its deliberately
    // unused final column remains unused in the physical terminal as well.
    output.push(b'\r');
    let mut start = 0;
    for (index, byte) in card.iter().enumerate() {
        if *byte == b'\n' {
            output.extend_from_slice(&normalize_codex_welcome_line(&card[start..=index], cols));
            start = index + 1;
        }
    }
    if start < card.len() {
        output.extend_from_slice(&card[start..]);
    }
    output
}

fn normalize_codex_welcome_line(line: &[u8], cols: u16) -> Vec<u8> {
    // Keep the final PTY column unused. Filling it leaves many VT emulators in
    // the delayed-autowrap state, so the next printable byte can create a
    // one-line continuation at column zero even though the card was already
    // narrowed to the reported terminal width.
    let card_cols = cols.saturating_sub(1);
    let visible = terminal_visible_text(line);
    let Some(first) = visible.chars().next() else {
        return line.to_vec();
    };
    match first {
        '╭' => rebuild_codex_border_line(line, card_cols, '╭', '╮'),
        '╰' => rebuild_codex_border_line(line, card_cols, '╰', '╯'),
        '│' => rebuild_codex_content_line(line, card_cols),
        _ => line.to_vec(),
    }
}

fn rebuild_codex_border_line(line: &[u8], cols: u16, left: char, right: char) -> Vec<u8> {
    let (body, ending) = split_line_ending(line);
    let first_printable = first_printable_offset(body).unwrap_or(0);
    let mut output = Vec::with_capacity(body.len());
    append_sgr_sequences(&mut output, &body[..first_printable]);
    output.extend_from_slice(left.to_string().as_bytes());
    output.extend_from_slice("─".repeat(usize::from(cols.saturating_sub(2))).as_bytes());
    output.extend_from_slice(right.to_string().as_bytes());
    output.extend_from_slice(b"\x1b[0m");
    output.extend_from_slice(ending);
    output
}

fn rebuild_codex_content_line(line: &[u8], cols: u16) -> Vec<u8> {
    let (body, ending) = split_line_ending(line);
    let target_before_right_border = usize::from(cols.saturating_sub(1));
    let mut output = Vec::with_capacity(body.len());
    let mut index = 0;
    let mut width = 0;
    let mut saw_printable = false;
    while index < body.len() {
        if let Some(len) = terminal_control_sequence_len(&body[index..]) {
            let sequence = &body[index..index + len];
            if is_sgr_sequence(sequence) {
                output.extend_from_slice(sequence);
            }
            index += len;
            continue;
        }
        let Some((ch, len)) = next_utf8_char(&body[index..]) else {
            output.push(body[index]);
            index += 1;
            continue;
        };
        let char_width = ch.width().unwrap_or(0);
        if saw_printable && ch == '│' {
            break;
        }
        if width + char_width > target_before_right_border {
            break;
        }
        output.extend_from_slice(&body[index..index + len]);
        width += char_width;
        saw_printable = saw_printable || char_width > 0;
        index += len;
    }
    output.extend(std::iter::repeat_n(
        b' ',
        target_before_right_border.saturating_sub(width),
    ));
    output.extend_from_slice(b"\x1b[0m\x1b[2m");
    output.extend_from_slice("│".as_bytes());
    output.extend_from_slice(b"\x1b[0m");
    output.extend_from_slice(ending);
    output
}

fn append_sgr_sequences(output: &mut Vec<u8>, bytes: &[u8]) {
    let mut index = 0;
    while index < bytes.len() {
        if let Some(len) = terminal_control_sequence_len(&bytes[index..]) {
            let sequence = &bytes[index..index + len];
            if is_sgr_sequence(sequence) {
                output.extend_from_slice(sequence);
            }
            index += len;
            continue;
        }
        if let Some((_, len)) = next_utf8_char(&bytes[index..]) {
            index += len;
        } else {
            index += 1;
        }
    }
}

fn is_sgr_sequence(sequence: &[u8]) -> bool {
    sequence.starts_with(b"\x1b[") && sequence.ends_with(b"m")
}

fn split_line_ending(line: &[u8]) -> (&[u8], &[u8]) {
    if let Some(body) = line.strip_suffix(b"\r\n") {
        (body, b"\r\n")
    } else if let Some(body) = line.strip_suffix(b"\n") {
        (body, b"\n")
    } else {
        (line, b"")
    }
}

fn first_printable_offset(bytes: &[u8]) -> Option<usize> {
    let mut index = 0;
    while index < bytes.len() {
        if let Some(len) = terminal_control_sequence_len(&bytes[index..]) {
            index += len;
            continue;
        }
        return Some(index);
    }
    None
}

fn terminal_control_sequence_len(bytes: &[u8]) -> Option<usize> {
    if bytes.first().copied() != Some(0x1b) {
        return None;
    }
    match bytes.get(1).copied() {
        Some(b'[') => bytes[2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map(|offset| offset + 3),
        Some(b']' | b'P') => {
            let mut index = 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    return Some(index + 1);
                }
                if bytes[index] == 0x1b && bytes.get(index + 1) == Some(&b'\\') {
                    return Some(index + 2);
                }
                index += 1;
            }
            None
        }
        Some(_) => Some(bytes.len().min(2)),
        None => None,
    }
}

fn next_utf8_char(bytes: &[u8]) -> Option<(char, usize)> {
    let width = match *bytes.first()? {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return None,
    };
    let text = std::str::from_utf8(bytes.get(..width)?).ok()?;
    Some((text.chars().next()?, width))
}

fn terminal_visible_text(bytes: &[u8]) -> String {
    let mut text = String::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(len) = terminal_control_sequence_len(&bytes[index..]) {
            index += len;
            continue;
        }
        match bytes[index] {
            b'\r' => index += 1,
            b'\n' => {
                text.push('\n');
                index += 1;
            }
            _ => {
                if let Some((ch, len)) = next_utf8_char(&bytes[index..]) {
                    text.push(ch);
                    index += len;
                } else {
                    index += 1;
                }
            }
        }
    }
    text
}

#[cfg(test)]
pub(crate) fn terminal_visible_text_for_test(bytes: &[u8]) -> String {
    terminal_visible_text(bytes)
}

#[cfg(test)]
pub(crate) fn terminal_display_width_for_test(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

#[derive(Debug)]
pub(crate) struct LocalOutputFilter {
    pub(crate) pending: Vec<u8>,
    pub(crate) color_query_palette: Option<PaletteState>,
    // When set (codex sessions), alternate-screen switches (47/1047/1049) are
    // stripped from the app-facing `remote_output` only, so terminal_core keeps
    // modelling codex in the primary screen and its conversation stays in the
    // app's managed scrollback. The host's `local_output` keeps the switch so
    // codex renders in the real terminal's native alternate screen — a fixed,
    // scrollback-free viewport where its bottom status/input UI stays pinned
    // instead of scrolling off once output exceeds one screen.
    pub(crate) strip_alternate_screen_from_remote: bool,
    // Current clamped PTY viewport height and the physical host terminal
    // height. In a shared session the PTY is sized to the (smaller) app
    // viewport, so a full-screen *primary-buffer* app (e.g. macOS `top`, which
    // does not use the alternate screen) sets its scroll region to the PTY
    // height. On the taller host terminal that region traps later output in the
    // top rows while the visible prompt below stays frozen. When `host_rows`
    // exceeds `pty_rows` we expand such bottom-anchored scroll regions to the
    // host height on `local_output` only (see `host_scroll_region_sequence`).
    // 0 means unknown -> no rewriting.
    pub(crate) pty_cols: u16,
    pub(crate) pty_rows: u16,
    pub(crate) host_rows: u16,
    // When true, answer a child's cursor-position query (`\x1b[6n` / `\x1b[?6n`)
    // directly by feeding a report back into the PTY, instead of forwarding the
    // query to the host terminal and relying on it to respond. Set for the
    // desktop GUI relay child on Windows: there the child runs under a ConPTY
    // and the GUI xterm's CPR response, written back into the inner
    // pseudoconsole's input, is not delivered to the child, so the shell
    // (PSReadLine) blocks forever waiting for it and never draws its prompt.
    // On a real terminal (or non-GUI) the host answers, so this stays false.
    pub(crate) answer_cursor_position_query: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LocalOutputFilterResult {
    pub(crate) local_output: Vec<u8>,
    pub(crate) remote_output: Vec<u8>,
    pub(crate) pty_input: Vec<u8>,
}

impl LocalOutputFilter {
    #[cfg(test)]
    pub(crate) fn new_with_color_query_policy(
        palette: PaletteState,
        answer_color_queries: bool,
    ) -> Self {
        Self::new_with_color_query_palette(answer_color_queries.then_some(palette))
    }

    pub(crate) fn new_with_color_query_palette(color_query_palette: Option<PaletteState>) -> Self {
        Self {
            pending: Vec::new(),
            color_query_palette,
            strip_alternate_screen_from_remote: false,
            pty_cols: 0,
            pty_rows: 0,
            host_rows: 0,
            answer_cursor_position_query: false,
        }
    }

    pub(crate) fn filter(&mut self, bytes: &[u8]) -> LocalOutputFilterResult {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);

        let mut result = LocalOutputFilterResult {
            local_output: Vec::with_capacity(input.len()),
            remote_output: Vec::with_capacity(input.len()),
            pty_input: Vec::new(),
        };
        let mut index = 0;
        while index < input.len() {
            let remaining = &input[index..];
            if remaining.starts_with(b"\x1b[") {
                let Some(body_final_offset) = remaining[2..]
                    .iter()
                    .position(|byte| (0x40..=0x7e).contains(byte))
                else {
                    self.pending.extend_from_slice(remaining);
                    break;
                };
                let final_offset = body_final_offset + 2;
                let sequence = &remaining[..=final_offset];
                if self.strip_alternate_screen_from_remote
                    && is_alternate_screen_mode_sequence(sequence)
                {
                    // Keep the alternate-screen switch on the host's local
                    // terminal so codex renders in its native alt-screen buffer
                    // (fixed viewport, no scrollback) and its bottom status/input
                    // UI stays pinned. Strip it only from the app-facing stream.
                    // In both cases still drop host-filtered modes (1004) and keep
                    // any other params of a combined sequence
                    // (e.g. \x1b[?1049;2004l keeps 2004) so legitimate modes are
                    // not lost.
                    if let Some(local_seq) = split_filtered_dec_private_modes(sequence) {
                        result.local_output.extend_from_slice(&local_seq);
                    }
                    if let Some(remote_seq) = split_remote_filtered_dec_private_modes(sequence) {
                        result.remote_output.extend_from_slice(&remote_seq);
                    }
                    index += final_offset + 1;
                    continue;
                }
                if is_cursor_position_query(sequence) {
                    // CPR (\x1b[6n) and DECXCPR (\x1b[?6n) queries.
                    if self.answer_cursor_position_query {
                        // No host terminal will answer (GUI relay child on
                        // Windows), so report a position straight back to the
                        // PTY child or it blocks waiting forever. The cursor
                        // sits at column 1 (the prompt was primed with a fresh
                        // line); a row of 1 matches the long-standing answer.
                        result
                            .pty_input
                            .extend_from_slice(cursor_position_report(sequence));
                    } else if self.strip_alternate_screen_from_remote {
                        // Managed-alt-screen TUIs (codex, opencode) run in a
                        // child PTY sized to the phone grid while the host
                        // terminal keeps its own geometry. Answer directly so
                        // neither the host grid nor a query timeout can affect
                        // the child's later layout decision.
                        result
                            .pty_input
                            .extend_from_slice(cursor_position_report(sequence));
                    } else {
                        // Pass to the host terminal so the shell receives the
                        // real cursor position. Do NOT send to remote
                        // (terminal_core doesn't need it).
                        result.local_output.extend_from_slice(sequence);
                    }
                    index += final_offset + 1;
                    continue;
                }
                if is_text_area_size_query(sequence) {
                    if self.strip_alternate_screen_from_remote
                        && let Some(report) = text_area_size_report(self.pty_rows, self.pty_cols)
                    {
                        result.pty_input.extend_from_slice(&report);
                    }
                    index += final_offset + 1;
                    continue;
                }
                if is_host_terminal_query_or_mode_sequence(sequence) {
                    // For combined DEC private mode sequences that mix filtered
                    // modes (1004) with non-filtered modes (e.g. 1049, 2004),
                    // pass the non-filtered sub-sequence through to both outputs
                    // so legitimate mode changes are not silently dropped.
                    if let Some(passthrough) = split_filtered_dec_private_modes(sequence) {
                        result.local_output.extend_from_slice(&passthrough);
                        result.remote_output.extend_from_slice(&passthrough);
                    }
                    index += final_offset + 1;
                    continue;
                }
                if is_set_scroll_region_sequence(sequence) {
                    // The app (remote) renders a grid exactly `pty_rows` tall,
                    // so the child's region is correct for it verbatim. On the
                    // taller host terminal a bottom-anchored region would trap
                    // later output above the visible prompt, so expand it to the
                    // host height for `local_output` only.
                    result
                        .local_output
                        .extend_from_slice(&host_scroll_region_sequence(
                            sequence,
                            self.pty_rows,
                            self.host_rows,
                        ));
                    result.remote_output.extend_from_slice(sequence);
                    index += final_offset + 1;
                    continue;
                }
                result.local_output.extend_from_slice(sequence);
                result.remote_output.extend_from_slice(sequence);
                index += final_offset + 1;
                continue;
            }
            if remaining.starts_with(b"\x1b]") || remaining.starts_with(b"\x1bP") {
                match local_output_string_control_action(
                    remaining,
                    self.color_query_palette.as_ref(),
                ) {
                    LocalInputSequenceAction::Drop(len) => {
                        index += len;
                        continue;
                    }
                    LocalInputSequenceAction::Pass(len) => {
                        let sequence = &remaining[..len];
                        if !is_osc_title_sequence(sequence) {
                            result.local_output.extend_from_slice(sequence);
                        }
                        result.remote_output.extend_from_slice(&remaining[..len]);
                        index += len;
                        continue;
                    }
                    LocalInputSequenceAction::PassAndMirror(len, pty_response) => {
                        result.pty_input.extend_from_slice(&pty_response);
                        index += len;
                        continue;
                    }
                    LocalInputSequenceAction::ToggleWorkMode(len) => {
                        result.local_output.extend_from_slice(&remaining[..len]);
                        result.remote_output.extend_from_slice(&remaining[..len]);
                        index += len;
                        continue;
                    }
                    LocalInputSequenceAction::Pending => {
                        self.pending.extend_from_slice(remaining);
                        break;
                    }
                }
            }
            if is_host_terminal_control_prefix(remaining) {
                self.pending.extend_from_slice(remaining);
                break;
            }
            result.local_output.push(input[index]);
            result.remote_output.push(input[index]);
            index += 1;
        }

        result
    }
}

pub(crate) fn is_osc_title_sequence(sequence: &[u8]) -> bool {
    let Some(body) = sequence.strip_prefix(b"\x1b]") else {
        return false;
    };
    let body = trim_string_control_terminator(body);
    let Some(semicolon) = body.iter().position(|byte| *byte == b';') else {
        return false;
    };
    matches!(&body[..semicolon], b"0" | b"1" | b"2")
}

impl Default for LocalOutputFilter {
    fn default() -> Self {
        Self::new_with_color_query_palette(None)
    }
}

pub(crate) fn is_alternate_screen_mode_sequence(sequence: &[u8]) -> bool {
    let Some((&final_byte, body)) = sequence.split_last() else {
        return false;
    };
    if !matches!(final_byte, b'h' | b'l') {
        return false;
    }
    let Some(params) = body.strip_prefix(b"\x1b[?") else {
        return false;
    };
    params
        .split(|byte| *byte == b';')
        .any(|param| matches!(param, b"47" | b"1047" | b"1049"))
}

/// Matches standard CPR (`\x1b[6n`) and extended DECXCPR (`\x1b[?6n`) queries.
/// Both ask the host terminal for the current cursor position; the response
/// (`\x1b[row;colR` or `\x1b[?row;colR`) must reach the PTY child so the
/// shell can position its prompt correctly.
pub(crate) fn is_cursor_position_query(sequence: &[u8]) -> bool {
    sequence == b"\x1b[6n" || sequence == b"\x1b[?6n"
}

/// Cursor-position report (row 1, col 1) answering a CPR/DECXCPR query, matching
/// the query's form: `\x1b[6n` -> `\x1b[1;1R`, `\x1b[?6n` -> `\x1b[?1;1R`.
pub(crate) fn cursor_position_report(query: &[u8]) -> &'static [u8] {
    if query == b"\x1b[?6n" {
        b"\x1b[?1;1R"
    } else {
        b"\x1b[1;1R"
    }
}

pub(crate) fn is_text_area_size_query(sequence: &[u8]) -> bool {
    sequence == b"\x1b[18t"
}

pub(crate) fn text_area_size_report(rows: u16, cols: u16) -> Option<Vec<u8>> {
    if rows == 0 || cols == 0 {
        return None;
    }
    Some(format!("\x1b[8;{rows};{cols}t").into_bytes())
}

pub(crate) fn is_host_terminal_control_prefix(bytes: &[u8]) -> bool {
    matches!(bytes, b"\x1b" | b"\x1b[" | b"\x1b[?") || matches!(bytes, b"\x1b]" | b"\x1bP")
}

pub(crate) fn is_host_terminal_query_or_mode_sequence(sequence: &[u8]) -> bool {
    let Some((&final_byte, body)) = sequence.split_last() else {
        return false;
    };
    let Some(params) = body.strip_prefix(b"\x1b[?") else {
        let Some(params) = body.strip_prefix(b"\x1b[") else {
            return false;
        };
        return match final_byte {
            b'c' => params.is_empty() || params == b">",
            // \x1b[6n (CPR) is handled separately before this function is
            // called; it passes through to local_output so the host terminal
            // can respond with the real cursor position.
            b't' => csi_body_is_digits_semicolons(params),
            _ => false,
        };
    };
    match final_byte {
        b'h' | b'l' => params
            .split(|byte| *byte == b';')
            .any(|param| param == b"1004"),
        b'c' => true,
        // DEC private status queries (\x1b[?...n) including DECXCPR
        // (\x1b[?6n) are now handled by is_cursor_position_query()
        // before this function is called.
        b'p' => params.contains(&b'$'),
        _ => false,
    }
}

/// For a combined DEC private mode sequence containing alt-screen modes
/// (47/1047/1049) and/or the host-filtered focus mode (1004), returns a
/// sub-sequence with those params removed (e.g. `\x1b[?1049;2004l` ->
/// `\x1b[?2004l`).  Returns `None` if no other params remain.  Used to build
/// the app-facing stream for codex, where alt-screen is forced into the
/// primary screen and 1004 is stripped like everywhere else.
pub(crate) fn split_remote_filtered_dec_private_modes(sequence: &[u8]) -> Option<Vec<u8>> {
    split_dec_private_modes_excluding(sequence, |param| {
        matches!(param, b"47" | b"1047" | b"1049" | b"1004")
    })
}

/// For a combined DEC private mode sequence containing mode 1004 among other
/// params (e.g. `\x1b[?1049;1004l`), returns a sub-sequence with only the
/// non-filtered params (e.g. `\x1b[?1049l`).  Returns `None` if 1004 is the
/// only param (nothing to pass through).
pub(crate) fn split_filtered_dec_private_modes(sequence: &[u8]) -> Option<Vec<u8>> {
    split_dec_private_modes_excluding(sequence, |param| param == b"1004")
}

/// Shared helper: rebuilds a DEC private mode sequence excluding params matched
/// by the predicate.  Returns `None` if no params remain after filtering.
pub(crate) fn split_dec_private_modes_excluding(
    sequence: &[u8],
    exclude: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let Some((&final_byte, body)) = sequence.split_last() else {
        return None;
    };
    if !matches!(final_byte, b'h' | b'l') {
        return None;
    }
    let Some(params) = body.strip_prefix(b"\x1b[?") else {
        return None;
    };
    let kept: Vec<&[u8]> = params
        .split(|byte| *byte == b';')
        .filter(|param| !exclude(param))
        .collect();
    if kept.is_empty() {
        return None;
    }
    let mut result = Vec::with_capacity(sequence.len());
    result.extend_from_slice(b"\x1b[?");
    for (i, param) in kept.iter().enumerate() {
        if i > 0 {
            result.push(b';');
        }
        result.extend_from_slice(param);
    }
    result.push(final_byte);
    Some(result)
}

/// True for a DECSTBM "set top/bottom margins" CSI (`\x1b[Pt;Pbr`). Excludes the
/// DEC private `\x1b[?...r` form (XTRESTORE), which is unrelated.
pub(crate) fn is_set_scroll_region_sequence(sequence: &[u8]) -> bool {
    let Some(body) = sequence
        .strip_prefix(b"\x1b[")
        .and_then(|body| body.strip_suffix(b"r"))
    else {
        return false;
    };
    body.first() != Some(&b'?') && csi_body_is_digits_semicolons(body)
}

/// Rewrites a child DECSTBM scroll-region sequence for the host's local output.
///
/// In a shared session the PTY is sized to the (smaller) app viewport, so a
/// full-screen primary-buffer app (e.g. macOS `top`) sets a scroll region whose
/// bottom margin reaches the PTY's last row (`\x1b[1;{pty_rows}r`). On the
/// physically taller host terminal that region traps every later line in the
/// top rows while the visible prompt below it stays frozen. When the host is
/// taller than the PTY we expand such *bottom-anchored* regions to the host's
/// last row so the host stays scrollable across its whole screen.
///
/// Regions that deliberately stop short of the PTY bottom (e.g. a status-line
/// app reserving the last row) are left untouched, as is a plain reset
/// (`\x1b[r`, which the host already treats as its full screen). The app-facing
/// stream always keeps the original sequence — its grid is exactly `pty_rows`
/// tall, so the child's region is already correct there.
pub(crate) fn host_scroll_region_sequence(
    sequence: &[u8],
    pty_rows: u16,
    host_rows: u16,
) -> Vec<u8> {
    if pty_rows == 0 || host_rows <= pty_rows {
        return sequence.to_vec();
    }
    let Some(body) = sequence
        .strip_prefix(b"\x1b[")
        .and_then(|body| body.strip_suffix(b"r"))
    else {
        return sequence.to_vec();
    };
    // A bare reset already means "full screen" on the host.
    if body.is_empty() {
        return sequence.to_vec();
    }
    let mut params = body.split(|byte| *byte == b';');
    let top = params.next().unwrap_or(b"");
    let bottom = params.next();
    // More than two params is not a DECSTBM we recognise; pass it through.
    if params.next().is_some() {
        return sequence.to_vec();
    }
    let bottom_anchored = match bottom {
        // `\x1b[Ptr` / `\x1b[Pt;r`: omitted bottom defaults to the last row.
        None | Some(b"") => true,
        Some(bottom) => parse_csi_u16(bottom).is_some_and(|value| value >= pty_rows),
    };
    if !bottom_anchored {
        return sequence.to_vec();
    }
    let top_row = parse_csi_u16(top).unwrap_or(1).max(1);
    format!("\x1b[{top_row};{host_rows}r").into_bytes()
}

pub(crate) fn parse_csi_u16(bytes: &[u8]) -> Option<u16> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

pub(crate) fn local_output_string_control_action(
    bytes: &[u8],
    color_query_palette: Option<&PaletteState>,
) -> LocalInputSequenceAction {
    let action = local_input_string_control_action(bytes);
    let LocalInputSequenceAction::Drop(len) = action else {
        return action;
    };

    let sequence = &bytes[..len];
    if let Some(palette) = color_query_palette
        && is_terminal_color_query(sequence)
    {
        return LocalInputSequenceAction::PassAndMirror(
            len,
            deterministic_terminal_color_response(sequence, palette),
        );
    }
    if is_host_terminal_string_query(sequence) {
        LocalInputSequenceAction::Drop(len)
    } else {
        LocalInputSequenceAction::Pass(len)
    }
}

pub(crate) fn deterministic_terminal_color_response(
    sequence: &[u8],
    palette: &PaletteState,
) -> Vec<u8> {
    let Some(body) = sequence.strip_prefix(b"\x1b]") else {
        return Vec::new();
    };
    let body = trim_string_control_terminator(body);
    match body {
        b"10;?" => osc_color_response(b"10", &palette.default_fg, palette),
        b"11;?" => osc_color_response(b"11", &palette.default_bg, palette),
        b"12;?" => osc_color_response(b"12", &palette.cursor, palette),
        _ => deterministic_osc4_color_responses(body, palette),
    }
}

pub(crate) fn deterministic_osc4_color_responses(body: &[u8], palette: &PaletteState) -> Vec<u8> {
    let Some(rest) = body.strip_prefix(b"4;") else {
        return Vec::new();
    };
    let mut response = Vec::new();
    let mut parts = rest.split(|byte| *byte == b';');
    while let (Some(index), Some(query)) = (parts.next(), parts.next()) {
        if index.is_empty() || !index.iter().all(|byte| byte.is_ascii_digit()) || query != b"?" {
            continue;
        }
        let Some(index_value) = ascii_u16(index) else {
            continue;
        };
        let color = palette
            .ansi
            .get(usize::from(index_value))
            .cloned()
            .unwrap_or_else(|| xterm_indexed_color(index_value));
        response.extend_from_slice(b"\x1b]4;");
        response.extend_from_slice(index);
        response.extend_from_slice(b";");
        response.extend_from_slice(&osc_rgb_body(&color, palette));
        response.push(0x07);
    }
    response
}

pub(crate) fn osc_color_response(
    kind: &[u8],
    color: &TerminalColor,
    palette: &PaletteState,
) -> Vec<u8> {
    let mut response = Vec::new();
    response.extend_from_slice(b"\x1b]");
    response.extend_from_slice(kind);
    response.extend_from_slice(b";");
    response.extend_from_slice(&osc_rgb_body(color, palette));
    response.push(0x07);
    response
}

pub(crate) fn osc_rgb_body(color: &TerminalColor, palette: &PaletteState) -> Vec<u8> {
    let (r, g, b) = resolved_terminal_rgb(color, palette);
    format!("rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}").into_bytes()
}

pub(crate) fn resolved_terminal_rgb(color: &TerminalColor, palette: &PaletteState) -> (u8, u8, u8) {
    resolved_terminal_rgb_with_depth(color, palette, 0)
}

pub(crate) fn resolved_terminal_rgb_with_depth(
    color: &TerminalColor,
    palette: &PaletteState,
    depth: usize,
) -> (u8, u8, u8) {
    if depth > 8 {
        return (238, 238, 238);
    }
    match color {
        TerminalColor::Rgb { r, g, b } => (*r, *g, *b),
        TerminalColor::Indexed(index) => {
            if let Some(palette_color) = palette.ansi.get(usize::from(*index))
                && palette_color != color
            {
                return resolved_terminal_rgb_with_depth(palette_color, palette, depth + 1);
            }
            match xterm_indexed_color(*index) {
                TerminalColor::Rgb { r, g, b } => (r, g, b),
                _ => (238, 238, 238),
            }
        }
        TerminalColor::Default => {
            if palette.default_fg != TerminalColor::Default {
                resolved_terminal_rgb_with_depth(&palette.default_fg, palette, depth + 1)
            } else {
                (238, 238, 238)
            }
        }
    }
}

pub(crate) fn ascii_u16(bytes: &[u8]) -> Option<u16> {
    if bytes.is_empty() || !bytes.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}
