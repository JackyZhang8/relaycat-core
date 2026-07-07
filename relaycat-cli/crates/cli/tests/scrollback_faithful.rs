//! Proves the CLI's deep scrollback is a *faithful* mirror of the vt100 parser's
//! own scrollback — exactly what the local terminal shows — rather than a
//! heuristically reconstructed copy that can amplify rows on resize.
//!
//! The previous incremental detector compared successive scrollback windows and,
//! when a TUI repaint broke the alignment, fell back to dumping the entire
//! window as "newly scrolled" rows. Across repeated reconnect / resize cycles
//! that amplification piled up huge blocks of duplicated lines. The redesign
//! derives history straight from vt100's append-only scrollback by position, so
//! the result must match an independent vt100 oracle byte for byte.

use relaycat_cli::terminal_core::{TerminalCore, TerminalCoreConfig};
use relaycat_protocol::{ResizeEventV2, TerminalRow};

const HISTORY_CAPACITY: usize = 20_000;

fn row_text(row: &TerminalRow) -> String {
    row.cells
        .iter()
        .flat_map(|run| run.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The vt100 scrollback the local terminal itself would retain for the same
/// byte stream and resizes, read oldest-first as trimmed text.
fn oracle_scrollback_texts(parser: &vt100::Parser) -> Vec<String> {
    let mut screen = parser.screen().clone();
    screen.set_scrollback(usize::MAX);
    let len = screen.scrollback();
    let (_rows, cols) = screen.size();
    let mut out = Vec::with_capacity(len);
    for offset in (1..=len).rev() {
        screen.set_scrollback(offset);
        let line: String = (0..cols)
            .map(|col| match screen.cell(0, col) {
                Some(cell) if cell.has_contents() => cell.contents().to_string(),
                _ => " ".to_string(),
            })
            .collect();
        out.push(line.trim_end().to_string());
    }
    out
}

fn core_scrollback_texts(core: &mut TerminalCore) -> Vec<String> {
    core.snapshot()
        .scrollback_window
        .iter()
        .map(row_text)
        .collect()
}

/// A scripted run: feed numbered lines, interleaved with resizes and TUI-style
/// repaints (re-emitting recently printed lines), applied identically to both
/// the `TerminalCore` and a bare vt100 oracle. The CLI history must equal the
/// oracle's scrollback regardless of how many resize cycles occur.
#[test]
fn core_scrollback_matches_vt100_oracle_through_resize_churn() {
    // Keep every line shorter than the narrowest width used so trailing-space
    // trimming makes width-at-scroll-time irrelevant to the text comparison.
    let mut core = TerminalCore::new(TerminalCoreConfig {
        terminal_run_id: "run-1".to_string(),
        cols: 16,
        rows: 4,
        patch_retention: 256,
    });
    let mut oracle = vt100::Parser::new(4, 16, HISTORY_CAPACITY);

    let mut resize_seq = 0;
    let feed = |core: &mut TerminalCore, oracle: &mut vt100::Parser, bytes: &[u8]| {
        core.feed_vt_bytes(bytes);
        oracle.process(bytes);
    };
    let mut resize = |core: &mut TerminalCore, oracle: &mut vt100::Parser, cols: u16, rows: u16| {
        resize_seq += 1;
        core.resize(ResizeEventV2 {
            resize_seq,
            cols,
            rows,
            input_stream_id: "s".to_string(),
            last_input_ack: 0,
        });
        oracle.screen_mut().set_size(rows, cols);
    };

    // Numbered "questions" like the user's 1-9 reproduction, with disconnect /
    // reconnect / background-foreground style resize churn and repaints in
    // between. Each cycle would have triggered the old whole-window dump.
    for q in 1..=9u32 {
        for line in 0..5u32 {
            feed(
                &mut core,
                &mut oracle,
                format!("q{q}-l{line}\r\n").as_bytes(),
            );
        }
        match q % 3 {
            0 => {
                // "disconnect": desktop reclaims a larger local size, TUI repaints.
                resize(&mut core, &mut oracle, 24, 6);
                feed(&mut core, &mut oracle, b"q-repaint-a\r\nq-repaint-b\r\n");
                // "reconnect": app resizes back to its own size, TUI repaints again.
                resize(&mut core, &mut oracle, 16, 4);
                feed(&mut core, &mut oracle, b"q-repaint-a\r\nq-repaint-b\r\n");
            }
            1 => {
                // a plain font-size change while connected.
                resize(&mut core, &mut oracle, 20, 5);
            }
            _ => {}
        }
    }

    let expected = oracle_scrollback_texts(&oracle);
    let actual = core_scrollback_texts(&mut core);

    // Snapshots intentionally ship only the latest twelve screens of scrollback.
    // The CLI history still tracks the same append-only order as vt100; the
    // snapshot window should therefore equal the oracle's newest rows.
    let expected = expected
        .into_iter()
        .rev()
        .take(usize::from(4_u16) * 12)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    assert_eq!(
        actual, expected,
        "CLI scrollback must faithfully mirror the vt100 oracle (no amplification, no loss)"
    );
}
