use super::*;

struct SnapshotPlan {
    snapshot: TerminalSnapshotV2,
    patches: Vec<TerminalPatchV2>,
    final_state_seq: u64,
}

impl TerminalCore {
    pub fn snapshot(&mut self) -> TerminalSnapshotV2 {
        self.snapshot_with_reset_app_cache(false)
    }

    pub fn snapshot_with_reset_app_cache(&mut self, reset_app_cache: bool) -> TerminalSnapshotV2 {
        if self.snapshot_emitted {
            self.active_snapshot_id = self.next_snapshot_id;
            self.next_snapshot_id = self.next_snapshot_id.saturating_add(1);
        } else {
            self.snapshot_emitted = true;
        }
        self.retained_patches.clear();
        // The snapshot carries the full attribute table, so subsequent patches
        // only need to re-send entries added after this point.
        self.emitted_attrs_len = self.attrs.len();

        let mut snapshot = self.budgeted_current_snapshot();
        snapshot.reset_app_cache = reset_app_cache;
        snapshot
    }

    /// Build a relay-safe snapshot transaction and commit it atomically.
    ///
    /// A normal-sized state is one snapshot. An oversized state starts with a
    /// lightweight snapshot and reconstructs scrollback and visible rows with
    /// independently sequenced patches that can be resumed after disconnect.
    pub fn snapshot_messages(
        &mut self,
        reset_app_cache: bool,
    ) -> Result<Vec<PlainMsg>, TerminalWireError> {
        self.snapshot_messages_with_scrollback(reset_app_cache, self.snapshot_scrollback_window())
    }

    pub(crate) fn snapshot_messages_with_full_history(
        &mut self,
        reset_app_cache: bool,
    ) -> Result<Vec<PlainMsg>, TerminalWireError> {
        let cols = usize::from(self.cols);
        let history = self
            .history
            .iter()
            .map(|row| normalize_row_to_cols(row, cols))
            .collect();
        self.snapshot_messages_with_scrollback(reset_app_cache, history)
    }

    pub(crate) fn snapshot_messages_with_scrollback(
        &mut self,
        reset_app_cache: bool,
        scrollback_window: Vec<TerminalRow>,
    ) -> Result<Vec<PlainMsg>, TerminalWireError> {
        let snapshot_id = if self.snapshot_emitted {
            self.next_snapshot_id
        } else {
            self.active_snapshot_id
        };
        let plan = match self.build_snapshot_plan(
            snapshot_id,
            reset_app_cache,
            scrollback_window,
            TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                // The bootstrap snapshot itself blew the budget, which can
                // only be the append-only attribute table after a long
                // truecolor run. Garbage-collect it (the snapshot we are
                // about to emit resets the app's table anyway) and retry.
                let Some(remap) = self.compact_attrs() else {
                    return Err(error);
                };
                let mut scrollback_window = self.snapshot_scrollback_window();
                remap_row_attr_ids(scrollback_window.iter_mut(), &remap);
                self.build_snapshot_plan(
                    snapshot_id,
                    reset_app_cache,
                    scrollback_window,
                    TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES,
                )?
            }
        };
        self.commit_snapshot_plan(&plan);

        let mut messages = Vec::with_capacity(plan.patches.len().saturating_add(1));
        messages.push(PlainMsg::TerminalSnapshotV2(plan.snapshot));
        messages.extend(plan.patches.into_iter().map(PlainMsg::TerminalPatchV2));
        Ok(messages)
    }

    fn build_snapshot_plan(
        &self,
        snapshot_id: u64,
        reset_app_cache: bool,
        scrollback_window: Vec<TerminalRow>,
        budget: usize,
    ) -> Result<SnapshotPlan, TerminalWireError> {
        let mut full_snapshot = self.current_snapshot();
        full_snapshot.snapshot_id = snapshot_id;
        full_snapshot.reset_app_cache = reset_app_cache;
        full_snapshot.scrollback_window = scrollback_window;
        if terminal_snapshot_v2_encoded_len(&full_snapshot) <= budget {
            return Ok(SnapshotPlan {
                final_state_seq: full_snapshot.state_seq,
                snapshot: full_snapshot,
                patches: Vec::new(),
            });
        }

        let reconstruction_scrollback = std::mem::take(&mut full_snapshot.scrollback_window);
        let reconstruction_screen = std::mem::take(&mut full_snapshot.screen_rows);
        full_snapshot.state_seq = 0;
        full_snapshot.screen_rows = reconstruction_screen
            .iter()
            .map(|row| TerminalRow {
                line_id: row.line_id,
                wrapped: row.wrapped,
                cells: Vec::new(),
            })
            .collect();
        let snapshot_len = terminal_snapshot_v2_encoded_len(&full_snapshot);
        if snapshot_len > budget {
            return Err(TerminalWireError::new(format!(
                "terminal bootstrap snapshot exceeds relay budget: {snapshot_len} > {budget}"
            )));
        }

        let mut patches = Vec::new();
        let mut next_state_seq = 1_u64;
        self.build_scrollback_reconstruction_patches(
            snapshot_id,
            &reconstruction_scrollback,
            budget,
            &mut next_state_seq,
            &mut patches,
        )?;
        let screen_ops = reconstruction_screen
            .into_iter()
            .enumerate()
            .map(|(row, line)| {
                let row = u16::try_from(row).map_err(|_| {
                    TerminalWireError::new("terminal screen row index exceeds protocol range")
                })?;
                Ok(PatchOp::ReplaceRow { row, line })
            })
            .collect::<Result<Vec<_>, TerminalWireError>>()?;
        self.build_op_reconstruction_patches(
            snapshot_id,
            &screen_ops,
            budget,
            &mut next_state_seq,
            &mut patches,
        )?;

        Ok(SnapshotPlan {
            snapshot: full_snapshot,
            final_state_seq: next_state_seq.saturating_sub(1),
            patches,
        })
    }

    fn build_scrollback_reconstruction_patches(
        &self,
        snapshot_id: u64,
        rows: &[TerminalRow],
        budget: usize,
        next_state_seq: &mut u64,
        patches: &mut Vec<TerminalPatchV2>,
    ) -> Result<(), TerminalWireError> {
        let mut offset = 0usize;
        while offset < rows.len() {
            let mut low = 1usize;
            let mut high = rows.len() - offset;
            let mut best = 0usize;
            while low <= high {
                let middle = low + (high - low) / 2;
                let patch = self.reconstruction_patch(
                    snapshot_id,
                    *next_state_seq,
                    vec![PatchOp::AppendScrollback {
                        rows: rows[offset..offset + middle].to_vec(),
                    }],
                );
                if terminal_patch_v2_encoded_len(&patch) <= budget {
                    best = middle;
                    low = middle.saturating_add(1);
                } else {
                    high = middle.saturating_sub(1);
                }
            }
            if best == 0 {
                return Err(TerminalWireError::new(
                    "single terminal scrollback row exceeds relay budget",
                ));
            }
            patches.push(self.reconstruction_patch(
                snapshot_id,
                *next_state_seq,
                vec![PatchOp::AppendScrollback {
                    rows: rows[offset..offset + best].to_vec(),
                }],
            ));
            *next_state_seq = next_state_seq.saturating_add(1);
            offset += best;
        }
        Ok(())
    }

    fn build_op_reconstruction_patches(
        &self,
        snapshot_id: u64,
        ops: &[PatchOp],
        budget: usize,
        next_state_seq: &mut u64,
        patches: &mut Vec<TerminalPatchV2>,
    ) -> Result<(), TerminalWireError> {
        let mut offset = 0usize;
        while offset < ops.len() {
            let mut low = 1usize;
            let mut high = ops.len() - offset;
            let mut best = 0usize;
            while low <= high {
                let middle = low + (high - low) / 2;
                let patch = self.reconstruction_patch(
                    snapshot_id,
                    *next_state_seq,
                    ops[offset..offset + middle].to_vec(),
                );
                if terminal_patch_v2_encoded_len(&patch) <= budget {
                    best = middle;
                    low = middle.saturating_add(1);
                } else {
                    high = middle.saturating_sub(1);
                }
            }
            if best == 0 {
                return Err(TerminalWireError::new(
                    "single terminal screen row exceeds relay budget",
                ));
            }
            patches.push(self.reconstruction_patch(
                snapshot_id,
                *next_state_seq,
                ops[offset..offset + best].to_vec(),
            ));
            *next_state_seq = next_state_seq.saturating_add(1);
            offset += best;
        }
        Ok(())
    }

    fn reconstruction_patch(
        &self,
        snapshot_id: u64,
        state_seq: u64,
        ops: Vec<PatchOp>,
    ) -> TerminalPatchV2 {
        TerminalPatchV2 {
            terminal_run_id: self.terminal_run_id.clone(),
            base_snapshot_id: snapshot_id,
            from_state_seq: state_seq,
            to_state_seq: state_seq,
            attrs: Vec::new(),
            attrs_base_len: None,
            ops,
        }
    }

    fn commit_snapshot_plan(&mut self, plan: &SnapshotPlan) {
        if self.snapshot_emitted {
            self.active_snapshot_id = plan.snapshot.snapshot_id;
            self.next_snapshot_id = plan.snapshot.snapshot_id.saturating_add(1);
        } else {
            self.snapshot_emitted = true;
            self.active_snapshot_id = plan.snapshot.snapshot_id;
        }
        self.state_seq = plan.final_state_seq;
        self.retained_patches.clear();
        for patch in &plan.patches {
            self.retain_patch(patch.clone());
        }
        self.emitted_attrs_len = self.attrs.len();
    }

    pub fn resize_messages(
        &mut self,
        event: ResizeEventV2,
        reset_app_cache: bool,
    ) -> Result<Vec<PlainMsg>, TerminalWireError> {
        let (cols, rows) = bounded_terminal_size(event.cols, event.rows);
        let ack = ResizeAckV2 {
            resize_seq: event.resize_seq,
        };
        if cols != self.cols || rows != self.rows {
            // Trim at the new cell budget before widening retained vt100 rows;
            // otherwise a narrow 2048-row history can briefly expand to 4096
            // columns before being reduced to 64 rows.
            self.parser
                .screen_mut()
                .set_scrollback_len(terminal_history_max_rows(cols));
            self.parser.screen_mut().set_size(rows, cols);
            self.sync_vt_screen_state();
            self.normalize_history_to_current_cols();
        }

        let transaction = self.snapshot_messages(reset_app_cache)?;
        let mut messages = Vec::with_capacity(transaction.len().saturating_add(1));
        messages.push(PlainMsg::ResizeAckV2(ack));
        messages.extend(transaction);
        Ok(messages)
    }

    /// Serve a page of screen transcript entries. Unlike terminal scrollback,
    /// this can include deduplicated alternate-screen frames for full-screen
    /// TUIs such as codex, vim, or htop.
    pub fn transcript_chunk(&self, request: &RequestTranscriptV2) -> TranscriptChunkV2 {
        self.transcript_store
            .chunk(request, &self.terminal_run_id, self.attrs.clone())
    }

    pub fn resize(&mut self, event: ResizeEventV2) -> (ResizeAckV2, TerminalSnapshotV2) {
        let (cols, rows) = bounded_terminal_size(event.cols, event.rows);
        if cols == self.cols && rows == self.rows {
            self.emitted_attrs_len = self.attrs.len();
            return (
                ResizeAckV2 {
                    resize_seq: event.resize_seq,
                },
                self.budgeted_current_snapshot(),
            );
        }

        self.active_snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id = self.next_snapshot_id.saturating_add(1);
        self.snapshot_emitted = true;
        self.state_seq = 0;
        self.retained_patches.clear();
        // Apply the new history budget before resizing retained rows to the new
        // width, then synchronize the visible grid and pending scrollback.
        self.parser
            .screen_mut()
            .set_scrollback_len(terminal_history_max_rows(cols));
        self.parser.screen_mut().set_size(rows, cols);
        self.sync_vt_screen_state();
        self.normalize_history_to_current_cols();
        self.emitted_attrs_len = self.attrs.len();

        (
            ResizeAckV2 {
                resize_seq: event.resize_seq,
            },
            self.budgeted_current_snapshot(),
        )
    }

    pub fn resize_with_reset_app_cache(
        &mut self,
        event: ResizeEventV2,
        reset_app_cache: bool,
    ) -> (ResizeAckV2, TerminalSnapshotV2) {
        if !reset_app_cache {
            return self.resize(event);
        }
        if event.cols == self.cols && event.rows == self.rows {
            return (
                ResizeAckV2 {
                    resize_seq: event.resize_seq,
                },
                self.snapshot_with_reset_app_cache(true),
            );
        }

        let (ack, mut snapshot) = self.resize(event);
        snapshot.reset_app_cache = true;
        (ack, snapshot)
    }

    pub fn ack_render(&mut self, ack: RenderAckV2) {
        if ack.terminal_run_id != self.terminal_run_id || ack.snapshot_id != self.active_snapshot_id
        {
            return;
        }

        while self
            .retained_patches
            .front()
            .is_some_and(|patch| patch.to_state_seq <= ack.applied_state_seq)
        {
            self.retained_patches.pop_front();
        }
    }

    pub fn retained_patches_after(
        &self,
        snapshot_id: u64,
        state_seq: u64,
    ) -> Option<Vec<TerminalPatchV2>> {
        if snapshot_id != self.active_snapshot_id {
            return None;
        }
        if state_seq == self.state_seq {
            return Some(Vec::new());
        }
        if state_seq > self.state_seq {
            return None;
        }

        let expected_first = state_seq.saturating_add(1);
        let first_index = self
            .retained_patches
            .iter()
            .position(|patch| patch.from_state_seq == expected_first)?;

        let mut expected = expected_first;
        let mut patches = Vec::new();
        for patch in self.retained_patches.iter().skip(first_index) {
            if patch.from_state_seq != expected {
                return None;
            }
            expected = patch.to_state_seq.saturating_add(1);
            patches.push(patch.clone());
            if expected > self.state_seq {
                return Some(patches);
            }
        }

        None
    }

    pub fn resume_messages(
        &mut self,
        resume: &ResumeV2,
    ) -> Result<Vec<PlainMsg>, TerminalWireError> {
        let can_resume = resume.terminal_run_id.as_deref() == Some(self.terminal_run_id.as_str())
            && resume.last_snapshot_id == Some(self.active_snapshot_id);

        if can_resume
            && let Some(patches) =
                self.retained_patches_after(self.active_snapshot_id, resume.last_applied_state_seq)
            && (self.incremental_attrs_enabled()
                || patches.iter().all(|patch| patch.attrs_base_len.is_none()))
            && patches_fit_relay_budget(&patches)
        {
            return Ok(coalesce_resume_patches(patches)
                .into_iter()
                .map(PlainMsg::TerminalPatchV2)
                .collect());
        }

        self.snapshot_messages(true)
    }

    pub(crate) fn current_snapshot(&self) -> TerminalSnapshotV2 {
        TerminalSnapshotV2 {
            terminal_run_id: self.terminal_run_id.clone(),
            snapshot_id: self.active_snapshot_id,
            state_seq: self.state_seq,
            cols: self.cols,
            rows: self.rows,
            title: self.title.clone(),
            cursor: self.cursor.clone(),
            modes: self.modes.clone(),
            palette: self.palette.clone(),
            attrs: self.attrs.clone(),
            reset_app_cache: false,
            scrollback_window: self.snapshot_scrollback_window(),
            screen_rows: self.screen_rows.clone(),
        }
    }

    /// The most recent retained scrollback to seed a snapshot's local cache.
    ///
    /// Each row is normalized to the current terminal width (`self.cols`) so the
    /// snapshot never ships rows wider or narrower than the declared `cols`.
    /// This matters after a LocalMode/RemoteMode round-trip: rows that scrolled
    /// off in LocalMode were recorded at the wider local width and must be
    /// truncated/padded before being sent to the app.
    pub(crate) fn snapshot_scrollback_window(&self) -> Vec<TerminalRow> {
        let max_rows = usize::from(self.rows).saturating_mul(TERMINAL_SNAPSHOT_SCROLLBACK_SCREENS);
        let cols = usize::from(self.cols);
        let start = self.history.len().saturating_sub(max_rows);
        self.history
            .iter()
            .skip(start)
            .map(|row| normalize_row_to_cols(row, cols))
            .collect()
    }

    pub(crate) fn budgeted_current_snapshot(&self) -> TerminalSnapshotV2 {
        let mut snapshot = self.current_snapshot();
        while snapshot.scrollback_window.len() > 1
            && terminal_snapshot_v2_encoded_len(&snapshot) > TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES
        {
            let remove = (snapshot.scrollback_window.len() / 4).max(1);
            snapshot.scrollback_window.drain(..remove);
        }
        if terminal_snapshot_v2_encoded_len(&snapshot) > TERMINAL_RELAY_SAFE_PLAIN_MSG_BYTES {
            snapshot.scrollback_window.clear();
        }
        snapshot
    }

    pub(crate) fn retain_patch(&mut self, patch: TerminalPatchV2) {
        if self.patch_retention == 0 {
            return;
        }
        self.retained_patches.push_back(patch);
        while self.retained_patches.len() > self.patch_retention {
            self.retained_patches.pop_front();
        }
    }
}
