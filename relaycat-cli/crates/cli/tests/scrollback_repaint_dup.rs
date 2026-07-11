use relaycat_cli::terminal_core::{TerminalCore, TerminalCoreConfig};
use relaycat_protocol::{ResizeEventV2, TerminalRow};

fn row_text(row: &TerminalRow) -> String {
    row.cells
        .iter()
        .flat_map(|run| run.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn scrollback_texts(core: &mut TerminalCore) -> Vec<String> {
    core.snapshot()
        .scrollback_window
        .iter()
        .map(row_text)
        .filter(|t| !t.is_empty())
        .collect()
}

fn duplicate_counts(lines: &[String]) -> Vec<(String, usize)> {
    use std::collections::BTreeMap;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in lines {
        *counts.entry(line.clone()).or_default() += 1;
    }
    counts.into_iter().filter(|(_, n)| *n > 1).collect()
}

fn new_core() -> TerminalCore {
    TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run".to_string(),
        cols: 20,
        rows: 4,
        patch_retention: 256,
    })
}

fn feed_numbered(core: &mut TerminalCore) {
    let mut printed = String::new();
    for n in 1..=9u32 {
        printed.push_str(&format!("line {n}\r\n"));
    }
    let _ = core.feed_vt_bytes(printed.as_bytes());
}

fn resize(core: &mut TerminalCore, seq: u64, cols: u16, rows: u16) {
    let _ = core.resize(ResizeEventV2 {
        resize_seq: seq,
        cols,
        rows,
        input_stream_id: "s".to_string(),
        last_input_ack: 0,
    });
}

/// An idle CLI that is reconnected to repeatedly without any PTY resize keeps a
/// stable scrollback: no new bytes are produced, so nothing is appended and no
/// duplicates accumulate. This is the state the relay's disconnect debounce
/// preserves for background/foreground bounces (the size is held steady, so the
/// running TUI is never asked to repaint).
#[test]
fn steady_size_idle_reconnect_cycles_do_not_duplicate_scrollback() {
    let mut core = new_core();
    feed_numbered(&mut core);
    let baseline = scrollback_texts(&mut core);
    assert!(duplicate_counts(&baseline).is_empty());

    // Many "background/foreground" cycles: the app resumes against the unchanged
    // snapshot base and the CLI is idle, so there is no resize and no new output.
    for _ in 0..12 {
        let _ = core.feed_vt_bytes(b"");
    }

    let after = scrollback_texts(&mut core);
    assert_eq!(
        after, baseline,
        "idle reconnect cycles without resize must not change scrollback"
    );
    assert!(duplicate_counts(&after).is_empty());
}

/// Documents the mechanism the debounce avoids: when the PTY is resized, a TUI
/// repaints by re-emitting recently printed lines, and those re-emitted lines
/// are faithfully captured as fresh scrollback (the local terminal shows them
/// too). Each resize therefore adds another copy of the visible lines. The fix
/// is to stop the resize churn (relay-side debounce), not to second-guess the
/// byte stream here.
#[test]
fn resize_repaint_is_captured_faithfully_per_emission() {
    let mut core = new_core();
    feed_numbered(&mut core);
    let copies_of_line1 = |core: &mut TerminalCore| {
        scrollback_texts(core)
            .iter()
            .filter(|l| *l == "line 1")
            .count()
    };
    let before = copies_of_line1(&mut core);

    // A single resize followed by a TUI repaint of the same content.
    resize(&mut core, 1, 40, 8);
    feed_numbered(&mut core);
    for _ in 0..12 {
        let _ = core.feed_vt_bytes(b"");
    }
    let after = copies_of_line1(&mut core);

    assert!(
        after > before,
        "a resize repaint re-emits visible lines; the V2 model captures them \
         faithfully (no amplification, but also no fabricated dedup)"
    );
}
