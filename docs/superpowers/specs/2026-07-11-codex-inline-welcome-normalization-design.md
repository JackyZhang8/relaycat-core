# Codex Inline Welcome Card Normalization Design

## Problem

Codex CLI 0.144.1 initially renders correctly at the terminal's real width.
After model initialization, its inline-history insertion writes the welcome
card as a logical line approximately 52 cells wide even when the PTY is
narrower. A terminal correctly autowraps those bytes, producing fragments such
as `───╮`, `change │`, and `───╯` on following rows.

The PTY, GUI terminal, semantic model, iOS app, and Android app already agree on
the phone's measured size. The fix must preserve that size. No platform has a
fixed column count: the reported width varies with device, orientation,
available viewport, selected font, and measured font metrics.

## Goals

- Keep the app-reported terminal columns unchanged end to end.
- Normalize only the affected Codex inline welcome card to the current PTY
  width before any terminal emulator sees it.
- Send identical normalized bytes to the desktop GUI and semantic model so all
  clients remain consistent.
- Support arbitrary valid phone widths rather than hard-coded iOS or Android
  values.
- Leave subsequent Codex output and all other session kinds byte-for-byte
  unchanged.

## Non-goals

- Raising the PTY or model to Codex's internal minimum width.
- Shrinking mobile fonts to display a synthetic wider grid.
- Rewriting general long terminal output or changing normal autowrap behavior.
- Maintaining a complete parser for every possible Codex screen.

## Architecture

### Stream placement

Add a stateful Codex welcome-card normalizer in the CLI output path before
`LocalOutputFilter`. The output thread already refreshes the current PTY column
count before processing every read. The normalizer receives that dynamic value
and returns one byte stream that is then passed through the existing local and
remote filtering paths.

Only built-in Codex sessions enable the normalizer. OpenCode, Claude, shell,
custom tools, and generic terminal output bypass it.

### Detection

The normalizer works on explicit output lines, not terminal-emulator wrapped
rows. PTYs do not insert newline bytes when a line exceeds the screen; the
terminal emulator performs that wrap later. Therefore the filter can repair
the logical line before wrapping occurs.

Detection is conservative:

1. Buffer a candidate when a logical line's visible text begins with the
   rounded top-left border `╭`.
2. Confirm the candidate only when the following bounded group contains
   `OpenAI Codex` and ends with a logical line beginning with `╰`.
3. If confirmation fails, the byte limit is exceeded, or the group is
   incomplete beyond the bounded candidate window, flush the original bytes
   unchanged.

The candidate buffer is capped so arbitrary application output cannot cause
unbounded memory growth or indefinitely delay the stream.

### ANSI-aware width normalization

For a confirmed card, normalize each box line to the current PTY width:

- Top border: `╭` + exactly `cols - 2` horizontal cells + `╮`.
- Bottom border: `╰` + exactly `cols - 2` horizontal cells + `╯`.
- Content rows: reserve column 1 for `│` and the final column for `│`.
  Preserve visible content and ANSI styling up to the available interior
  width, truncate excess content on a Unicode cell boundary, pad shorter rows,
  then emit the right border.
- Preserve safe SGR styling transitions while ignoring zero-width control
  sequences for display-width accounting.
- Never split UTF-8 or double-width terminal cells.

Widths too small to form a box fall back to the original bytes. Normal RelayCat
minimum terminal validation means this path normally receives much larger
values.

### Chunking

The normalizer retains only the partial logical line and bounded candidate card
between PTY reads. It must behave identically whether the entire card arrives
in one read or every escape sequence/UTF-8 character is split across reads.

### Mobile and GUI behavior

No iOS, Android, or GUI layout change is required. Each app continues to report
its measured viewport size. The CLI keeps the PTY and semantic model at that
size. Both desktop and mobile receive a welcome card already formatted for the
actual negotiated width.

## Failure Handling

- Unrecognized or changed Codex output passes through unchanged.
- Invalid UTF-8 outside recognized printable text remains byte-preserved.
- Candidate overflow flushes original bytes and resets detection state.
- A PTY resize during a buffered candidate uses the latest width when the card
  is emitted, matching the grid that will consume it.

## Testing

Unit tests cover:

- A 52-cell Codex welcome card normalized to 48 columns.
- The same card normalized dynamically to 50 and 60 columns.
- ANSI-styled model/version text with safe truncation and a visible right
  border in the final column.
- UTF-8 and double-width content without byte or cell splitting.
- Card data split across arbitrary PTY reads.
- An unrelated rounded-border box passing through unchanged.
- A malformed or oversized candidate passing through unchanged.
- Non-Codex sessions bypassing normalization.
- Output after the bottom border remaining byte-for-byte unchanged.

The complete CLI suite must continue to pass. No mobile code changes are
required, but existing iOS and Android terminal-size tests verify that their
measured widths remain unchanged.

## Acceptance Criteria

- Logs continue to show the phone's true reported and effective width, such as
  48, 50, or any other measured value; there is no forced 52-column grid.
- After model initialization, the Codex welcome card top and bottom borders
  remain on one row at the negotiated width.
- The right border occupies the last visible terminal column in both GUI and
  app.
- Later Codex conversation output preserves normal terminal wrapping.
- iOS and Android require no horizontal scrolling or hidden extra columns.
