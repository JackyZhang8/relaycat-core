use super::*;

pub(crate) fn process_exit_msg(status: &ExitStatus) -> PlainMsg {
    PlainMsg::ProcessExit {
        code: i32::try_from(status.exit_code()).ok(),
    }
}

pub(crate) fn plain_msg_fits_relay_budget(msg: &PlainMsg) -> Result<bool> {
    Ok(encode_plain_msg(msg)
        .context("failed to encode PlainMsg for relay budget check")?
        .len()
        <= RELAY_SAFE_PLAIN_MSG_BYTES)
}

pub(crate) fn collect_cli_status(pid: Option<u32>, process_name: &str) -> CliStatus {
    let (cpu_percent_x10, memory_bytes) =
        read_process_usage(std::process::id(), pid).unwrap_or((0, 0));
    CliStatus {
        cpu_percent_x10,
        memory_bytes,
        rx_bytes_per_sec: 0,
        tx_bytes_per_sec: 0,
        process_name: process_name.to_string(),
        collected_at_unix_ms: current_unix_millis(),
    }
}

pub(crate) fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

#[cfg(any(unix, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessUsageRow {
    pub(crate) pid: u32,
    pub(crate) parent_pid: u32,
    pub(crate) cpu_percent_x10: u16,
    pub(crate) rss_bytes: u64,
}

#[cfg(unix)]
pub(crate) fn read_process_usage(cli_pid: u32, target_pid: Option<u32>) -> Option<(u16, u64)> {
    let output = ProcessCommand::new("ps")
        .args([
            "-ax", "-o", "pid=", "-o", "ppid=", "-o", "%cpu=", "-o", "rss=",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let rows = parse_process_usage_table(&text);
    total_process_usage(cli_pid, target_pid, &rows)
}

#[cfg(unix)]
pub(crate) fn parse_process_usage_table(text: &str) -> Vec<ProcessUsageRow> {
    text.lines().filter_map(parse_process_usage_row).collect()
}

#[cfg(unix)]
pub(crate) fn parse_process_usage_row(line: &str) -> Option<ProcessUsageRow> {
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let parent_pid = parts.next()?.parse::<u32>().ok()?;
    let cpu_percent = parts.next()?.parse::<f32>().ok()?;
    let rss_kib = parts.next()?.parse::<u64>().ok()?;
    Some(ProcessUsageRow {
        pid,
        parent_pid,
        cpu_percent_x10: cpu_percent_to_x10(cpu_percent),
        rss_bytes: rss_kib.saturating_mul(1024),
    })
}

/// Windows has no `ps`. An earlier version shelled out to
/// `powershell.exe -ExecutionPolicy Bypass -Command <inline CIM script>` every
/// few seconds, but an unsigned binary repeatedly launching PowerShell to run
/// an inline script is a textbook Microsoft Defender behavioral/AMSI heuristic
/// and got the whole `relaycat.exe` quarantined as malware. Read the process
/// table through the native OS APIs instead (via `sysinfo`, which calls
/// `NtQuerySystemInformation`/toolhelp directly and never spawns a child
/// process), then reuse the shared subtree aggregation.
///
/// CPU usage is a delta between two samples, so we refresh, wait the library's
/// minimum interval, and refresh again before reading. `sysinfo`'s per-process
/// `cpu_usage()` can exceed 100% across cores, matching how Unix `%cpu` sums,
/// and `memory()` is the working set in bytes. This runs on a blocking task, so
/// the short sleep does not stall the relay pump.
#[cfg(windows)]
pub(crate) fn read_process_usage(cli_pid: u32, target_pid: Option<u32>) -> Option<(u16, u64)> {
    use sysinfo::{MINIMUM_CPU_UPDATE_INTERVAL, ProcessRefreshKind, ProcessesToUpdate, System};

    let mut system = System::new();
    let refresh = ProcessRefreshKind::new().with_cpu().with_memory();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);
    std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);

    let rows: Vec<ProcessUsageRow> = system
        .processes()
        .values()
        .map(|process| ProcessUsageRow {
            pid: process.pid().as_u32(),
            parent_pid: process.parent().map_or(0, sysinfo::Pid::as_u32),
            cpu_percent_x10: cpu_percent_to_x10(process.cpu_usage()),
            rss_bytes: process.memory(),
        })
        .collect();
    total_process_usage(cli_pid, target_pid, &rows)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn read_process_usage(_cli_pid: u32, _target_pid: Option<u32>) -> Option<(u16, u64)> {
    None
}

#[cfg(any(unix, windows))]
pub(crate) fn cpu_percent_to_x10(cpu_percent: f32) -> u16 {
    (cpu_percent * 10.0).round().clamp(0.0, f32::from(u16::MAX)) as u16
}

#[cfg(any(unix, windows))]
pub(crate) fn total_process_usage(
    cli_pid: u32,
    target_pid: Option<u32>,
    rows: &[ProcessUsageRow],
) -> Option<(u16, u64)> {
    let mut found = false;
    let mut cpu_percent_x10 = 0_u32;
    let mut rss_bytes = 0_u64;

    for row in rows {
        if row.pid != cli_pid
            && !target_pid.is_some_and(|pid| process_is_or_descends_from(row.pid, pid, rows))
        {
            continue;
        }
        found = true;
        cpu_percent_x10 = cpu_percent_x10.saturating_add(u32::from(row.cpu_percent_x10));
        rss_bytes = rss_bytes.saturating_add(row.rss_bytes);
    }

    found.then_some((cpu_percent_x10.min(u32::from(u16::MAX)) as u16, rss_bytes))
}

#[cfg(any(unix, windows))]
pub(crate) fn process_is_or_descends_from(pid: u32, root_pid: u32, rows: &[ProcessUsageRow]) -> bool {
    if pid == root_pid {
        return true;
    }

    let mut current = pid;
    for _ in 0..rows.len() {
        let Some(row) = rows.iter().find(|row| row.pid == current) else {
            return false;
        };
        if row.parent_pid == root_pid {
            return true;
        }
        if row.parent_pid == 0 || row.parent_pid == current {
            return false;
        }
        current = row.parent_pid;
    }

    false
}

