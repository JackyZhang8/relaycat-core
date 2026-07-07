use super::*;

impl TerminalCore {
    pub(crate) fn trim_history(&mut self) {
        while self.history.len() > TERMINAL_HISTORY_MAX_ROWS {
            self.history.pop_front();
        }
    }

    pub(crate) fn put_cells(&mut self, row: u16, col: u16, runs: &[CellRun]) {
        let cols = usize::from(self.cols);
        let Some(line) = self.screen_rows.get_mut(usize::from(row)) else {
            return;
        };

        let mut cells = expand_row(line, cols);
        let mut cursor = usize::from(col).min(cols);
        for run in runs {
            for cell in &run.cells {
                if cursor >= cols {
                    return;
                }
                cells[cursor] = (run.attr_id, cell.clone());
                cursor += usize::from(cell.width.max(1));
            }
        }
        line.cells = compress_cells(cells);
    }

    pub(crate) fn clear_range(&mut self, row: u16, col_start: u16, col_end: u16, attr_id: u32) {
        let cols = usize::from(self.cols);
        let Some(line) = self.screen_rows.get_mut(usize::from(row)) else {
            return;
        };

        let start = usize::from(col_start).min(cols);
        let end = usize::from(col_end).min(cols);
        if end <= start {
            return;
        }

        let mut cells = expand_row(line, cols);
        for cell in cells.iter_mut().take(end).skip(start) {
            *cell = (attr_id, blank_cell());
        }
        line.cells = compress_cells(cells);
    }

    pub(crate) fn replace_row(&mut self, row: u16, mut line: TerminalRow) {
        let cols = usize::from(self.cols);
        let Some(current) = self.screen_rows.get_mut(usize::from(row)) else {
            return;
        };

        let cells = expand_row(&line, cols);
        line.cells = compress_cells(cells);
        *current = line;
    }

    pub(crate) fn scroll_region(&mut self, top: u16, bottom: u16, delta: i16) {
        let rows = self.screen_rows.len();
        let top = usize::from(top).min(rows);
        let bottom = usize::from(bottom).min(rows);
        if top >= bottom || delta == 0 {
            return;
        }

        let distance = usize::from(delta.unsigned_abs()).min(bottom - top);
        if delta > 0 {
            for index in (top + distance..bottom).rev() {
                self.screen_rows[index] = self.screen_rows[index - distance].clone();
            }
            for index in top..top + distance {
                self.screen_rows[index] =
                    blank_row(self.cols, next_line_id(&mut self.next_line_id));
            }
        } else {
            for index in top..bottom - distance {
                self.screen_rows[index] = self.screen_rows[index + distance].clone();
            }
            for index in bottom - distance..bottom {
                self.screen_rows[index] =
                    blank_row(self.cols, next_line_id(&mut self.next_line_id));
            }
        }
    }
}
