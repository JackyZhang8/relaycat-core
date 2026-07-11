use super::*;

pub(crate) fn cursor_from_vt_screen(screen: &vt100::Screen) -> CursorState {
    let (row, col) = screen.cursor_position();
    CursorState {
        row,
        col,
        visible: !screen.hide_cursor(),
        style: CursorStyle::Block,
    }
}

pub(crate) fn modes_from_vt_screen(screen: &vt100::Screen) -> TerminalModes {
    TerminalModes {
        alt_screen: screen.alternate_screen(),
        bracketed_paste: screen.bracketed_paste(),
        application_cursor: screen.application_cursor(),
    }
}

pub(crate) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}
