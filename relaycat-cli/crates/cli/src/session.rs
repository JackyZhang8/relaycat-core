//! Frontend-agnostic PTY session primitive shared by the CLI and the desktop
//! GUI (`relaycat-gui`).
//!
//! [`crate::pty::run_interactive`] wires a PTY to the process's real stdin /
//! stdout / `SIGWINCH`, which is exactly what a terminal CLI wants. A GUI has
//! no controlling TTY: its "terminal" is an `xterm.js` widget in a webview, so
//! it needs to drive the PTY directly — clone the reader to stream bytes to the
//! frontend, take the writer to forward keystrokes, resize on demand, and kill
//! the child when the tab closes. [`PtySession`] exposes exactly those handles
//! without making any assumption about who renders the bytes.

use std::io::{Read, Write};

use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize, native_pty_system};

use crate::command::TargetCommand;
use crate::relay::kill_child_process_group;

/// A spawned child process attached to a PTY, decoupled from any particular
/// frontend. The caller owns rendering: clone [`PtySession::reader`] to read
/// the child's output and take [`PtySession::writer`] to send input.
pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    child_pid: Option<u32>,
}

impl PtySession {
    /// Open a PTY of the given size and spawn `target` (program + args + cwd)
    /// attached to its slave end. The child inherits the current process
    /// environment.
    pub fn spawn(target: &TargetCommand, rows: u16, cols: u16) -> Result<Self> {
        target.validate()?;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(pty_size(rows, cols))
            .context("failed to open PTY")?;

        let mut command = CommandBuilder::new(&target.program);
        command.args(&target.args);
        if let Some(cwd) = &target.cwd {
            command.cwd(cwd);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to spawn {}", target.program))?;
        let child_pid = child.process_id();
        // Drop the slave handle so the master reader sees EOF once every
        // process attached to the PTY exits.
        drop(pair.slave);

        Ok(Self {
            master: pair.master,
            child,
            child_pid,
        })
    }

    /// Clone a reader over the PTY master. Each call returns an independent
    /// reader; typically the frontend takes one and streams it to the UI.
    pub fn reader(&self) -> Result<Box<dyn Read + Send>> {
        self.master
            .try_clone_reader()
            .context("failed to clone PTY reader")
    }

    /// Take the writer for the PTY master so the frontend can forward input.
    pub fn writer(&self) -> Result<Box<dyn Write + Send>> {
        self.master
            .take_writer()
            .context("failed to take PTY writer")
    }

    /// Resize the PTY (e.g. after the GUI terminal widget reflows).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master
            .resize(pty_size(rows, cols))
            .context("failed to resize PTY")
    }

    /// The spawned child's process id, when known.
    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    /// Kill the child and its whole process group (so descendants attached to
    /// the PTY slave also go away and the master reader reaches EOF).
    pub fn kill(&mut self) -> Result<()> {
        kill_child_process_group(self.child_pid);
        self.child.kill().context("failed to kill child process")
    }

    /// Block until the child exits.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().context("child process wait failed")
    }

    /// Poll whether the child has exited without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .context("child process try_wait failed")
    }
}

fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}
