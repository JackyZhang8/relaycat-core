/// Longest partial escape sequence carried across output chunk boundaries.
/// DECSET sequences are short (`\x1b[?1000;1002;1003;1006h` is 22 bytes), so
/// anything longer than this cannot be a mode change we care about.
const MAX_PENDING_SEQUENCE: usize = 64;

/// Tracks the mouse-reporting modes the child TUI has enabled via DECSET /
/// DECRST (9/1000/1002/1003 for tracking, 1005/1006/1015/1016 for the report
/// encoding). The app forwards touch scrolls as SGR wheel reports; the child
/// only understands them if it enabled SGR (1006), so wheel reports from the
/// app are re-encoded into whatever format the child actually negotiated.
#[derive(Debug, Default)]
pub(crate) struct MouseReportModes {
    pending: Vec<u8>,
    tracking: bool,
    sgr: bool,
    urxvt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WheelEncoding {
    /// Mouse tracking is off: the child would render wheel bytes as garbage
    /// input, so reports are dropped.
    Disabled,
    /// SGR 1006 (`\x1b[<b;col;rowM`), the format the app already sends.
    Sgr,
    /// urxvt 1015 (`\x1b[b+32;col;rowM`).
    Urxvt,
    /// Legacy X10 (`\x1b[M` + three `32 + value` bytes).
    X10,
}

impl MouseReportModes {
    pub(crate) fn wheel_encoding(&self) -> WheelEncoding {
        if !self.tracking {
            WheelEncoding::Disabled
        } else if self.sgr {
            WheelEncoding::Sgr
        } else if self.urxvt {
            WheelEncoding::Urxvt
        } else {
            WheelEncoding::X10
        }
    }

    /// Scans raw PTY output for DEC private mode set/reset sequences and
    /// updates the tracked mouse modes. Partial trailing sequences are carried
    /// over to the next call so chunk boundaries do not lose mode changes.
    pub(crate) fn observe_output(&mut self, bytes: &[u8]) {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);
        let mut index = 0;
        while index < input.len() {
            let remaining = &input[index..];
            if remaining[0] != 0x1b {
                index += 1;
                continue;
            }
            if !remaining.starts_with(b"\x1b[?") {
                if remaining.len() < 3 {
                    break;
                }
                index += 1;
                continue;
            }
            let Some(final_offset) = remaining[3..]
                .iter()
                .position(|byte| (0x40..=0x7e).contains(byte))
            else {
                break;
            };
            let final_byte = remaining[3 + final_offset];
            let params = &remaining[3..3 + final_offset];
            if matches!(final_byte, b'h' | b'l') {
                self.apply_mode_change(params, final_byte == b'h');
            }
            index += 3 + final_offset + 1;
        }
        if index < input.len() && input.len() - index <= MAX_PENDING_SEQUENCE {
            self.pending = input[index..].to_vec();
        }
    }

    fn apply_mode_change(&mut self, params: &[u8], enabled: bool) {
        for param in params.split(|byte| *byte == b';') {
            match param {
                b"9" | b"1000" | b"1002" | b"1003" => self.tracking = enabled,
                b"1006" | b"1016" => self.sgr = enabled,
                b"1015" => self.urxvt = enabled,
                _ => {}
            }
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WheelRewriteResult {
    pub(crate) bytes: Vec<u8>,
    pub(crate) reports: usize,
}

/// Re-encodes SGR wheel reports (`\x1b[<64;col;rowM` / `\x1b[<65;col;rowM`)
/// embedded in app input into the encoding the child negotiated. All other
/// bytes pass through untouched.
pub(crate) fn rewrite_app_wheel_reports(
    bytes: &[u8],
    encoding: WheelEncoding,
) -> WheelRewriteResult {
    let mut result = WheelRewriteResult {
        bytes: Vec::with_capacity(bytes.len()),
        reports: 0,
    };
    let mut index = 0;
    while index < bytes.len() {
        match parse_sgr_wheel_report(&bytes[index..]) {
            Some((length, button, col, row)) => {
                result.reports += 1;
                encode_wheel_report(&mut result.bytes, encoding, button, col, row);
                index += length;
            }
            None => {
                result.bytes.push(bytes[index]);
                index += 1;
            }
        }
    }
    result
}

/// Parses a leading SGR wheel report, returning `(length, button, col, row)`.
fn parse_sgr_wheel_report(bytes: &[u8]) -> Option<(usize, u16, u16, u16)> {
    let rest = bytes.strip_prefix(b"\x1b[<")?;
    let end = rest.iter().position(|byte| *byte == b'M')?;
    if end > 16 {
        return None;
    }
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let mut parts = body.split(';');
    let button: u16 = parts.next()?.parse().ok()?;
    let col: u16 = parts.next()?.parse().ok()?;
    let row: u16 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !matches!(button, 64 | 65) {
        return None;
    }
    Some((3 + end + 1, button, col, row))
}

fn encode_wheel_report(
    out: &mut Vec<u8>,
    encoding: WheelEncoding,
    button: u16,
    col: u16,
    row: u16,
) {
    let col = col.max(1);
    let row = row.max(1);
    match encoding {
        WheelEncoding::Disabled => {}
        WheelEncoding::Sgr => {
            out.extend_from_slice(format!("\x1b[<{button};{col};{row}M").as_bytes());
        }
        WheelEncoding::Urxvt => {
            out.extend_from_slice(format!("\x1b[{};{col};{row}M", button + 32).as_bytes());
        }
        WheelEncoding::X10 => {
            // X10 coordinates are single bytes offset by 32, capped at 223.
            out.extend_from_slice(b"\x1b[M");
            out.push(32 + button as u8);
            out.push(32 + col.min(223) as u8);
            out.push(32 + row.min(223) as u8);
        }
    }
}
