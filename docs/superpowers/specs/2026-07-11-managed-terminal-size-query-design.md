# Managed terminal size query fix

## Problem

When the macOS desktop GUI launches Codex inside the relay PTY and a phone pairs, the PTY and shared terminal model are resized correctly to the phone grid. Codex initially renders at that width, then approximately one second later redraws using a slightly wider width.

Managed alternate-screen sessions currently do not have a deterministic source for terminal query responses. A cursor-position query can reach the host terminal in the baseline behavior, whose xterm.js grid does not necessarily match the inner PTY. The current uncommitted mitigation swallows that query, while CSI window-size queries such as `CSI 18 t` are also swallowed. Swallowing leaves Codex's query timeout and fallback behavior active, matching the delayed redraw.

## Scope

Change only the CLI output-filter boundary used by managed alternate-screen session kinds such as Codex and OpenCode. Do not change the relay protocol, shared terminal model, iOS/Android rendering, GUI sizing, or ordinary shell query behavior.

## Design

The output filter will track the current inner PTY columns as well as rows. For managed alternate-screen sessions:

- Consume `CSI 18 t` and feed `CSI 8 ; rows ; cols t` back into the child PTY using the current inner PTY dimensions.
- Consume CPR and DECXCPR queries and feed deterministic inner-terminal reports back into the child rather than forwarding them to the host or leaving them unanswered.
- Never copy these managed-session queries into either local display output or app-facing remote output.

For ordinary shell sessions, preserve the existing behavior: CPR may be forwarded to the real host terminal, while host-only CSI window queries remain filtered as they are today.

Unknown or unavailable PTY dimensions must not produce a fabricated size response. In that case the existing filtered behavior is retained.

## Data flow

1. The child writes a terminal query to its PTY output.
2. `LocalOutputFilter` recognizes the complete CSI sequence, including sequences split across PTY reads.
3. For a managed session, the filter builds a response from its current `pty_rows` and `pty_cols` and places it in `pty_input`.
4. The output thread writes `pty_input` back to the child PTY before displaying or forwarding the remaining child output.
5. Codex receives geometry belonging to the same PTY that constrains the shared model, so later redraws retain the negotiated phone width.

## Tests

Add focused output-filter tests before production changes:

- A managed filter at 48 columns by 35 rows answers `CSI 18 t` with `CSI 8 ; 35 ; 48 t` and emits no query bytes locally or remotely.
- The response still works when `CSI 18 t` is split across two filter calls.
- A managed filter answers CPR and DECXCPR directly without host output.
- A non-managed filter retains its existing CPR behavior.
- A managed filter with unknown dimensions does not fabricate a window-size response.

Run the focused CLI tests, then the complete CLI crate test suite and formatting/static checks available in the repository.

## Success criteria

- Codex receives only inner-PTY geometry responses after phone pairing.
- No host xterm geometry can enter the child through the handled query paths.
- The query timeout path is removed for handled managed-session queries.
- Existing shell and app-facing terminal semantics remain unchanged.
