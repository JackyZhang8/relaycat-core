use crate::{ProjectRoot, WorkspaceServiceError};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use relaycat_protocol::{ShellDescriptor, ShellSnapshot, WorkspaceErrorCode};
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    env,
    io::{Read, Write},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
    },
    thread,
};

const OUTPUT_CHUNK: usize = 32 * 1024;
const OUTPUT_CHANNEL_CHUNKS: usize = 64;
static NEXT_SHELL_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellTransportEvent {
    Started(ShellDescriptor),
    Output { shell_id: String, bytes: Vec<u8> },
    Exit { shell_id: String, code: Option<i32> },
}

#[derive(Clone)]
pub struct ShellManager {
    root: ProjectRoot,
    shells: Arc<Mutex<HashMap<String, Arc<Shell>>>>,
    pending_events: Arc<Mutex<VecDeque<ShellTransportEvent>>>,
}

struct Shell {
    descriptor: Mutex<ShellDescriptor>,
    last_input_seq: Mutex<u64>,
    output_rx: Mutex<Receiver<Vec<u8>>>,
    transport_started: AtomicBool,
    transport_exit_sent: AtomicBool,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
}

impl ShellManager {
    pub fn new(root: ProjectRoot, _limit: u8) -> Self {
        Self {
            root,
            shells: Arc::new(Mutex::new(HashMap::new())),
            pending_events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn list(&self) -> Vec<ShellDescriptor> {
        let shells = self.shells.lock().expect("shell map poisoned");
        let mut list: Vec<_> = shells
            .values()
            .filter_map(|shell| {
                let descriptor = shell.descriptor.lock().expect("descriptor poisoned");
                (!descriptor.exited).then(|| descriptor.clone())
            })
            .collect();
        list.sort_by(|a, b| a.title.cmp(&b.title));
        list
    }

    pub fn active_shell_id(&self) -> Option<String> {
        self.shells.lock().ok()?.iter().find_map(|(id, shell)| {
            shell
                .descriptor
                .lock()
                .ok()
                .is_some_and(|descriptor| !descriptor.exited)
                .then(|| id.clone())
        })
    }

    pub fn create(&self, cols: u16, rows: u16) -> Result<ShellSnapshot, WorkspaceServiceError> {
        let mut shells = self
            .shells
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?;
        if shells.values().any(|shell| {
            shell
                .descriptor
                .lock()
                .map(|descriptor| !descriptor.exited)
                .unwrap_or(true)
        }) {
            return Err(WorkspaceServiceError::busy());
        }

        let numeric = NEXT_SHELL_ID.fetch_add(1, Ordering::Relaxed);
        let id = format!("shell-{numeric}");
        let cols = cols.max(1);
        let rows = rows.max(1);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(internal_error)?;
        let (program, args) = default_shell_command();
        let mut command = CommandBuilder::new(program);
        command.args(args);
        let cwd = windows_shell_cwd(self.root.path());
        command.cwd(cwd.as_ref());
        configure_shell_terminal_env(&mut command);
        let child = pair.slave.spawn_command(command).map_err(internal_error)?;
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().map_err(internal_error)?;
        let writer = pair.master.take_writer().map_err(internal_error)?;
        let descriptor = ShellDescriptor {
            shell_id: id.clone(),
            title: "Shell 1".to_string(),
            cols,
            rows,
            last_output_seq: 0,
            exited: false,
            exit_code: None,
        };
        let (output_tx, output_rx) = sync_channel(OUTPUT_CHANNEL_CHUNKS);
        let shell = Arc::new(Shell {
            descriptor: Mutex::new(descriptor.clone()),
            last_input_seq: Mutex::new(0),
            output_rx: Mutex::new(output_rx),
            transport_started: AtomicBool::new(false),
            transport_exit_sent: AtomicBool::new(false),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
        });
        let background = Arc::clone(&shell);
        thread::Builder::new()
            .name(format!("relaycat-{id}-reader"))
            .spawn(move || read_output(&mut reader, &background, output_tx))
            .map_err(WorkspaceServiceError::io)?;
        shells.insert(id, Arc::clone(&shell));

        Ok(ShellSnapshot {
            descriptor,
            first_output_seq: 0,
            last_output_seq: 0,
            bytes: Vec::new(),
            complete_screen: false,
        })
    }

    pub fn write_active(&self, bytes: Vec<u8>) -> Result<(), WorkspaceServiceError> {
        let shell = self.active_shell()?;
        let mut writer = shell
            .writer
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?;
        writer.write_all(&bytes).map_err(WorkspaceServiceError::io)?;
        writer.flush().map_err(WorkspaceServiceError::io)
    }

    pub fn resize_active(&self, cols: u16, rows: u16) -> Result<(), WorkspaceServiceError> {
        let id = self
            .active_shell_id()
            .ok_or_else(shell_not_found)?;
        self.resize(&id, cols, rows)
    }

    pub fn drain_transport_events(&self) -> Vec<ShellTransportEvent> {
        let mut events: Vec<ShellTransportEvent> = self
            .pending_events
            .lock()
            .map(|mut pending| pending.drain(..).collect())
            .unwrap_or_default();
        let mut shells: Vec<Arc<Shell>> = self
            .shells
            .lock()
            .map(|shells| shells.values().cloned().collect())
            .unwrap_or_default();
        shells.sort_by_key(|shell| {
            let exited = shell
                .descriptor
                .lock()
                .ok()
                .is_some_and(|descriptor| descriptor.exited);
            if exited {
                0
            } else if shell.transport_started.load(Ordering::Acquire) {
                1
            } else {
                2
            }
        });
        let mut exited_shell_ids = Vec::new();
        for shell in shells {
            let descriptor = match shell.descriptor.lock() {
                Ok(descriptor) => descriptor.clone(),
                Err(_) => continue,
            };
            if !shell.transport_started.swap(true, Ordering::AcqRel) {
                events.push(ShellTransportEvent::Started(descriptor.clone()));
            }
            if let Ok(receiver) = shell.output_rx.lock() {
                loop {
                    match receiver.try_recv() {
                        Ok(bytes) => events.push(ShellTransportEvent::Output {
                            shell_id: descriptor.shell_id.clone(),
                            bytes,
                        }),
                        Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                    }
                }
            }
            if descriptor.exited && !shell.transport_exit_sent.swap(true, Ordering::AcqRel) {
                events.push(ShellTransportEvent::Exit {
                    shell_id: descriptor.shell_id.clone(),
                    code: descriptor.exit_code,
                });
            }
            if descriptor.exited && shell.transport_exit_sent.load(Ordering::Acquire) {
                exited_shell_ids.push(descriptor.shell_id);
            }
        }
        if !exited_shell_ids.is_empty()
            && let Ok(mut shells) = self.shells.lock()
        {
            for shell_id in exited_shell_ids {
                if shells.get(&shell_id).is_some_and(|shell| {
                    shell.transport_exit_sent.load(Ordering::Acquire)
                        && shell
                            .descriptor
                            .lock()
                            .ok()
                            .is_some_and(|descriptor| descriptor.exited)
                }) {
                    shells.remove(&shell_id);
                }
            }
        }
        events
    }

    pub fn input(
        &self,
        id: &str,
        seq: u64,
        bytes: Vec<u8>,
    ) -> Result<(), WorkspaceServiceError> {
        let shell = self.shell(id)?;
        let mut last = shell
            .last_input_seq
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?;
        if seq <= *last {
            return Ok(());
        }
        if seq != last.saturating_add(1) {
            return Err(WorkspaceServiceError::invalid("shell input sequence gap"));
        }
        let mut writer = shell
            .writer
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?;
        writer.write_all(&bytes).map_err(WorkspaceServiceError::io)?;
        writer.flush().map_err(WorkspaceServiceError::io)?;
        *last = seq;
        Ok(())
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), WorkspaceServiceError> {
        let shell = self.shell(id)?;
        let cols = cols.max(1);
        let rows = rows.max(1);
        shell
            .master
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(internal_error)?;
        let mut descriptor = shell
            .descriptor
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?;
        descriptor.cols = cols;
        descriptor.rows = rows;
        Ok(())
    }

    pub fn close(&self, id: &str) -> Result<(), WorkspaceServiceError> {
        let shell = self
            .shells
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?
            .remove(id)
            .ok_or_else(shell_not_found)?;
        let _ = shell
            .child
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?
            .kill();
        if !shell.transport_exit_sent.swap(true, Ordering::AcqRel)
            && let Ok(mut pending) = self.pending_events.lock()
        {
            pending.push_back(ShellTransportEvent::Exit {
                shell_id: id.to_string(),
                code: shell
                    .descriptor
                    .lock()
                    .ok()
                    .and_then(|descriptor| descriptor.exit_code),
            });
        }
        Ok(())
    }

    pub fn close_all(&self) -> Result<(), WorkspaceServiceError> {
        let ids: Vec<String> = self
            .shells
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?
            .keys()
            .cloned()
            .collect();
        for id in ids {
            self.close(&id)?;
        }
        Ok(())
    }

    fn active_shell(&self) -> Result<Arc<Shell>, WorkspaceServiceError> {
        let id = self
            .active_shell_id()
            .ok_or_else(shell_not_found)?;
        self.shell(&id)
    }

    fn shell(&self, id: &str) -> Result<Arc<Shell>, WorkspaceServiceError> {
        self.shells
            .lock()
            .map_err(|_| WorkspaceServiceError::busy())?
            .get(id)
            .cloned()
            .ok_or_else(shell_not_found)
    }
}

impl Drop for ShellManager {
    fn drop(&mut self) {
        if Arc::strong_count(&self.shells) == 1 {
            let _ = self.close_all();
        }
    }
}

fn read_output(reader: &mut dyn Read, shell: &Arc<Shell>, output_tx: SyncSender<Vec<u8>>) {
    let mut buffer = vec![0_u8; OUTPUT_CHUNK];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => {
                if let Ok(mut descriptor) = shell.descriptor.lock() {
                    descriptor.exited = true;
                }
                break;
            }
            Ok(count) => {
                if let Ok(mut descriptor) = shell.descriptor.lock() {
                    descriptor.last_output_seq = descriptor.last_output_seq.saturating_add(1);
                } else {
                    break;
                }
                if output_tx.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        }
    }
}

fn default_shell() -> String {
    if cfg!(windows) {
        env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    } else {
        env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
}

fn default_shell_command() -> (String, Vec<String>) {
    let program = default_shell();
    #[cfg(windows)]
    let args = vec![
        "/D".to_string(),
        "/Q".to_string(),
        "/K".to_string(),
        "chcp 65001>nul".to_string(),
    ];
    #[cfg(not(windows))]
    let args = Vec::new();
    (program, args)
}

/// Establish the byte-level contract expected by xterm.js: VT escape
/// sequences and UTF-8 text, regardless of whether the GUI was launched from
/// a terminal, Finder/Explorer, an IDE or a desktop service.
pub fn configure_shell_terminal_env(command: &mut CommandBuilder) {
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env_remove("COLUMNS");
    command.env_remove("LINES");

    #[cfg(unix)]
    {
        let lc_all_is_utf8 = command
            .get_env("LC_ALL")
            .and_then(|value| value.to_str())
            .is_some_and(is_utf8_locale);
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .filter_map(|key| command.get_env(key).and_then(|value| value.to_str()))
            .find(|value| is_utf8_locale(value))
            .map(str::to_string)
            .unwrap_or_else(default_utf8_locale);

        if !lc_all_is_utf8 {
            command.env_remove("LC_ALL");
        }
        command.env("LANG", &locale);
        command.env("LC_CTYPE", &locale);
    }
}

#[cfg(unix)]
fn is_utf8_locale(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    normalized.contains("utf-8") || normalized.contains("utf8")
}

#[cfg(all(unix, target_os = "macos"))]
fn default_utf8_locale() -> String {
    "en_US.UTF-8".to_string()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_utf8_locale() -> String {
    "C.UTF-8".to_string()
}

/// `std::fs::canonicalize` returns local Windows paths in the extended form
/// (`\\?\C:\...`). Interactive `cmd.exe` treats that spelling like a UNC
/// working directory and falls back to the Windows directory. Use the normal
/// drive spelling for child shells while leaving real UNC and non-Windows
/// paths untouched.
fn windows_shell_cwd(path: &Path) -> Cow<'_, Path> {
    let Some(text) = path.to_str() else {
        return Cow::Borrowed(path);
    };
    let Some(legacy) = text.strip_prefix(r"\\?\") else {
        return Cow::Borrowed(path);
    };
    let bytes = legacy.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        Cow::Owned(legacy.into())
    } else {
        Cow::Borrowed(path)
    }
}

fn shell_not_found() -> WorkspaceServiceError {
    WorkspaceServiceError::new(WorkspaceErrorCode::NotFound, "shell was not found", false)
}

fn internal_error(error: impl ToString) -> WorkspaceServiceError {
    WorkspaceServiceError::new(WorkspaceErrorCode::Internal, error.to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::{configure_shell_terminal_env, windows_shell_cwd};
    #[cfg(windows)]
    use super::default_shell_command;
    use portable_pty::CommandBuilder;
    use std::path::Path;

    #[test]
    fn workspace_shell_configures_xterm_and_utf8_locale() {
        let mut command = CommandBuilder::new("shell");
        command.env("TERM", "");
        command.env("LANG", "C");
        command.env("LC_CTYPE", "POSIX");
        command.env("LC_ALL", "C");

        configure_shell_terminal_env(&mut command);

        assert_eq!(
            command.get_env("TERM").and_then(|value| value.to_str()),
            Some("xterm-256color")
        );
        #[cfg(unix)]
        {
            let locale = command
                .get_env("LC_CTYPE")
                .and_then(|value| value.to_str())
                .unwrap();
            assert!(locale.to_ascii_lowercase().contains("utf"));
            assert!(command.get_env("LC_ALL").is_none());
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_workspace_shell_switches_cmd_to_utf8_code_page() {
        let (_, args) = default_shell_command();
        assert!(args.iter().any(|arg| arg.contains("chcp 65001")));
    }

    #[test]
    fn windows_shell_cwd_removes_verbatim_disk_prefix() {
        assert_eq!(
            windows_shell_cwd(Path::new(r"\\?\C:\data")),
            Path::new(r"C:\data")
        );
    }

    #[test]
    fn windows_shell_cwd_preserves_regular_paths() {
        assert_eq!(
            windows_shell_cwd(Path::new(r"C:\data\relaycat")),
            Path::new(r"C:\data\relaycat")
        );
    }
}
