use super::*;

pub(crate) fn patches_fit_relay_budget(patches: &[TerminalPatchV2]) -> bool {
    patches
        .iter()
        .try_fold(0usize, |total, patch| {
            let len = terminal_patch_v2_encoded_len(patch);
            let next = total.checked_add(len)?;
            (next <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES).then_some(next)
        })
        .is_some()
}

pub(crate) fn patches_fit_resume_scrollback_window(patches: &[TerminalPatchV2], rows: u16) -> bool {
    let max_scrollback_rows = usize::from(rows)
        .max(1)
        .saturating_mul(TERMINAL_RESUME_PATCH_REPLAY_MAX_SCREENS);
    patches
        .iter()
        .flat_map(|patch| patch.ops.iter())
        .map(|op| match op {
            PatchOp::AppendScrollback { rows } => rows.len(),
            _ => 0,
        })
        .try_fold(0usize, |total, rows| {
            let next = total.checked_add(rows)?;
            (next <= max_scrollback_rows).then_some(next)
        })
        .is_some()
}

pub(crate) fn coalesce_resume_patches(patches: Vec<TerminalPatchV2>) -> Vec<TerminalPatchV2> {
    let mut merged = Vec::new();
    let mut current: Option<TerminalPatchV2> = None;

    for patch in patches {
        let Some(active) = current.take() else {
            current = Some(patch);
            continue;
        };

        let Some(candidate) = merged_resume_patch(&active, &patch) else {
            merged.push(active);
            current = Some(patch);
            continue;
        };

        if terminal_patch_v2_encoded_len(&candidate) <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES {
            current = Some(candidate);
        } else {
            merged.push(active);
            current = Some(patch);
        }
    }

    if let Some(active) = current {
        merged.push(active);
    }

    merged
}

pub(crate) fn merged_resume_patch(left: &TerminalPatchV2, right: &TerminalPatchV2) -> Option<TerminalPatchV2> {
    if left.terminal_run_id != right.terminal_run_id
        || left.base_snapshot_id != right.base_snapshot_id
        || right.from_state_seq != left.to_state_seq.saturating_add(1)
    {
        return None;
    }

    // Incremental-attr patches carry only a tail keyed by a base index, so they
    // cannot be collapsed by picking one side's table; leave them unmerged (they
    // still replay correctly in order).
    if left.attrs_base_len.is_some() || right.attrs_base_len.is_some() {
        return None;
    }

    let mut ops = left.ops.clone();
    ops.extend(right.ops.clone());
    Some(TerminalPatchV2 {
        terminal_run_id: left.terminal_run_id.clone(),
        base_snapshot_id: left.base_snapshot_id,
        from_state_seq: left.from_state_seq,
        to_state_seq: right.to_state_seq,
        attrs: if right.attrs.is_empty() {
            left.attrs.clone()
        } else {
            right.attrs.clone()
        },
        attrs_base_len: None,
        ops,
    })
}
