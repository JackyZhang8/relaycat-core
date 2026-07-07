use super::*;

pub(crate) fn blank_rows(rows: u16, cols: u16, line_id_counter: &mut u64) -> Vec<TerminalRow> {
    (0..rows)
        .map(|_| blank_row(cols, next_line_id(line_id_counter)))
        .collect()
}

pub(crate) fn next_line_id(next_line_id: &mut u64) -> u64 {
    let id = *next_line_id;
    *next_line_id = (*next_line_id).saturating_add(1);
    id
}

pub(crate) fn blank_row(cols: u16, line_id: u64) -> TerminalRow {
    let cells = compress_cells((0..cols).map(|_| (0, blank_cell())).collect());
    TerminalRow {
        line_id,
        wrapped: false,
        cells,
    }
}

pub(crate) fn blank_cell() -> TerminalCell {
    TerminalCell {
        text: " ".to_string(),
        width: 1,
    }
}

pub(crate) fn expand_row(row: &TerminalRow, cols: usize) -> Vec<(u32, TerminalCell)> {
    let mut cells = Vec::with_capacity(cols);
    for run in &row.cells {
        for cell in &run.cells {
            if cells.len() >= cols {
                return cells;
            }
            cells.push((run.attr_id, cell.clone()));
        }
    }
    cells.resize_with(cols, || (0, blank_cell()));
    cells
}

pub(crate) fn compress_cells(cells: Vec<(u32, TerminalCell)>) -> Vec<CellRun> {
    let mut cells = cells;
    while cells
        .last()
        .is_some_and(|(attr_id, cell)| *attr_id == 0 && is_blank_default_cell(cell))
    {
        cells.pop();
    }

    let mut runs: Vec<CellRun> = Vec::new();
    for (attr_id, cell) in cells {
        if let Some(run) = runs.last_mut()
            && run.attr_id == attr_id
        {
            run.cells.push(cell);
            continue;
        }
        runs.push(CellRun {
            attr_id,
            cells: vec![cell],
        });
    }
    runs
}

/// Re-encode a history row so it has exactly `cols` cells, matching the current
/// terminal width. Rows wider than `cols` are truncated; narrower rows are
/// padded with blank default cells (which `compress_cells` trims away).
pub(crate) fn normalize_row_to_cols(row: &TerminalRow, cols: usize) -> TerminalRow {
    let cells = expand_row(row, cols);
    TerminalRow {
        line_id: row.line_id,
        wrapped: row.wrapped,
        cells: compress_cells(cells),
    }
}

pub(crate) fn is_blank_default_cell(cell: &TerminalCell) -> bool {
    cell.text == " " && cell.width == 1
}

pub(crate) fn rows_render_equal(left: &TerminalRow, right: &TerminalRow) -> bool {
    left.wrapped == right.wrapped && left.cells == right.cells
}

pub(crate) fn transcript_rows_are_blank(rows: &[TerminalRow]) -> bool {
    rows.iter().all(|row| {
        row.cells
            .iter()
            .flat_map(|run| run.cells.iter())
            .all(is_blank_default_cell)
    })
}

pub(crate) fn changed_rows(
    previous_rows: &[TerminalRow],
    current_rows: &[TerminalRow],
) -> Vec<(usize, TerminalRow)> {
    current_rows
        .iter()
        .enumerate()
        .filter_map(|(index, current)| match previous_rows.get(index) {
            Some(previous) if rows_render_equal(previous, current) => None,
            _ => Some((index, current.clone())),
        })
        .collect()
}

/// Probe the actual cell width of the currently visible row 0. vt100 stores
/// each scrollback row at its original width; `Row::get(col)` returns `None`
/// for columns beyond that width. Starting from `min_cols`, we probe
/// successive columns until `cell()` returns `None`.
pub(crate) fn scrollback_row_actual_cols(screen: &vt100::Screen, min_cols: u16) -> u16 {
    let mut cols = min_cols;
    while cols < u16::MAX && screen.cell(0, cols).is_some() {
        cols = cols.saturating_add(1);
    }
    cols
}

/// Like [`read_recent_scrollback`] but reads each row at the wider of
/// `min_cols` and the row's own stored width. This prevents truncation when
/// the terminal was recently narrowed but the scrollback still holds wider
/// rows from before the resize.
pub(crate) fn read_recent_scrollback_faithful(
    screen: &mut vt100::Screen,
    count: usize,
    min_cols: u16,
    line_id_counter: &mut u64,
    attrs: &mut Vec<CellAttr>,
    attr_index: &mut HashMap<CellAttr, u32>,
) -> Vec<TerminalRow> {
    let take = count.min(screen.scrollback());
    let mut rows = Vec::with_capacity(take);
    for offset in (1..=take).rev() {
        screen.set_scrollback(offset);
        let row_cols = scrollback_row_actual_cols(screen, min_cols);
        rows.push(row_from_vt_screen(
            screen,
            0,
            row_cols,
            next_line_id(line_id_counter),
            attrs,
            attr_index,
        ));
    }
    rows
}

/// Read the most-recent `count` rows from the vt100 parser's own scrollback,
/// oldest first, assigning each a fresh `line_id`. `screen` must already have
/// its scrollback offset set to the maximum so `scrollback()` reports the total
/// length. The newest scrolled-off row is at offset 1, the next at offset 2,
/// and so on, so the requested tail is offsets `count..=1`.
pub(crate) fn read_recent_scrollback(
    screen: &mut vt100::Screen,
    count: usize,
    cols: u16,
    line_id_counter: &mut u64,
    attrs: &mut Vec<CellAttr>,
    attr_index: &mut HashMap<CellAttr, u32>,
) -> Vec<TerminalRow> {
    let take = count.min(screen.scrollback());
    let mut rows = Vec::with_capacity(take);
    for offset in (1..=take).rev() {
        screen.set_scrollback(offset);
        rows.push(row_from_vt_screen(
            screen,
            0,
            cols,
            next_line_id(line_id_counter),
            attrs,
            attr_index,
        ));
    }
    rows
}

pub(crate) fn row_from_vt_screen(
    screen: &vt100::Screen,
    row: u16,
    cols: u16,
    line_id: u64,
    attrs: &mut Vec<CellAttr>,
    attr_index: &mut HashMap<CellAttr, u32>,
) -> TerminalRow {
    let mut cells = Vec::with_capacity(usize::from(cols));
    for col in 0..cols {
        let Some(cell) = screen.cell(row, col) else {
            cells.push((0, blank_cell()));
            continue;
        };
        let attr_id = attr_id_for(attrs, attr_index, cell_attr_from_vt_cell(cell));
        cells.push((attr_id, terminal_cell_from_vt_cell(cell)));
    }

    TerminalRow {
        line_id,
        wrapped: screen.row_wrapped(row),
        cells: compress_cells(cells),
    }
}
