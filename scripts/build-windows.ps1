# Build the Windows artifacts: engine CLI + tray.
#
# The tray lives in the same directory as the engine (it resolves the engine
# next to its own executable), so the two binaries are always built together.
# The shipped binaries must link the CRT statically — see .cargo/config.toml —
# otherwise a clean Windows 10/11 machine refuses to start them with 0xC0000135.
[CmdletBinding()]
param(
    [string]$Profile = "release",
    # Pass -Target to cross-build from macOS/Linux (requires cargo-xwin).
    [string]$Target = ""
)

Set-Location (Join-Path $PSScriptRoot "..")
$ErrorActionPreference = "Stop"

$targetArgs = if ($Target) { @("--target", $Target) } else { @() }
$outDir = if ($Target) { "target/$Target/$Profile" } else { "target/$Profile" }

if ($Target -like "*-windows-*") {
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
}

cargo build "--$Profile" @targetArgs -p dropbridge-cli -p dropbridge-tray
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "Artifacts:"
Get-ChildItem "$outDir/dropbridge*.exe" | Select-Object -ExpandProperty FullName
