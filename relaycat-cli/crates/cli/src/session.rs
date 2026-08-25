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
use relaycat_workspace::configure_shell_terminal_env;

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
        Self::spawn_with_envs(target, rows, cols, &[])
    }

    /// Like [`PtySession::spawn`], with additional environment variables set on
    /// the child (e.g. the GUI host's per-tab session id for relay children).
    pub fn spawn_with_envs(
        target: &TargetCommand,
        rows: u16,
        cols: u16,
        envs: &[(&str, String)],
    ) -> Result<Self> {
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
        for (key, value) in envs {
            command.env(key, value);
        }
        configure_shell_terminal_env(&mut command);

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

#[cfg(test)]
mod tests {
    use super::PtySession;
    use crate::command::{SessionKind, TargetCommand};
    use std::io::Read;

    #[cfg(unix)]
    #[test]
    fn pty_child_receives_xterm_and_utf8_locale_even_from_desktop_style_env() {
        let target = TargetCommand {
            program: "/usr/bin/env".to_string(),
            args: Vec::new(),
            cwd: None,
            relay: None,
            session_kind: SessionKind::shell(),
        };
        let mut session = PtySession::spawn_with_envs(
            &target,
            24,
            80,
            &[
                ("TERM", String::new()),
                ("LANG", "C".to_string()),
                ("LC_CTYPE", "POSIX".to_string()),
                ("LC_ALL", "C".to_string()),
            ],
        )
        .unwrap();
        let mut output = Vec::new();
        session.reader().unwrap().read_to_end(&mut output).unwrap();
        session.wait().unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("TERM=xterm-256color"));
        assert!(
            output.lines().any(|line| {
                line.strip_prefix("LC_CTYPE=")
                    .is_some_and(|value| value.to_ascii_lowercase().contains("utf"))
            }),
            "LC_CTYPE should explicitly select UTF-8: {output}"
        );
        assert!(
            !output
                .lines()
                .any(|line| line == "LC_ALL=C" || line == "LC_ALL=POSIX"),
            "LC_ALL must not override the UTF-8 character locale: {output}"
        );
    }
}
