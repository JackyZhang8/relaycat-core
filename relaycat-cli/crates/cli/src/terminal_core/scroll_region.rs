use super::*;

pub(crate) fn tracked_scroll_region_from_csi_r(sequence: &[u8], rows: u16) -> Option<TrackedScrollRegion> {
    let body = sequence.strip_prefix(b"\x1b[")?.strip_suffix(b"r")?;
    if body.is_empty() {
        return None;
    }
    let mut parts = body.split(|byte| *byte == b';');
    let top = parse_csi_u16_param(parts.next(), 1)?;
    let bottom = parse_csi_u16_param(parts.next(), rows)?;
    if parts.next().is_some() {
        return None;
    }

    let top = top.max(1).saturating_sub(1);
    let bottom = bottom.min(rows);
    if top == 0 && bottom > top && bottom < rows {
        Some(TrackedScrollRegion { top, bottom })
    } else {
        None
    }
}

pub(crate) fn parse_csi_u16_param(param: Option<&[u8]>, default: u16) -> Option<u16> {
    let Some(param) = param else {
        return Some(default);
    };
    if param.is_empty() {
        return Some(default);
    }
    if !param.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(param).ok()?.parse().ok()
}

pub(crate) fn top_anchored_scroll_region_shifted_out_rows(
    previous: &PreviousTerminalState,
    current: &[TerminalRow],
    region: TrackedScrollRegion,
) -> Vec<TerminalRow> {
    let bottom = usize::from(region.bottom)
        .min(previous.rows.len())
        .min(current.len());
    if bottom <= 1 {
        return Vec::new();
    }

    for shift in (1..bottom).rev() {
        let overlap = bottom - shift;
        if (0..overlap)
            .all(|index| rows_render_equal(&previous.rows[index + shift], &current[index]))
        {
            let shifted_out = previous.rows[..shift].to_vec();
            return if transcript_rows_are_blank(&shifted_out) {
                Vec::new()
            } else {
                shifted_out
            };
        }
    }

    Vec::new()
}

