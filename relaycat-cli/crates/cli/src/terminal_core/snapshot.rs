use super::*;

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

    /// Serve a page of screen transcript entries. Unlike terminal scrollback,
    /// this can include deduplicated alternate-screen frames for full-screen
    /// TUIs such as codex, vim, or htop.
    pub fn transcript_chunk(&self, request: &RequestTranscriptV2) -> TranscriptChunkV2 {
        self.transcript_store
            .chunk(request, &self.terminal_run_id, self.attrs.clone())
    }

    pub fn resize(&mut self, event: ResizeEventV2) -> (ResizeAckV2, TerminalSnapshotV2) {
        if event.cols == self.cols && event.rows == self.rows {
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
        // vt100 resizes only the visible grid; it never reflows or rewrites the
        // scrollback, so `scrollback_seen` stays valid across a resize and the
        // running TUI's post-resize repaint is captured faithfully (one copy,
        // exactly like the local terminal) instead of being amplified.
        self.parser.screen_mut().set_size(event.rows, event.cols);
        self.scroll_region_history = None;
        self.sync_vt_screen_state();
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

    pub fn resume_messages(&mut self, resume: &ResumeV2) -> Vec<PlainMsg> {
        let can_resume = resume.terminal_run_id.as_deref() == Some(self.terminal_run_id.as_str())
            && resume.last_snapshot_id == Some(self.active_snapshot_id);

        if can_resume
            && let Some(patches) =
                self.retained_patches_after(self.active_snapshot_id, resume.last_applied_state_seq)
            && patches_fit_relay_budget(&patches)
            && patches_fit_resume_scrollback_window(&patches, self.rows)
        {
            return coalesce_resume_patches(patches)
                .into_iter()
                .map(PlainMsg::TerminalPatchV2)
                .collect();
        }

        vec![PlainMsg::TerminalSnapshotV2(
            self.snapshot_with_reset_app_cache(true),
        )]
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
