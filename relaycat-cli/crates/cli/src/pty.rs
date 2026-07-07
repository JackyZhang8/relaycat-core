use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::{
    command::TargetCommand,
    relay::{
        LocalInterruptAction, current_terminal_size, join_pty_output_thread,
        kill_child_process_group, local_interrupt_action_for_session,
    },
};

pub fn run_interactive(target: TargetCommand) -> Result<()> {
    target.validate()?;

    let pty_system = native_pty_system();
    let pty_size = current_terminal_size().unwrap_or(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    });
    let pair = pty_system.openpty(pty_size).context("failed to open PTY")?;

    let mut command = CommandBuilder::new(&target.program);
    command.args(&target.args);
    if let Some(cwd) = &target.cwd {
        command.cwd(cwd);
    }

    let mut child = pair
        .slave
        .spawn_command(command)
        .with_context(|| format!("failed to spawn {}", target.program))?;
    let child_pid = child.process_id();
    let child_killer = Arc::new(Mutex::new(child.clone_killer()));
    drop(pair.slave);

    let stdin_is_terminal = io::stdin().is_terminal();
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;

    let master = Arc::new(Mutex::new(Some(pair.master)));
    let master_for_input = master.clone();

    let output_thread = thread::spawn(move || -> io::Result<()> {
        let mut stdout = io::stdout().lock();
        io::copy(&mut reader, &mut stdout)?;
        stdout.flush()
    });

    // Signals the stdin-forwarding thread to stop once the child exits so it
    // stops consuming stdin before control returns to the caller (the TUI
    // launcher loop needs to read keyboard input again afterwards).
    let stop_input = Arc::new(AtomicBool::new(false));

    let input_thread = if stdin_is_terminal {
        let mut writer = master
            .lock()
            .map_err(|_| io::Error::other("pty master lock poisoned"))?
            .as_mut()
            .ok_or_else(|| io::Error::other("pty master closed"))?
            .take_writer()?;
        let child_killer = child_killer.clone();
        let child_pid_for_input = child_pid;
        let interrupt_session_kind = target.session_kind.clone();
        let stop_input = stop_input.clone();

        Some(thread::spawn(move || -> io::Result<()> {
            let _nonblocking = StdinNonblockingGuard::enable();
            let mut buffer = [0_u8; 8192];
            let mut last_interrupt_at = None;

            while !stop_input.load(Ordering::Relaxed) {
                let read = match read_stdin(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(err) => return Err(err),
                };

                let chunk = &buffer[..read];
                if chunk.contains(&0x03) {
                    let now = Instant::now();
                    match local_interrupt_action_for_session(
                        interrupt_session_kind.clone(),
                        last_interrupt_at,
                        now,
                    ) {
                        LocalInterruptAction::Exit => {
                            kill_child_process_group(child_pid_for_input);
                            if let Ok(mut guard) = child_killer.lock() {
                                let _ = guard.kill();
                            }
                            if let Ok(mut guard) = master_for_input.lock() {
                                guard.take();
                            }
                            break;
                        }
                        LocalInterruptAction::Forward => {
                            last_interrupt_at = Some(now);
                        }
                    }
                }
                writer.write_all(chunk)?;
                writer.flush()?;
            }

            Ok(())
        }))
    } else {
        None
    };

    let _status = child.wait().context("child process wait failed")?;
    stop_input.store(true, Ordering::Relaxed);
    // Reap any descendant still attached to the PTY slave so the master reader
    // hits EOF; otherwise the output-thread join below could block forever.
    kill_child_process_group(child_pid);
    if let Ok(mut guard) = master.lock() {
        guard.take();
    }

    // Join the input thread before returning so it has stopped reading and
    // restored stdin's blocking mode for the next reader (e.g. the launcher).
    if let Some(input_thread) = input_thread {
        let _ = input_thread.join();
    }
    join_pty_output_thread(output_thread);

    Ok(())
}

/// Read whatever is currently available from stdin. On Unix this is a
/// non-blocking `read(2)` (stdin is switched to non-blocking mode by
/// [`StdinNonblockingGuard`]) so the caller can poll a stop flag instead of
/// blocking forever; other platforms fall back to a blocking read.
#[cfg(unix)]
fn read_stdin(buffer: &mut [u8]) -> io::Result<usize> {
    use std::os::unix::io::AsRawFd;

    let fd = io::stdin().as_raw_fd();
    // SAFETY: `buffer` is valid writable memory for `buffer.len()` bytes and
    // `fd` is the process stdin descriptor.
    let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
    if read < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(read as usize)
    }
}

#[cfg(not(unix))]
fn read_stdin(buffer: &mut [u8]) -> io::Result<usize> {
    use std::io::Read;

    io::stdin().lock().read(buffer)
}

/// Puts stdin into non-blocking mode for the guard's lifetime and restores the
/// original flags on drop, so the forwarding loop can poll a stop flag instead
/// of blocking forever inside `read`.
#[cfg(unix)]
struct StdinNonblockingGuard {
    fd: i32,
    flags: i32,
}

#[cfg(unix)]
impl StdinNonblockingGuard {
    fn enable() -> Self {
        use std::os::unix::io::AsRawFd;

        let fd = io::stdin().as_raw_fd();
        // SAFETY: `fd` is the process stdin descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            // SAFETY: re-applying the queried flags plus O_NONBLOCK to stdin.
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        }
        Self { fd, flags }
    }
}

#[cfg(unix)]
impl Drop for StdinNonblockingGuard {
    fn drop(&mut self) {
        if self.flags >= 0 {
            // SAFETY: restoring the flags originally read from this fd.
            unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.flags) };
        }
    }
}

#[cfg(not(unix))]
struct StdinNonblockingGuard;

#[cfg(not(unix))]
impl StdinNonblockingGuard {
    fn enable() -> Self {
        Self
    }
}
