@echo off
REM Double-click to launch the RelayCat TUI in a console window.
REM Expects relaycat.exe on PATH; otherwise set RELAYCAT_BIN below.

if defined RELAYCAT_BIN (
    "%RELAYCAT_BIN%" tui
) else (
    relaycat tui
)
