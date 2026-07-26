use std::fmt;

use relaycat_protocol::{
    InputAckV2, InputDecision, InputDedupe, PlainMsg, ResumeAcceptMode, ResumeAcceptedV2,
    TerminalStreamMessageV2, TerminalStreamV2,
};

use crate::terminal_core::{TerminalCore, TerminalCoreConfig, TerminalWireError};

pub const WORKSPACE_SHELL_STREAM_ID: &str = "workspace_shell";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceTerminalAction {
    Write(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkspaceTerminalResult {
    pub actions: Vec<WorkspaceTerminalAction>,
    pub outbound: Vec<TerminalStreamV2>,
}

#[derive(Debug)]
pub enum WorkspaceTerminalError {
    WrongStream(String),
    InputGap { expected: u64, received: u64 },
    UnsupportedInboundMessage,
    Wire(TerminalWireError),
}

impl fmt::Display for WorkspaceTerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongStream(stream_id) => {
                write!(formatter, "unknown terminal stream: {stream_id}")
            }
            Self::InputGap { expected, received } => {
                write!(
                    formatter,
                    "workspace terminal input gap: expected {expected}, got {received}"
                )
            }
            Self::UnsupportedInboundMessage => {
                formatter.write_str("unsupported inbound workspace terminal message")
            }
            Self::Wire(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for WorkspaceTerminalError {}

impl From<TerminalWireError> for WorkspaceTerminalError {
    fn from(error: TerminalWireError) -> Self {
        Self::Wire(error)
    }
}

pub struct WorkspaceTerminalHost {
    core: TerminalCore,
    input_dedupe: InputDedupe,
}

impl WorkspaceTerminalHost {
    pub fn new(cols: u16, rows: u16, patch_retention: usize) -> Self {
        Self {
            core: TerminalCore::new(TerminalCoreConfig {
                terminal_run_id: format!("{WORKSPACE_SHELL_STREAM_ID}-run"),
                cols,
                rows,
                patch_retention,
            }),
            input_dedupe: InputDedupe::default(),
        }
    }

    pub fn initial_messages(&mut self) -> Result<Vec<TerminalStreamV2>, WorkspaceTerminalError> {
        Self::wrap_plain_messages(self.core.snapshot_messages(false)?)
    }

    pub fn feed_output(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<TerminalStreamV2>, WorkspaceTerminalError> {
        Self::wrap_plain_messages(
            self.core
                .feed_vt_bytes_transport_batch(bytes)?
                .into_messages(),
        )
    }

    pub fn handle_message(
        &mut self,
        stream: TerminalStreamV2,
    ) -> Result<WorkspaceTerminalResult, WorkspaceTerminalError> {
        if stream.stream_id != WORKSPACE_SHELL_STREAM_ID {
            return Err(WorkspaceTerminalError::WrongStream(stream.stream_id));
        }

        let mut result = WorkspaceTerminalResult::default();
        let messages = match stream.message {
            TerminalStreamMessageV2::Input {
                input_stream_id,
                input_seq,
                bytes,
            } => {
                let decision = self.input_dedupe.observe(&input_stream_id, input_seq);
                match decision {
                    InputDecision::Accept => {
                        result.actions.push(WorkspaceTerminalAction::Write(bytes))
                    }
                    InputDecision::Duplicate => {}
                    InputDecision::Gap => {
                        return Err(WorkspaceTerminalError::InputGap {
                            expected: self
                                .input_dedupe
                                .highest_contiguous_input_seq()
                                .saturating_add(1),
                            received: input_seq,
                        });
                    }
                }
                vec![PlainMsg::InputAckV2(InputAckV2 {
                    input_stream_id: self
                        .input_dedupe
                        .input_stream_id()
                        .unwrap_or("legacy")
                        .to_string(),
                    highest_contiguous_input_seq: self.input_dedupe.highest_contiguous_input_seq(),
                })]
            }
            TerminalStreamMessageV2::RenderAck(ack) => {
                self.core.ack_render(ack);
                Vec::new()
            }
            TerminalStreamMessageV2::RequestSnapshot(_) => self.core.snapshot_messages(true)?,
            TerminalStreamMessageV2::Resume(resume) => {
                self.input_dedupe
                    .synchronize_ack(&resume.input_stream_id, resume.last_input_ack);
                let mut messages = self.core.resume_messages(&resume)?;
                let mode = if messages
                    .iter()
                    .any(|message| matches!(message, PlainMsg::TerminalSnapshotV2(_)))
                {
                    ResumeAcceptMode::SendingSnapshot
                } else if messages
                    .iter()
                    .any(|message| matches!(message, PlainMsg::TerminalPatchV2(_)))
                {
                    ResumeAcceptMode::ReplayingPatches
                } else {
                    ResumeAcceptMode::UpToDate
                };
                messages.insert(
                    0,
                    PlainMsg::ResumeAcceptedV2(ResumeAcceptedV2 {
                        mode,
                        target_state_seq: self.core.current_state_seq(),
                    }),
                );
                messages
            }
            TerminalStreamMessageV2::Resize(event) => {
                self.input_dedupe
                    .synchronize_ack(&event.input_stream_id, event.last_input_ack);
                result.actions.push(WorkspaceTerminalAction::Resize {
                    cols: event.cols,
                    rows: event.rows,
                });
                self.core.resize_messages(event, false)?
            }
            TerminalStreamMessageV2::Exit { .. } => {
                result.actions.push(WorkspaceTerminalAction::Close);
                Vec::new()
            }
            TerminalStreamMessageV2::Snapshot(_)
            | TerminalStreamMessageV2::Patch(_)
            | TerminalStreamMessageV2::ResumeAccepted(_)
            | TerminalStreamMessageV2::InputAck(_)
            | TerminalStreamMessageV2::ResizeAck(_) => {
                return Err(WorkspaceTerminalError::UnsupportedInboundMessage);
            }
        };
        result.outbound = Self::wrap_plain_messages(messages)?;
        Ok(result)
    }

    fn wrap_plain_messages(
        messages: Vec<PlainMsg>,
    ) -> Result<Vec<TerminalStreamV2>, WorkspaceTerminalError> {
        messages
            .into_iter()
            .map(|message| {
                let message = match message {
                    PlainMsg::TerminalSnapshotV2(snapshot) => {
                        TerminalStreamMessageV2::Snapshot(snapshot)
                    }
                    PlainMsg::TerminalPatchV2(patch) => TerminalStreamMessageV2::Patch(patch),
                    PlainMsg::InputAckV2(ack) => TerminalStreamMessageV2::InputAck(ack),
                    PlainMsg::ResizeAckV2(ack) => TerminalStreamMessageV2::ResizeAck(ack),
                    PlainMsg::ResumeAcceptedV2(accepted) => {
                        TerminalStreamMessageV2::ResumeAccepted(accepted)
                    }
                    _ => return Err(WorkspaceTerminalError::UnsupportedInboundMessage),
                };
                Ok(TerminalStreamV2 {
                    stream_id: WORKSPACE_SHELL_STREAM_ID.to_string(),
                    message,
                })
            })
            .collect()
    }
}
