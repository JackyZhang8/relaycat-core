use super::*;

/// Preserve the original single-patch convenience API for callers that do not
/// transport its result. A parser drain emits new attributes only on its first
/// fragment, so the batch can be represented as one semantic patch range.
pub(crate) fn merge_patch_batch(patches: Vec<TerminalPatchV2>) -> Option<TerminalPatchV2> {
    let mut patches = patches.into_iter();
    let mut merged = patches.next()?;
    for patch in patches {
        debug_assert_eq!(patch.terminal_run_id, merged.terminal_run_id);
        debug_assert_eq!(patch.base_snapshot_id, merged.base_snapshot_id);
        debug_assert_eq!(patch.from_state_seq, merged.to_state_seq.saturating_add(1));
        debug_assert!(patch.attrs.is_empty());
        debug_assert!(patch.attrs_base_len.is_none());
        merged.to_state_seq = patch.to_state_seq;
        merged.ops.extend(patch.ops);
    }
    Some(merged)
}

pub(crate) fn patches_fit_relay_budget(patches: &[TerminalPatchV2]) -> bool {
    patches
        .iter()
        .all(|patch| terminal_patch_v2_encoded_len(patch) <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES)
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

pub(crate) fn merged_resume_patch(
    left: &TerminalPatchV2,
    right: &TerminalPatchV2,
) -> Option<TerminalPatchV2> {
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
