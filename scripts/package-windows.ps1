# Package the Windows portable bundle (run on Windows).
# Produces dropbridge-windows-x64.zip with engine + tray + docs.
param([string]$Profile = "release")
Set-Location (Join-Path $PSScriptRoot "..")

cargo build --$Profile -p dropbridge-cli
cargo build --$Profile -p dropbridge-tray

$stage = "dist/stage-windows"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null

Copy-Item "target/$Profile/dropbridge.exe" $stage
Copy-Item "target/$Profile/dropbridge-tray.exe" $stage
Copy-Item "README.md","docs/WINDOWS.md","docs/SECURITY.md","CHANGELOG.md" $stage

$zip = "dist/dropbridge-windows-x64.zip"
Compress-Archive -Path "$stage/*" -DestinationPath $zip -Force
Get-FileHash $zip -Algorithm SHA256 | Format-List
Write-Host "Packaged: $zip"
