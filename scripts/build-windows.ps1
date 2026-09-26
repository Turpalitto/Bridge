# Build the Windows artifacts: engine CLI + tray.
param([string]$Profile = "release")

Set-Location (Join-Path $PSScriptRoot "..")

cargo build --$Profile -p dropbridge-cli
cargo build --$Profile -p dropbridge-tray

Write-Host "Artifacts:"
Get-ChildItem "target/$Profile/dropbridge*.exe" | Select-Object -ExpandProperty FullName
