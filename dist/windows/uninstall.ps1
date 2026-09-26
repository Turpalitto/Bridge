# DropBridge Windows 11 Uninstaller
# Removes registry entries, shortcuts, and installed files.

[CmdletBinding()]
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\DropBridge"
)

$ErrorActionPreference = "SilentlyContinue"

Write-Host "========================================" -ForegroundColor Yellow
Write-Host "     DropBridge — Удаление" -ForegroundColor Yellow
Write-Host "========================================" -ForegroundColor Yellow

# 1. Stop running processes
Stop-Process -Name "dropbridge-tray" -Force
Stop-Process -Name "dropbridge" -Force

# 2. Remove context menu entries
$Targets = @(
    "HKCU:\Software\Classes\*\shell\DropBridge",
    "HKCU:\Software\Classes\Directory\shell\DropBridge"
)
foreach ($key in $Targets) {
    if (Test-Path $key) {
        Remove-Item -Path $key -Recurse -Force
        Write-Host "[-] Удалён пункт контекстного меню: $key" -ForegroundColor Yellow
    }
}

# 3. Remove autostart entry
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
Remove-ItemProperty -Path $RunKey -Name "DropBridge" -Force
Write-Host "[-] Удалён автозапуск из реестра" -ForegroundColor Yellow

# 4. Remove Start Menu Shortcut
$StartMenuDir = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\DropBridge"
if (Test-Path $StartMenuDir) {
    Remove-Item -Path $StartMenuDir -Recurse -Force
    Write-Host "[-] Удалены ярлыки меню «Пуск»" -ForegroundColor Yellow
}

# 5. Remove from PATH
$UserPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
if ($UserPath -like "*$InstallDir*") {
    $NewPath = ($UserPath.Split(';') | Where-Object { $_ -ne $InstallDir -and $_ -ne "" }) -join ';'
    [Environment]::SetEnvironmentVariable("Path", $NewPath, [EnvironmentVariableTarget]::User)
    Write-Host "[-] Удалён из переменной окружения PATH" -ForegroundColor Yellow
}

# 6. Remove installed files
if (Test-Path $InstallDir) {
    Remove-Item -Path $InstallDir -Recurse -Force
    Write-Host "[-] Удалена директория приложения: $InstallDir" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "✓ DropBridge успешно удалён из системы." -ForegroundColor Green
