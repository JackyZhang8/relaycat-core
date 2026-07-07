use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalInterruptAction {
    Forward,
    Exit,
}

pub(crate) fn local_interrupt_action(
    last_interrupt_at: Option<Instant>,
    now: Instant,
) -> LocalInterruptAction {
    match last_interrupt_at {
        Some(last) if now.duration_since(last) <= LOCAL_INTERRUPT_EXIT_WINDOW => {
            LocalInterruptAction::Exit
        }
        _ => LocalInterruptAction::Forward,
    }
}

pub(crate) fn local_interrupt_action_for_session(
    session_kind: SessionKind,
    last_interrupt_at: Option<Instant>,
    now: Instant,
) -> LocalInterruptAction {
    if session_kind.as_str() == "claude" {
        return LocalInterruptAction::Exit;
    }
    local_interrupt_action(last_interrupt_at, now)
}

#[derive(Debug, Default)]
pub(crate) struct LocalInputFilter {
    pub(crate) pending: Vec<u8>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LocalInputFilterResult {
    pub(crate) pty_input: Vec<u8>,
    pub(crate) mirrored_output: Vec<u8>,
    pub(crate) toggle_work_mode_count: usize,
}

impl LocalInputFilter {
    pub(crate) fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    pub(crate) fn filter(&mut self, bytes: &[u8]) -> LocalInputFilterResult {
        let mut input = std::mem::take(&mut self.pending);
        input.extend_from_slice(bytes);

        let mut result = LocalInputFilterResult {
            pty_input: Vec::with_capacity(input.len()),
            mirrored_output: Vec::new(),
            toggle_work_mode_count: 0,
        };
        let mut index = 0;
        while index < input.len() {
            let remaining = &input[index..];
            match self.local_input_sequence_action(remaining) {
                LocalInputSequenceAction::Drop(len) => {
                    index += len;
                }
                LocalInputSequenceAction::ToggleWorkMode(len) => {
                    result.toggle_work_mode_count += 1;
                    index += len;
                }
                LocalInputSequenceAction::Pass(len) => {
                    result.pty_input.extend_from_slice(&remaining[..len]);
                    index += len;
                }
                LocalInputSequenceAction::PassAndMirror(len, after_mirror) => {
                    result.pty_input.extend_from_slice(&remaining[..len]);
                    result.mirrored_output.extend_from_slice(&remaining[..len]);
                    result.mirrored_output.extend_from_slice(&after_mirror);
                    index += len;
                }
                LocalInputSequenceAction::Pending => {
                    self.pending.extend_from_slice(remaining);
                    break;
                }
            }
        }

        result
    }

    pub(crate) fn local_input_sequence_action(&mut self, bytes: &[u8]) -> LocalInputSequenceAction {
        if is_terminal_string_control_start(bytes) {
            let action = local_input_string_control_action(bytes);
            let LocalInputSequenceAction::Drop(len) = action else {
                return action;
            };
            return LocalInputSequenceAction::Drop(len);
        }

        local_input_sequence_action(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalInputSequenceAction {
    Drop(usize),
    ToggleWorkMode(usize),
    Pass(usize),
    PassAndMirror(usize, Vec<u8>),
    Pending,
}

pub(crate) fn local_input_sequence_action(bytes: &[u8]) -> LocalInputSequenceAction {
    if bytes.is_empty() {
        return LocalInputSequenceAction::Pending;
    }
    if bytes[0] == LOCAL_WORK_MODE_TOGGLE_KEY {
        return LocalInputSequenceAction::ToggleWorkMode(1);
    }
    if bytes[0] != 0x1b {
        return LocalInputSequenceAction::Pass(1);
    }
    if bytes.len() == 1 {
        return LocalInputSequenceAction::Pending;
    }

    match bytes[1] {
        b'[' => local_input_csi_action(bytes),
        b']' | b'P' | b'^' | b'_' => local_input_string_control_action(bytes),
        _ => LocalInputSequenceAction::Pass(1),
    }
}

pub(crate) fn is_terminal_string_control_start(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1b && matches!(bytes[1], b']' | b'P' | b'^' | b'_')
}

pub(crate) fn local_input_csi_action(bytes: &[u8]) -> LocalInputSequenceAction {
    debug_assert!(bytes.starts_with(b"\x1b["));
    let Some(final_offset) = bytes[2..]
        .iter()
        .position(|byte| (0x40..=0x7e).contains(byte))
        .map(|offset| offset + 2)
    else {
        return LocalInputSequenceAction::Pending;
    };
    let sequence = &bytes[..=final_offset];
    if is_local_terminal_csi_response(sequence) {
        LocalInputSequenceAction::Drop(final_offset + 1)
    } else {
        LocalInputSequenceAction::Pass(final_offset + 1)
    }
}

pub(crate) fn local_input_string_control_action(bytes: &[u8]) -> LocalInputSequenceAction {
    debug_assert!(bytes.len() >= 2);
    let mut index = 2;
    while index < bytes.len() {
        if bytes[index] == 0x07 {
            return LocalInputSequenceAction::Drop(index + 1);
        }
        if bytes[index] == 0x1b {
            if index + 1 >= bytes.len() {
                return LocalInputSequenceAction::Pending;
            }
            if bytes[index + 1] == b'\\' {
                return LocalInputSequenceAction::Drop(index + 2);
            }
        }
        index += 1;
    }
    LocalInputSequenceAction::Pending
}

pub(crate) fn is_local_terminal_csi_response(sequence: &[u8]) -> bool {
    if sequence == b"\x1b[I" || sequence == b"\x1b[O" {
        return true;
    }

    let Some((&final_byte, body)) = sequence.split_last() else {
        return false;
    };
    let Some(params_and_intermediates) = body.strip_prefix(b"\x1b[") else {
        return false;
    };
    if params_and_intermediates.is_empty() {
        return false;
    }

    match final_byte {
        // CPR responses (\x1b[<row>;<col>R) and DSR responses (\x1b[0n)
        // are now passed through to the PTY so the shell receives real
        // terminal status from the host terminal.
        b'c' => csi_body_starts_with_any(params_and_intermediates, b"?>"),
        b't' => csi_body_is_digits_semicolons(params_and_intermediates),
        b'x' => csi_body_is_report_params(params_and_intermediates),
        b'y' => csi_body_starts_with_any(params_and_intermediates, b"?"),
        _ => false,
    }
}

pub(crate) fn csi_body_is_digits_semicolons(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|byte| byte.is_ascii_digit() || *byte == b';')
}

pub(crate) fn csi_body_is_report_params(bytes: &[u8]) -> bool {
    bytes.iter().all(|byte| {
        byte.is_ascii_digit()
            || matches!(
                *byte,
                b';' | b'?' | b'>' | b'!' | b'"' | b'$' | b' ' | b'\''
            )
    })
}

pub(crate) fn csi_body_starts_with_any(bytes: &[u8], prefixes: &[u8]) -> bool {
    prefixes.iter().any(|prefix| bytes.first() == Some(prefix))
}

