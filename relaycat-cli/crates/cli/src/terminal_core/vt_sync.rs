use super::*;

impl TerminalCore {
    pub(crate) fn emit_patch(&mut self, ops: Vec<PatchOp>) -> TerminalPatchV2 {
        let next_seq = self.state_seq.saturating_add(1);
        self.state_seq = next_seq;

        // The attribute table is append-only, so only re-send it when it has
        // grown since the last emission; an empty table means "reuse". When the
        // peer negotiated incremental attrs, send just the appended tail keyed
        // by its base index instead of the whole (only-growing) table.
        let (attrs, attrs_base_len) = if self.attrs.len() > self.emitted_attrs_len {
            let base = self.emitted_attrs_len;
            let emitted = if self.incremental_attrs_enabled() {
                (
                    self.attrs[base..].to_vec(),
                    Some(u32::try_from(base).expect("attr table base index overflow")),
                )
            } else {
                (self.attrs.clone(), None)
            };
            self.emitted_attrs_len = self.attrs.len();
            emitted
        } else {
            (Vec::new(), None)
        };

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
                while self.history.len() > TERMINAL_HISTORY_MAX_ROWS {
                    self.history.pop_front();
                }
            }
            PatchOp::Bell => {}
        }
    }

    pub(crate) fn observe_scroll_region_controls(&mut self, bytes: &[u8]) -> Option<TrackedScrollRegion> {
        let mut saw_region_control = false;
        let mut region_for_history = None;
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
            if sequence.last() == Some(&b'r') {
                saw_region_control = true;
                self.scroll_region_history = tracked_scroll_region_from_csi_r(sequence, self.rows);
                if self.scroll_region_history.is_some() {
                    region_for_history = self.scroll_region_history;
                }
            }
            index += final_offset + 1;
        }

        if saw_region_control {
            region_for_history
        } else {
            self.scroll_region_history
        }
    }

    pub(crate) fn stage_top_anchored_scroll_region_history(
        &mut self,
        previous: &PreviousTerminalState,
        region: Option<TrackedScrollRegion>,
    ) {
        if self.history_frozen
            || self.modes.alt_screen
            || !self.pending_scrollback_append.is_empty()
        {
            return;
        }
        let Some(region) = region else {
            return;
        };
        if region.top != 0 || region.bottom >= self.rows {
            return;
        }
        let shifted_out =
            top_anchored_scroll_region_shifted_out_rows(previous, &self.screen_rows, region);
        if shifted_out.is_empty() {
            return;
        }

        let appended: Vec<TerminalRow> = shifted_out
            .into_iter()
            .map(|mut row| {
                row.line_id = next_line_id(&mut self.next_line_id);
                row
            })
            .collect();
        self.history.extend(appended.iter().cloned());
        self.trim_history();
        self.pending_scrollback_append = appended;
    }

    pub(crate) fn sync_from_vt_screen(
        &mut self,
        scroll_region_for_history: Option<TrackedScrollRegion>,
    ) -> Option<TerminalPatchV2> {
        let previous = self.sync_vt_screen_state();
        self.stage_top_anchored_scroll_region_history(&previous, scroll_region_for_history);
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
            ops.push(PatchOp::AppendScrollback { rows: appended });
        }
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
        if self.parser.callbacks().bell {
            ops.push(PatchOp::Bell);
            self.parser.callbacks_mut().bell = false;
        }
        if ops.is_empty() {
            return None;
        }
        if self.modes.alt_screen {
            self.transcript_store.record_screen_frame(
                TerminalTranscriptEntryKind::AltScreenFrame,
                &self.screen_rows,
                next_state_seq,
                &self.terminal_run_id,
                self.cols,
            );
        } else if self.record_primary_screen_frames {
            self.transcript_store.record_screen_frame(
                TerminalTranscriptEntryKind::ScreenFrame,
                &self.screen_rows,
                next_state_seq,
                &self.terminal_run_id,
                self.cols,
            );
        } else {
            self.transcript_store.clear_screen_frame_dedupe();
        }

        Some(self.emit_patch(ops))
    }

    pub(crate) fn sync_vt_screen_state(&mut self) -> PreviousTerminalState {
        let mut screen = self.parser.screen().clone();
        screen.set_scrollback(0);
        let (rows, cols) = screen.size();
        let previous = PreviousTerminalState {
            rows: self.screen_rows.clone(),
            cursor: self.cursor.clone(),
            title: self.title.clone(),
            modes: self.modes.clone(),
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
        self.sync_scrollback_history(&screen, &mut attrs, &mut attr_index);
        self.attrs = attrs;
        self.attr_index = attr_index;

        previous
    }

    /// Fold the rows that newly scrolled off the top of the screen into the deep
    /// history and stage them for streaming via `PatchOp::AppendScrollback`.
    ///
    /// The vt100 parser keeps its own faithful scrollback (sized to the deep
    /// history): rows are appended at the bottom as the screen scrolls and are
    /// never reflowed on resize. So the number of rows we have already consumed
    /// (`scrollback_seen`) deterministically identifies the new tail — we just
    /// read the most-recent `cur_len - scrollback_seen` scrollback rows. This
    /// replaces the previous content-alignment heuristic, whose fallback could
    /// mistake a TUI repaint for a whole fresh window and duplicate history.
    pub(crate) fn sync_scrollback_history(
        &mut self,
        screen: &vt100::Screen,
        attrs: &mut Vec<CellAttr>,
        attr_index: &mut HashMap<CellAttr, u32>,
    ) {
        self.pending_scrollback_append = Vec::new();

        // While the alternate screen is active the main-screen scrollback is
        // frozen; leave the consumed count untouched so nothing is duplicated
        // when the alternate screen exits and the scrollback reappears.
        if self.modes.alt_screen {
            return;
        }

        let mut scrollback = screen.clone();
        scrollback.set_scrollback(usize::MAX);
        let cur_len = scrollback.scrollback();

        // History is frozen while the app is disconnected and the PTY was
        // resized: the running TUI repaints at the new width and that churn
        // scrolls off here. Advance the consumed counter to the current length
        // so the churn is discarded (never folded into `history` and never
        // emitted as an append) — and so that, once thawed, only genuinely new
        // rows are appended. This is the deterministic, content-agnostic way to
        // drop the transient resize churn the user agreed to lose.
        if self.history_frozen {
            self.scrollback_seen = cur_len;
            return;
        }

        if cur_len < self.scrollback_seen {
            // The scrollback shrank (e.g. it was explicitly cleared). Rebuild
            // the deep history from the current faithful scrollback rather than
            // streaming a spurious append; the app re-syncs from the next
            // snapshot.
            //
            // Use `read_recent_scrollback_faithful` so each row is read at its
            // original width — surviving rows may have been generated at a
            // wider terminal size and would be silently truncated if we forced
            // the current (possibly narrower) `self.cols`.
            let rebuilt = read_recent_scrollback_faithful(
                &mut scrollback,
                cur_len,
                self.cols,
                &mut self.next_line_id,
                attrs,
                attr_index,
            );
            self.history.clear();
            self.history.extend(rebuilt);
            self.trim_history();
            self.scrollback_seen = cur_len;
            return;
        }

        let appended_count = cur_len - self.scrollback_seen;
        self.scrollback_seen = cur_len;
        if appended_count == 0 {
            return;
        }

        let appended = read_recent_scrollback(
            &mut scrollback,
            appended_count,
            self.cols,
            &mut self.next_line_id,
            attrs,
            attr_index,
        );
        self.history.extend(appended.iter().cloned());
        self.trim_history();
        self.pending_scrollback_append = appended;
    }
}
