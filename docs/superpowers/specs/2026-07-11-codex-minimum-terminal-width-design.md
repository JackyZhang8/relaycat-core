# Codex Minimum Terminal Width Compatibility Design

## Problem

Codex CLI 0.144.1 renders its live TUI correctly in a 48-column PTY, but after
model initialization its inline-history insertion renders the welcome card at
an internal minimum width of approximately 52 columns. In a 48-column PTY the
last four cells wrap onto the following row. RelayCat faithfully mirrors those
PTY cells, so the same broken border appears in the desktop GUI, iOS, and
Android.

The behavior reproduces when Codex is launched directly in an independent
48x35 macOS PTY. RelayCat's PTY `TIOCGWINSZ`, semantic model size, resize
events, and terminal-query answers remain at 48 columns, so this is an upstream
Codex narrow-terminal limitation rather than a RelayCat size propagation bug.

## Goals

- Give Codex at least 52 terminal columns so its inline-history welcome card
  does not wrap.
- Keep the desktop GUI and paired mobile app on one identical terminal grid.
- Preserve the user's configured mobile terminal font size whenever it fits.
- Reduce only the rendered mobile font size when necessary to fit the incoming
  grid without horizontal clipping or scrolling.
- Leave all non-Codex session sizing and rendering unchanged.

## Non-goals

- Rewriting or recognizing Codex ANSI welcome-card output.
- Changing the user's persisted font-size setting.
- Applying a global 52-column minimum to Claude, OpenCode, shell, or custom
  sessions.
- Working around arbitrary future minimum widths from other terminal tools.

## Architecture

### Core terminal sizing

The CLI introduces a Codex-only minimum remote width of 52 columns. When an
app reports fewer than 52 columns for a Codex session, the effective PTY and
semantic-model width become 52. Reported widths of 52 or more pass through
unchanged. Host-width clamping remains authoritative: RelayCat must not request
more columns than the desktop host terminal can provide.

The minimum is applied at the session-aware effective-size boundary, before
the PTY resize and terminal-model resize are emitted. This keeps the child PTY,
GUI, snapshots, patches, and mobile renderer on the same column count.

Non-Codex session kinds retain the existing `min(app, host)` behavior.

### Mobile fit-to-grid font size

iOS and Android each add a pure policy that derives a rendered terminal font
size from:

- the user's configured font size;
- available viewport width after horizontal padding;
- measured cell width at the configured font size; and
- the incoming semantic terminal column count.

If the configured font fits, the rendered size is unchanged. Otherwise:

```text
fit ratio = usable viewport width / (terminal columns * configured cell width)
rendered font size = configured font size * fit ratio
```

The ratio is capped at 1 so the compatibility path never enlarges text. A
small positive lower bound prevents invalid font metrics in pathological
layouts. The calculated size is view-local and is not written to settings.

The effective metrics are used consistently for row rendering, content width,
selection geometry, scroll calculations, and resize reporting. Once the
viewport becomes wide enough, the rendered size returns automatically to the
configured size.

This policy is grid-driven rather than explicitly Codex-driven on mobile: a
52-column snapshot that must fit is rendered to fit. Other sessions normally
never receive the Codex-only minimum, and any legitimately wider grid also
benefits from avoiding accidental horizontal clipping.

### Desktop GUI

No GUI rendering change is required. The CLI publishes the 52-column remote
grid through the existing GUI remote-size signal. Desktop windows have enough
horizontal space to display it at their normal terminal font metrics, while
the GUI and apps continue consuming identical terminal state.

## Data Flow

1. The mobile app measures its viewport and reports its natural grid, such as
   48x36.
2. The CLI identifies the session as Codex and computes an effective width of
   `max(48, 52)`, still bounded by the host terminal width.
3. The CLI resizes the child PTY and semantic terminal model to 52 columns.
4. Codex renders its 52-column minimum welcome history without wrapping.
5. The GUI renders the shared 52-column grid normally.
6. The mobile app receives a 52-column snapshot/patch and reduces only its
   rendered font metrics enough to fit all columns in the viewport.

## Edge Cases

- If the host terminal is narrower than 52 columns, the host clamp wins; the
  CLI must not claim unavailable space.
- A phone already capable of 52 or more columns keeps the configured font size.
- Rotation, split-screen changes, and font-setting changes recompute the
  rendered size from current geometry.
- Zero or unavailable viewport metrics fall back to the configured font size
  until valid geometry is available.
- Rows are never increased by this compatibility rule; only columns change.

## Testing

### relaycat-core

- Codex with an app width below 52 yields 52 when the host can provide it.
- Codex widths at or above 52 pass through.
- A host narrower than 52 remains the upper bound.
- Claude, OpenCode, shell, and custom kinds preserve existing widths.
- PTY and semantic-model dimensions remain identical after the adjustment.

### iOS

- A 52-column grid wider than the viewport produces a smaller rendered font.
- A grid that fits keeps the configured font unchanged.
- Increasing viewport width restores the configured font.
- Invalid geometry falls back safely to the configured font.

### Android

- Mirror the iOS fit, no-op, restore, and invalid-geometry cases.

## Acceptance Criteria

- On a phone whose natural report is 48 columns, a newly paired Codex 0.144.1
  session uses a 52-column PTY/model.
- The welcome-card top and bottom borders remain on one row in both GUI and
  mobile app after model initialization.
- No horizontal scrolling or clipping is required to see column 52 on mobile.
- The mobile font is reduced only as much as needed and the stored preference
  is unchanged.
- Existing core, iOS, and Android test suites pass.
