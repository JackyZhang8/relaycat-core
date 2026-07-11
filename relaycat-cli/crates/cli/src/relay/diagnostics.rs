use super::*;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalPatchOpCounts {
    append_scrollback_rows: usize,
    replace_rows: usize,
    scroll_region_ops: usize,
    switch_alt_screen: Option<bool>,
}

pub(crate) fn terminal_patch_op_counts(
    patch: &relaycat_protocol::TerminalPatchV2,
) -> TerminalPatchOpCounts {
    let mut counts = TerminalPatchOpCounts::default();
    for op in &patch.ops {
        match op {
            PatchOp::AppendScrollback { rows } => counts.append_scrollback_rows += rows.len(),
            PatchOp::ReplaceRow { .. } => counts.replace_rows += 1,
            PatchOp::ScrollRegion { .. } => counts.scroll_region_ops += 1,
            PatchOp::SwitchAltScreen(enabled) => counts.switch_alt_screen = Some(*enabled),
            _ => {}
        }
    }
    counts
}

pub(crate) fn terminal_patch_diagnostic_line(
    session_kind: &str,
    bytes: &[u8],
    terminal_core: &TerminalCore,
    patch: &relaycat_protocol::TerminalPatchV2,
) -> String {
    let counts = terminal_patch_op_counts(patch);
    let controls = terminal_output_control_diagnostics(bytes);
    let core = terminal_core.debug_snapshot();
    let patch_plain_bytes = terminal_patch_plain_bytes(patch);
    format!(
        "terminal_patch_diag kind={session_kind} bytes={} patch_plain_bytes={} seq={} ops={} append_rows={} replace_rows={} scroll_region_ops={} switch_alt={:?} attrs={} core_history={} vt_scrollback={} frozen={} alt={} size={}x{} raw_csi_r={} raw_csi_scroll={} raw_alt={} raw_preview={}",
        bytes.len(),
        patch_plain_bytes,
        patch.to_state_seq,
        patch.ops.len(),
        counts.append_scrollback_rows,
        counts.replace_rows,
        counts.scroll_region_ops,
        counts.switch_alt_screen,
        patch.attrs.len(),
        core.history_len,
        core.vt_scrollback_len,
        core.history_frozen,
        core.alt_screen,
        core.cols,
        core.rows,
        controls.csi_scroll_region,
        controls.csi_scroll_up_or_down,
        controls.alternate_screen,
        format_byte_preview(bytes),
    )
}

pub(crate) fn terminal_patch_plain_bytes(patch: &relaycat_protocol::TerminalPatchV2) -> usize {
    encode_plain_msg(&PlainMsg::TerminalPatchV2(patch.clone()))
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

pub(crate) fn terminal_input_diagnostic_line(
    session_kind: &str,
    input_stream_id: &str,
    input_seq: u64,
    bytes: &[u8],
    decision: InputDecision,
) -> String {
    format!(
        "terminal_input_diag kind={session_kind} stream={input_stream_id} input_seq={input_seq} decision={decision:?} bytes={} control={} raw_preview={}",
        bytes.len(),
        terminal_input_control_name(bytes),
        format_byte_preview(bytes),
    )
}

pub(crate) fn terminal_input_control_name(bytes: &[u8]) -> &'static str {
    match bytes {
        b"\x1b[5~" => "page_up",
        b"\x1b[6~" => "page_down",
        b"\x1b[A" => "arrow_up",
        b"\x1b[B" => "arrow_down",
        b"\x1b[C" => "arrow_right",
        b"\x1b[D" => "arrow_left",
        _ => "other",
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalOutputControlDiagnostics {
    pub(crate) csi_scroll_region: bool,
    pub(crate) csi_scroll_up_or_down: bool,
    pub(crate) alternate_screen: bool,
}

pub(crate) fn terminal_output_control_diagnostics(
    bytes: &[u8],
) -> TerminalOutputControlDiagnostics {
    let mut diagnostics = TerminalOutputControlDiagnostics::default();
    let mut index = 0;
    while index < bytes.len() {
        let remaining = &bytes[index..];
        if !remaining.starts_with(b"\x1b[") {
            index += 1;
            continue;
        }
        let Some(body_final_offset) = remaining[2..]
            .iter()
            .position(|byte| (0x40..=0x7e).contains(byte))
        else {
            break;
        };
        let final_offset = body_final_offset + 2;
        let sequence = &remaining[..=final_offset];
        match sequence.last().copied() {
            Some(b'r') => diagnostics.csi_scroll_region = true,
            Some(b'S' | b'T') => diagnostics.csi_scroll_up_or_down = true,
            Some(b'h' | b'l') if is_alternate_screen_mode_sequence(sequence) => {
                diagnostics.alternate_screen = true;
            }
            _ => {}
        }
        index += final_offset + 1;
    }
    diagnostics
}
