// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use relaycat_cli::gui_bridge::{RELAY_CHILD_ARG, run_relay_child};

fn main() {
    // Relay sessions re-execute this same binary with `RELAY_CHILD_ARG` so the
    // relay runs from the linked `relaycat-cli` source directly, rather than
    // locating a separate `relaycat` executable. On Windows the GUI hosts that
    // child over plain pipes (its stdio is already wired by the parent), so the
    // relay loop just needs a watchdog that takes the child — and the inner
    // shell it spawns — down when the GUI exits.
    let mut args = std::env::args();
    let _exe = args.next();
    if args.next().as_deref() == Some(RELAY_CHILD_ARG) {
        setup_relay_child_watchdog();
        let rest: Vec<String> = args.collect();
        match run_relay_child(&rest) {
            Ok(()) => std::process::exit(0),
            Err(err) => {
                eprintln!("relay session error: {err:#}");
                std::process::exit(1);
            }
        }
    }

    relaycat_gui_lib::run()
}

/// Make the relay child self-terminate when the GUI exits, taking its inner
/// shell with it.
///
/// On Windows a parent process exiting does NOT automatically kill its children
/// (unlike macOS/Linux, where the closed PTY delivers `SIGHUP`). Without this,
/// closing the GUI window would orphan the relay child: it keeps the secure
/// pairing alive, so the phone stays connected and can still run commands even
/// though there is no GUI anymore. We therefore (1) place this process and any
/// process it spawns in a job object that kills every member once our last
/// handle closes, and (2) watch the GUI process and tear the whole tree down
/// the moment it exits. On other platforms this is a no-op.
#[cfg(windows)]
fn setup_relay_child_watchdog() {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, WaitForSingleObject,
    };

    // SYNCHRONIZE access is enough to wait on the GUI process; INFINITE blocks
    // until it exits.
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const INFINITE: u32 = 0xFFFF_FFFF;

    // Only act when launched as the GUI's relay child (it always passes its PID).
    let Some(parent_pid) = std::env::var(relaycat_gui_lib::GUI_PARENT_PID_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
    else {
        return;
    };

    // 1) Put ourselves — and the inner shell we will spawn — in a job that kills
    //    every member when its last handle closes. The handle is intentionally
    //    leaked so it stays open for our whole lifetime; when this process exits
    //    the job closes and the inner shell is terminated with it.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if !job.is_null() {
        unsafe {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            AssignProcessToJobObject(job, GetCurrentProcess());
        }
    }

    // HANDLE is a raw pointer (not `Send`); pass it to the watchdog thread as an
    // integer.
    let job_addr = job as isize;
    std::thread::spawn(move || {
        let parent = unsafe { OpenProcess(SYNCHRONIZE, 0, parent_pid) };
        if !parent.is_null() {
            unsafe {
                WaitForSingleObject(parent, INFINITE);
                CloseHandle(parent);
            }
        }
        // The GUI has exited (or could not be opened): take this relay child and
        // its inner shell down. Terminating the job kills the inner shell;
        // exiting guarantees we go away even if the job could not be created.
        if job_addr != 0 {
            unsafe {
                TerminateJobObject(job_addr as HANDLE, 0);
            }
        }
        std::process::exit(0);
    });
}

#[cfg(not(windows))]
fn setup_relay_child_watchdog() {}
