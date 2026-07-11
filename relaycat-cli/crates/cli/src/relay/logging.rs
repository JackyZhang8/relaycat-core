use super::*;
use flate2::{Compression, write::GzEncoder};

pub(crate) struct LogSink {
    file: File,
    path: PathBuf,
    bytes_written: u64,
}

pub(crate) static LOG_FILE: OnceLock<Mutex<LogSink>> = OnceLock::new();

/// Size at which `cli.log` is rotated into a gzip archive (override with
/// `RELAYCAT_LOG_MAX_BYTES`). Archives older than `RELAYCAT_LOG_RETAIN_DAYS`
/// (default 7) are deleted, and at most [`MAX_LOG_ARCHIVES`] newest are kept.
const DEFAULT_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
const DEFAULT_LOG_RETAIN_DAYS: u64 = 7;
const MAX_LOG_ARCHIVES: usize = 10;

fn log_max_bytes() -> u64 {
    env_u64("RELAYCAT_LOG_MAX_BYTES").unwrap_or(DEFAULT_LOG_MAX_BYTES)
}

fn log_retain_days() -> u64 {
    env_u64("RELAYCAT_LOG_RETAIN_DAYS").unwrap_or(DEFAULT_LOG_RETAIN_DAYS)
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

pub(crate) fn init_log_file(project_dir: &Path) -> Option<PathBuf> {
    if LOG_FILE.get().is_some() {
        return None;
    }
    let dir = project_dir.join(RELAYCAT_DIR);
    fs::create_dir_all(&dir).ok()?;
    let path = dir.join("cli.log");
    // Rotation is a cheap rename here; compression and pruning run on a
    // background thread so startup (and the GUI hosting this CLI) never waits.
    if fs::metadata(&path).map(|m| m.len()).unwrap_or(0) >= log_max_bytes() {
        rotate_log_file(&path);
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    let bytes_written = file.metadata().map(|m| m.len()).unwrap_or(0);
    LOG_FILE
        .set(Mutex::new(LogSink {
            file,
            path: path.clone(),
            bytes_written,
        }))
        .ok()?;
    Some(path)
}

/// Rename the live log aside and hand it to a background thread that gzips it
/// and prunes old archives. Rename-only on the calling thread keeps rotation
/// effectively free for the writer.
fn rotate_log_file(path: &Path) {
    let Some(dir) = path.parent() else {
        return;
    };
    let archive = unique_archive_path(dir, &local_timestamp());
    if fs::rename(path, &archive).is_err() {
        return;
    }
    let dir = dir.to_path_buf();
    thread::spawn(move || {
        compress_log_archive(&archive);
        prune_log_archives(&dir, log_retain_days(), MAX_LOG_ARCHIVES);
    });
}

/// Archive name derived from the rotation time, e.g. `cli-20260703-134500.log`
/// (compressed to `.gz` in the background). A numeric suffix keeps it unique
/// if two rotations land in the same second.
pub(crate) fn unique_archive_path(dir: &Path, timestamp: &str) -> PathBuf {
    let stamp: String = timestamp
        .chars()
        .map(|c| if c == ' ' { '-' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let base = dir.join(format!("cli-{stamp}.log"));
    if !base.exists() && !base.with_extension("log.gz").exists() {
        return base;
    }
    for n in 1..100 {
        let candidate = dir.join(format!("cli-{stamp}-{n}.log"));
        if !candidate.exists() && !candidate.with_extension("log.gz").exists() {
            return candidate;
        }
    }
    base
}

fn compress_log_archive(archive: &Path) {
    let gz_path = archive.with_extension("log.gz");
    let Ok(mut input) = File::open(archive) else {
        return;
    };
    let Ok(output) = File::create(&gz_path) else {
        return;
    };
    let mut encoder = GzEncoder::new(output, Compression::default());
    if io::copy(&mut input, &mut encoder).is_err() || encoder.finish().is_err() {
        let _ = fs::remove_file(&gz_path);
        return;
    }
    let _ = fs::remove_file(archive);
}

/// Delete rotated archives (`cli-*.log.gz`, plus any `cli-*.log` left by a
/// failed compression) older than `retain_days`, and keep at most `max_kept`
/// newest even within the window.
pub(crate) fn prune_log_archives(dir: &Path, retain_days: u64, max_kept: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    let mut archives: Vec<(PathBuf, SystemTime)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !is_log_archive_name(name) {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    // Newest first; everything past `max_kept` or older than the window goes.
    archives.sort_by(|a, b| b.1.cmp(&a.1));
    let max_age = Duration::from_secs(retain_days * 24 * 60 * 60);
    for (index, (path, modified)) in archives.iter().enumerate() {
        let expired = now
            .duration_since(*modified)
            .map(|age| age > max_age)
            .unwrap_or(false);
        if index >= max_kept || expired {
            let _ = fs::remove_file(path);
        }
    }
}

pub(crate) fn is_log_archive_name(name: &str) -> bool {
    name != "cli.log"
        && name.starts_with("cli-")
        && (name.ends_with(".log.gz") || name.ends_with(".log"))
}

/// `DEBUG`-level lines are verbose diagnostics (e.g. the salt/proof presence of
/// an inbound `PeerJoined`) and are suppressed unless `RELAYCAT_DEBUG` is set to
/// a truthy value, so the log file stays quiet during normal pairing while the
/// detail is one env var away when triaging handshake failures.
pub(crate) fn relaycat_debug_enabled() -> bool {
    matches!(
        std::env::var("RELAYCAT_DEBUG")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(crate) fn relaycat_log(level: &str, message: impl AsRef<str>) {
    if level == "DEBUG" && !relaycat_debug_enabled() {
        return;
    }
    let line = format_relaycat_log_line(&local_timestamp(), level, message.as_ref());
    let Some(sink_mutex) = LOG_FILE.get() else {
        return;
    };
    let Ok(mut sink) = sink_mutex.lock() else {
        return;
    };
    if sink.bytes_written >= log_max_bytes() {
        // Rotate under the lock: rename the full log aside (compression and
        // pruning happen on a background thread) and continue in a fresh file.
        let live_path = sink.path.clone();
        rotate_log_file(&live_path);
        if let Ok(file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sink.path)
        {
            sink.file = file;
            sink.bytes_written = 0;
        }
    }
    let _ = writeln!(sink.file, "{line}");
    let _ = sink.file.flush();
    sink.bytes_written += line.len() as u64 + 1;
}

pub(crate) fn format_relaycat_log_line(timestamp: &str, level: &str, message: &str) -> String {
    format!("[{timestamp}] {level} relaycat: {message}")
}

/// Logs whether an inbound `PeerJoined` carried the per-connection salt and
/// proof. Under v3 a missing salt aborts the handshake, so this records what the
/// relay actually forwarded (i.e. what the peer's `Join` contained) to pinpoint
/// whether a stale peer/relay dropped the salt before the abort.
pub(crate) fn log_received_peer_joined(frame: &OuterFrame) {
    if let OuterFrame::PeerJoined {
        role,
        pairing_token_proof,
        connection_salt,
        ..
    } = frame
    {
        relaycat_log(
            "DEBUG",
            format!(
                "received peer_joined role={role:?} proof={} salt={}",
                if pairing_token_proof.is_some() {
                    "present"
                } else {
                    "absent"
                },
                if connection_salt.is_some() {
                    "present"
                } else {
                    "absent"
                },
            ),
        );
    }
}

pub(crate) fn format_byte_preview(bytes: &[u8]) -> String {
    const MAX_PREVIEW_BYTES: usize = 32;

    let mut preview = String::new();
    for (index, byte) in bytes.iter().take(MAX_PREVIEW_BYTES).enumerate() {
        if index > 0 {
            preview.push(' ');
        }
        preview.push_str(&format!("{byte:02X}"));
    }
    if bytes.len() > MAX_PREVIEW_BYTES {
        preview.push_str(" ...");
    }
    preview
}

pub(crate) fn local_timestamp() -> String {
    #[cfg(not(unix))]
    {
        return current_unix_timestamp().to_string();
    }

    #[cfg(unix)]
    {
        let mut now: libc::time_t = 0;
        unsafe {
            libc::time(&mut now);
        }

        let mut local = MaybeUninit::<libc::tm>::uninit();
        let local_ptr = unsafe { libc::localtime_r(&now, local.as_mut_ptr()) };
        if local_ptr.is_null() {
            return now.to_string();
        }

        let mut buffer = [0 as libc::c_char; 20];
        let written = unsafe {
            libc::strftime(
                buffer.as_mut_ptr(),
                buffer.len(),
                c"%Y-%m-%d %H:%M:%S".as_ptr(),
                local_ptr,
            )
        };
        if written == 0 {
            return now.to_string();
        }

        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
}
