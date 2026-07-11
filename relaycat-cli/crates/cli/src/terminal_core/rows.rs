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

pub(crate) fn row_from_vt_scrollback(
    row: &vt100::ScrollbackRow,
    line_id: u64,
    attrs: &mut Vec<CellAttr>,
    attr_index: &mut HashMap<CellAttr, u32>,
) -> TerminalRow {
    let mut cells = Vec::with_capacity(usize::from(row.cols()));
    for col in 0..row.cols() {
        let Some(cell) = row.cell(col) else {
            cells.push((0, blank_cell()));
            continue;
        };
        let attr_id = attr_id_for(attrs, attr_index, cell_attr_from_vt_cell(cell));
        cells.push((attr_id, terminal_cell_from_vt_cell(cell)));
    }

    TerminalRow {
        line_id,
        wrapped: row.wrapped(),
        cells: compress_cells(cells),
    }
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
