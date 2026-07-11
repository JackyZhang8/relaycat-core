# RelayCat desktop packaging

These scripts wrap the `relaycat` binary so it can be launched like a regular
desktop app. Every wrapper simply opens a terminal and runs `relaycat tui`
(the interactive launcher).

Build the binary first:

```bash
cargo build --release -p relaycat-cli
# binary at target/release/relaycat
```

## Linux — `.desktop` launcher

```bash
packaging/linux/install.sh            # installs to ~/.local/share/applications
packaging/linux/install.sh --system   # installs to /usr/share/applications (needs sudo)
```

The installer resolves the absolute path of the `relaycat` binary (preferring
`target/release/relaycat`, then `$PATH`) and writes a validated
`relaycat.desktop` with `Terminal=true`, so the desktop environment opens a
terminal window running `relaycat tui`.

`packaging/linux/relaycat.desktop` is the template (uses a `@RELAYCAT_BIN@`
placeholder); the installer substitutes the real path.

## macOS — `.app` bundle

```bash
packaging/macos/build-app.sh                 # builds dist/RelayCat.app using `relaycat` on PATH
packaging/macos/build-app.sh /path/to/relaycat   # embed a specific binary
```

Double-clicking `RelayCat.app` opens Terminal.app and runs `relaycat tui`.
The bundle is unsigned; for distribution you must codesign and notarize it
(`codesign --deep --sign "Developer ID Application: …"` then `xcrun notarytool`).

> Not runtime-tested on this Linux build machine — verify on macOS.

## Windows — double-clickable launchers

- `packaging/windows/relaycat.bat` — double-click to open a console running
  `relaycat tui` (expects `relaycat` on `PATH`).
- `packaging/windows/relaycat-launcher.ps1` — PowerShell equivalent.
- `relaycat.exe` itself launches the TUI when run with no arguments, so a Start
  Menu / desktop shortcut to `relaycat.exe` also works.

> Not runtime-tested on this Linux build machine — verify on Windows.
