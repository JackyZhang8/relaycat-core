use super::*;

impl TerminalCore {
    pub(crate) fn emit_patch(&mut self, ops: Vec<PatchOp>) -> TerminalPatchV2 {
        let next_seq = self.state_seq.saturating_add(1);
        self.state_seq = next_seq;

        // The attribute table is append-only, so only re-send it when it has
        // grown since the last emission; an empty table means "reuse". When the
        // peer negotiated incremental attrs, send just the appended tail keyed
        // by its base index instead of the whole (only-growing) table.
        let (attrs, attrs_base_len) = self.attrs_for_next_patch();
        if self.attrs.len() > self.emitted_attrs_len {
            self.emitted_attrs_len = self.attrs.len();
        }

        let patch = TerminalPatchV2 {
            terminal_run_id: self.terminal_run_id.clone(),
            base_snapshot_id: self.active_snapshot_id,
            from_state_seq: next_seq,
            to_state_seq: next_seq,
            attrs,
            attrs_base_len,
            ops,
        };
        self.retain_patch(patch.clone());
        patch
    }

    fn attrs_for_next_patch(&self) -> (Vec<CellAttr>, Option<u32>) {
        if self.attrs.len() > self.emitted_attrs_len {
            let base = self.emitted_attrs_len;
            if self.incremental_attrs_enabled() {
                (
                    self.attrs[base..].to_vec(),
                    Some(u32::try_from(base).expect("attr table base index overflow")),
                )
            } else {
                (self.attrs.clone(), None)
            }
        } else {
            (Vec::new(), None)
        }
    }

    fn preview_patch(&self, ops: Vec<PatchOp>) -> TerminalPatchV2 {
        let next_seq = self.state_seq.saturating_add(1);
        let (attrs, attrs_base_len) = self.attrs_for_next_patch();
        TerminalPatchV2 {
            terminal_run_id: self.terminal_run_id.clone(),
            base_snapshot_id: self.active_snapshot_id,
            from_state_seq: next_seq,
            to_state_seq: next_seq,
            attrs,
            attrs_base_len,
            ops,
        }
    }

    fn patch_ops_fit_budget(&self, ops: Vec<PatchOp>) -> bool {
        terminal_patch_v2_encoded_len(&self.preview_patch(ops))
            <= TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES
    }

    fn max_scrollback_prefix_that_fits(&self, rows: &[TerminalRow]) -> usize {
        let mut low = 1usize;
        let mut high = rows.len();
        let mut best = 0usize;
        while low <= high {
            let middle = low + (high - low) / 2;
            let ops = vec![PatchOp::AppendScrollback {
                rows: rows[..middle].to_vec(),
            }];
            if self.patch_ops_fit_budget(ops) {
                best = middle;
                low = middle.saturating_add(1);
            } else {
                high = middle.saturating_sub(1);
            }
        }
        best
    }

    fn max_op_prefix_that_fits(&self, ops: &[PatchOp]) -> usize {
        let mut low = 1usize;
        let mut high = ops.len();
        let mut best = 0usize;
        while low <= high {
            let middle = low + (high - low) / 2;
            if self.patch_ops_fit_budget(ops[..middle].to_vec()) {
                best = middle;
                low = middle.saturating_add(1);
            } else {
                high = middle.saturating_sub(1);
            }
        }
        best
    }

    fn emit_budgeted_ops(
        &mut self,
        ops: Vec<PatchOp>,
    ) -> Result<Vec<TerminalPatchV2>, TerminalWireError> {
        let mut patches = Vec::new();
        let mut offset = 0usize;
        while offset < ops.len() {
            let prefix_len = self.max_op_prefix_that_fits(&ops[offset..]);
            if prefix_len == 0
                && self.attrs.len() > self.emitted_attrs_len
                && self.patch_ops_fit_budget(Vec::new())
            {
                patches.push(self.emit_patch(Vec::new()));
                continue;
            }
            if prefix_len == 0 {
                return Err(TerminalWireError::new(
                    "single terminal patch operation exceeds relay budget",
                ));
            }
            patches.push(self.emit_patch(ops[offset..offset + prefix_len].to_vec()));
            offset += prefix_len;
        }
        Ok(patches)
    }

    fn emit_sync_patches(
        &mut self,
        appended: Vec<TerminalRow>,
        tail_ops: Vec<PatchOp>,
    ) -> Result<Vec<TerminalPatchV2>, TerminalWireError> {
        if appended.is_empty() {
            return self.emit_budgeted_ops(tail_ops);
        }

        let mut patches = Vec::new();
        let mut offset = 0usize;
        let mut tail_emitted = false;
        while offset < appended.len() {
            let remaining = &appended[offset..];
            let mut combined_ops = vec![PatchOp::AppendScrollback {
                rows: remaining.to_vec(),
            }];
            combined_ops.extend(tail_ops.iter().cloned());
            if self.patch_ops_fit_budget(combined_ops.clone()) {
                patches.push(self.emit_patch(combined_ops));
                tail_emitted = true;
                break;
            }

            let prefix_len = self.max_scrollback_prefix_that_fits(remaining);
            if prefix_len == 0
                && self.attrs.len() > self.emitted_attrs_len
                && self.patch_ops_fit_budget(Vec::new())
            {
                patches.push(self.emit_patch(Vec::new()));
                continue;
            }
            if prefix_len == 0 {
                return Err(TerminalWireError::new(
                    "single terminal scrollback row exceeds relay budget",
                ));
            }
            patches.push(self.emit_patch(vec![PatchOp::AppendScrollback {
                rows: remaining[..prefix_len].to_vec(),
            }]));
            offset += prefix_len;
        }

        if !tail_emitted && !tail_ops.is_empty() {
            patches.extend(self.emit_budgeted_ops(tail_ops)?);
        }
        Ok(patches)
    }

    pub(crate) fn apply_op(&mut self, op: &PatchOp) {
        match op {
            PatchOp::PutCells { row, col, cells } => self.put_cells(*row, *col, cells),
            PatchOp::ClearRange {
                row,
                col_start,
                col_end,
                attr_id,
            } => self.clear_range(*row, *col_start, *col_end, *attr_id),
            PatchOp::ReplaceRow { row, line } => self.replace_row(*row, line.clone()),
            PatchOp::ScrollRegion { top, bottom, delta } => {
                self.scroll_region(*top, *bottom, *delta);
            }
            PatchOp::SetCursor(cursor) => {
                self.cursor = cursor.clone();
            }
            PatchOp::SetTitle(title) => {
                self.title = title.clone();
            }
            PatchOp::SetPalette(palette) => {
                self.palette = palette.clone();
            }
            PatchOp::SetMode { mode, enabled } => match mode {
                TerminalMode::BracketedPaste => self.modes.bracketed_paste = *enabled,
                TerminalMode::ApplicationCursor => self.modes.application_cursor = *enabled,
            },
            PatchOp::SwitchAltScreen(enabled) => {
                self.modes.alt_screen = *enabled;
            }
            PatchOp::AppendScrollback { rows } => {
                for row in rows {
                    self.history.push_back(row.clone());
                }
                self.trim_history();
            }
            PatchOp::Bell => {}
        }
    }

    pub(crate) fn sync_from_vt_screen(&mut self) -> Result<TerminalFeedBatch, TerminalWireError> {
        let previous = self.sync_vt_screen_state();
        let reset_messages = if previous.scrollback_cleared {
            // The snapshot is the semantic RIS boundary. It deliberately has
            // no scrollback: every row captured after RIS is streamed below as
            // a retained patch from this newly committed snapshot base.
            self.snapshot_messages_with_scrollback(true, Vec::new())?
        } else {
            Vec::new()
        };
        let next_state_seq = self.state_seq.saturating_add(1);

        let mut ops = Vec::with_capacity(usize::from(self.rows) + 7);
        let appended = std::mem::take(&mut self.pending_scrollback_append);
        if !appended.is_empty() {
            self.transcript_store.record_rows(
                TerminalTranscriptEntryKind::NormalScrollback,
                appended.clone(),
                next_state_seq,
                &self.terminal_run_id,
                self.cols,
            );
        }
        if !previous.scrollback_cleared {
            for (row, line) in changed_rows(&previous.rows, &self.screen_rows) {
                let Ok(row) = u16::try_from(row) else {
                    break;
                };
                ops.push(PatchOp::ReplaceRow { row, line });
            }
            if self.cursor != previous.cursor {
                ops.push(PatchOp::SetCursor(self.cursor.clone()));
            }
            if self.title != previous.title {
                ops.push(PatchOp::SetTitle(self.title.clone()));
            }
            if self.modes.alt_screen != previous.modes.alt_screen {
                ops.push(PatchOp::SwitchAltScreen(self.modes.alt_screen));
            }
            if self.modes.bracketed_paste != previous.modes.bracketed_paste {
                ops.push(PatchOp::SetMode {
                    mode: TerminalMode::BracketedPaste,
                    enabled: self.modes.bracketed_paste,
                });
            }
            if self.modes.application_cursor != previous.modes.application_cursor {
                ops.push(PatchOp::SetMode {
                    mode: TerminalMode::ApplicationCursor,
                    enabled: self.modes.application_cursor,
                });
            }
        }
        if self.parser.callbacks().bell {
            ops.push(PatchOp::Bell);
            self.parser.callbacks_mut().bell = false;
        }
        let patches = if ops.is_empty() && appended.is_empty() {
            Vec::new()
        } else {
            self.emit_sync_patches(appended, ops)?
        };
        if self.modes.alt_screen {
            self.transcript_store.record_screen_frame(
                TerminalTranscriptEntryKind::AltScreenFrame,
                &self.screen_rows,
                self.state_seq,
                &self.terminal_run_id,
                self.cols,
            );
        } else if self.record_primary_screen_frames {
            self.transcript_store.record_screen_frame(
                TerminalTranscriptEntryKind::ScreenFrame,
                &self.screen_rows,
                self.state_seq,
                &self.terminal_run_id,
                self.cols,
            );
        } else {
            self.transcript_store.clear_screen_frame_dedupe();
        }

        Ok(TerminalFeedBatch {
            reset_messages,
            patches,
        })
    }

    pub(crate) fn sync_vt_screen_state(&mut self) -> PreviousTerminalState {
        let scrollback_update = self.parser.screen_mut().take_scrollback_update();
        let mut screen = self.parser.screen().clone();
        screen.set_scrollback(0);
        let (rows, cols) = screen.size();
        let previous = PreviousTerminalState {
            rows: self.screen_rows.clone(),
            cursor: self.cursor.clone(),
            title: self.title.clone(),
            modes: self.modes.clone(),
            scrollback_cleared: scrollback_update.cleared,
        };
        self.rows = rows;
        self.cols = cols;
        self.title = self.parser.callbacks().title.clone();
        self.cursor = cursor_from_vt_screen(&screen);
        self.modes = modes_from_vt_screen(&screen);

        // The attribute table is append-only across the terminal run so retained
        // history rows keep referencing valid `attr_id`s; reuse the existing
        // table and only append newly seen attributes.
        let mut attrs = std::mem::take(&mut self.attrs);
        let mut attr_index = std::mem::take(&mut self.attr_index);
        let mut rows = Vec::with_capacity(usize::from(self.rows));
        for row in 0..self.rows {
            rows.push(row_from_vt_screen(
                &screen,
                row,
                self.cols,
                next_line_id(&mut self.next_line_id),
                &mut attrs,
                &mut attr_index,
            ));
        }
        for (index, row) in rows.iter_mut().enumerate() {
            if let Some(previous_row) = previous.rows.get(index)
                && rows_render_equal(previous_row, row)
            {
                *row = previous_row.clone();
            }
        }
        self.screen_rows = rows;
        self.sync_scrollback_history(scrollback_update, &mut attrs, &mut attr_index);
        self.attrs = attrs;
        self.attr_index = attr_index;
        self.trim_history();

        previous
    }

    /// Fold the rows that newly scrolled off the top of the screen into the deep
    /// history and stage them for streaming via `PatchOp::AppendScrollback`.
    ///
    /// vt100 supplies the exact rows that scrolled off the top of the primary
    /// grid since the previous sync, including top-anchored scroll regions and
    /// rows evicted from its bounded native scrollback ring.
    pub(crate) fn sync_scrollback_history(
        &mut self,
        update: vt100::ScrollbackUpdate,
        attrs: &mut Vec<CellAttr>,
        attr_index: &mut HashMap<CellAttr, u32>,
    ) {
        self.pending_scrollback_append.clear();

        if update.cleared {
            self.history.clear();
        }

        // History is frozen while the app is disconnected and the PTY was
        // resized. The update has already been drained, so discarding its rows
        // prevents repaint churn from surfacing after history is thawed.
        if self.history_frozen {
            return;
        }

        let appended = update
            .rows
            .iter()
            .map(|row| {
                row_from_vt_scrollback(row, next_line_id(&mut self.next_line_id), attrs, attr_index)
            })
            .collect::<Vec<_>>();
        if appended.is_empty() {
            return;
        }
        self.history.extend(appended.iter().cloned());
        self.trim_history();
        self.pending_scrollback_append = appended;
    }
}
