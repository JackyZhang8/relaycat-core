use super::*;

pub(crate) fn mark_app_disconnected(app_connected: &AtomicBool) {
    app_connected.store(false, Ordering::Release);
}

#[derive(Debug, Default)]
pub(crate) struct AppResumeGate {
    pub(crate) resume_ready: bool,
}

impl AppResumeGate {
    pub(crate) fn mark_app_rejoined(&mut self) {
        self.resume_ready = false;
    }

    pub(crate) fn mark_app_disconnected(&mut self) {
        self.resume_ready = false;
    }

    pub(crate) fn mark_resume_processed(&mut self) {
        self.resume_ready = true;
    }

    pub(crate) fn can_send_terminal_state(&self, app_connected: bool) -> bool {
        app_connected && self.resume_ready
    }
}

pub(crate) fn current_pty_work_mode(mode: &Arc<Mutex<PtyWorkMode>>) -> PtyWorkModeKind {
    mode.lock()
        .map(|mode| mode.current())
        .unwrap_or(PtyWorkModeKind::Remote)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PtyWorkModeKind {
    Remote,
    Local,
}

#[derive(Debug, Clone)]
pub(crate) struct PtyWorkMode {
    pub(crate) kind: PtyWorkModeKind,
    pub(crate) remote_size: Option<(u16, u16)>,
    pub(crate) accept_next_remote_size: bool,
}

impl Default for PtyWorkMode {
    fn default() -> Self {
        Self::new()
    }
}

impl PtyWorkMode {
    pub(crate) fn new() -> Self {
        Self {
            kind: PtyWorkModeKind::Remote,
            remote_size: None,
            accept_next_remote_size: true,
        }
    }

    pub(crate) fn current(&self) -> PtyWorkModeKind {
        self.kind
    }

    pub(crate) fn observe_app_disconnected(&mut self) -> bool {
        false
    }

    pub(crate) fn observe_remote_size(&mut self, size: (u16, u16)) -> bool {
        if self.kind != PtyWorkModeKind::Remote || !self.accept_next_remote_size {
            return false;
        }
        self.remote_size = Some(size);
        self.accept_next_remote_size = false;
        true
    }

    pub(crate) fn enter_local_mode(&mut self) -> bool {
        if self.kind == PtyWorkModeKind::Local {
            return false;
        }
        self.kind = PtyWorkModeKind::Local;
        true
    }

    pub(crate) fn enter_remote_mode(&mut self) -> bool {
        if self.kind == PtyWorkModeKind::Remote {
            return false;
        }
        self.kind = PtyWorkModeKind::Remote;
        self.accept_next_remote_size = self.remote_size.is_none();
        true
    }

    pub(crate) fn remote_size(&self) -> Option<(u16, u16)> {
        self.remote_size
    }
}

pub(crate) fn pty_work_mode_observe_app_resize(mode: &mut PtyWorkMode, size: (u16, u16)) -> bool {
    mode.observe_remote_size(size)
}

