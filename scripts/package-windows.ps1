# Package the Windows portable bundle (run on Windows, or on any host via `cargo xwin`).
#
# This is the LOCAL packaging helper. The canonical, released artifact is built by
# the `build-windows` job in .github/workflows/release.yml — keep the file list
# below and the artifact name in sync with that job (it produces
# dist/DropBridge-Windows-x64.zip plus a matching .sha256).
#
# The bundle ships exactly five files: the engine, the tray, the installer, the
# uninstaller and the readme. The tray locates the engine next to itself, so the
# two binaries must always travel together.
[CmdletBinding()]
param(
    [string]$Profile = "release",
    # Pass -Target to cross-build from macOS/Linux (requires cargo-xwin).
    [string]$Target = ""
)

Set-Location (Join-Path $PSScriptRoot "..")
$ErrorActionPreference = "Stop"

$ArtifactName = "DropBridge-Windows-x64.zip"
$targetArgs = if ($Target) { @("--target", $Target) } else { @() }
$outDir = if ($Target) { "target/$Target/$Profile" } else { "target/$Profile" }

Write-Host "==> Building dropbridge-cli and dropbridge-tray ($Profile)"
# The shipped binaries must not depend on the VC++ Redistributable, otherwise a
# clean Windows 10/11 machine fails to start them with 0xC0000135
# (VCRUNTIME140.dll not found). .cargo/config.toml already sets
# -C target-feature=+crt-static for the Windows targets; when cross-building with
# cargo-xwin the flag travels with the same config, but keep the explicit
# RUSTFLAGS below so this script is correct even without it.
if ($Target -like "*-windows-*") {
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
}

cargo build "--$Profile" @targetArgs -p dropbridge-cli -p dropbridge-tray
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$engine = Join-Path $outDir "dropbridge.exe"
$tray = Join-Path $outDir "dropbridge-tray.exe"
foreach ($exe in @($engine, $tray)) {
    if (-not (Test-Path $exe)) { throw "missing $exe — the build did not produce it" }
}

Write-Host "==> Verifying the tray is a GUI-subsystem binary"
# A tray app must be PE subsystem 2 (WINDOWS_GUI). Subsystem 3 means a black
# console window sticks around for the whole process lifetime.
$pe = [System.IO.File]::ReadAllBytes($tray)
$peOffset = [System.BitConverter]::ToInt32($pe, 0x3C)
$subsystem = [System.BitConverter]::ToUInt16($pe, $peOffset + 0x5C)
if ($subsystem -ne 2) {
    throw "dropbridge-tray.exe has PE subsystem $subsystem, expected 2 (WINDOWS_GUI). Rebuild with a clean target dir."
}
Write-Host "    subsystem = 2 (WINDOWS_GUI) OK"

Write-Host "==> Checking the engine actually starts"
& $engine --version
if ($LASTEXITCODE -ne 0) { throw "dropbridge.exe --version failed with exit code $LASTEXITCODE" }

$stage = "dist/stage-windows"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item $engine $stage
Copy-Item $tray $stage
Copy-Item "dist/windows/install.ps1" $stage
Copy-Item "dist/windows/uninstall.ps1" $stage
Copy-Item "dist/windows/README.txt" $stage

$zip = "dist/$ArtifactName"
Compress-Archive -Path "$stage/*" -DestinationPath $zip -Force
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $ArtifactName" | Set-Content -Path "$zip.sha256" -Encoding ascii

Write-Host "==> Packaged $zip"
Write-Host "    sha256 $hash"
Write-Host "Run install.ps1 from a PowerShell window; see dist/windows/README.txt."
