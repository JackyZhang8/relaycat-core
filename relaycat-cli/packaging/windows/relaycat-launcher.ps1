# Launch the RelayCat TUI. Run by double-clicking (via "Run with PowerShell")
# or from a shortcut. Prefers $env:RELAYCAT_BIN, then relaycat.exe on PATH.
$ErrorActionPreference = 'Stop'

$bin = $env:RELAYCAT_BIN
if (-not $bin) {
    $cmd = Get-Command relaycat -ErrorAction SilentlyContinue
    if ($cmd) { $bin = $cmd.Source }
}
if (-not $bin) {
    Write-Error "relaycat.exe not found on PATH. Set RELAYCAT_BIN or add it to PATH."
    exit 1
}

& $bin tui
