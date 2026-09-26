# DropBridge Windows 11 Installer
# Installs DropBridge into %LOCALAPPDATA%\DropBridge and configures:
#  1. Windows 11 Explorer Context Menu ("Отправить через DropBridge")
#  2. System Tray Background Launcher with Autostart
#  3. Start Menu Shortcuts and PATH registration
# Zero administrative privileges required (HKCU based).

[CmdletBinding()]
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\DropBridge"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "     DropBridge — Установка для Windows 11" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# 1. Create target directory
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Write-Host "[+] Создана директория: $InstallDir" -ForegroundColor Green
}

# 2. Copy binaries and assets
$SourceDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Binaries = @("dropbridge.exe", "dropbridge-tray.exe")

foreach ($bin in $Binaries) {
    $src = Join-Path $SourceDir $bin
    if (Test-Path $src) {
        Copy-Item -Path $src -Destination $InstallDir -Force
        Write-Host "[+] Скопирован: $bin" -ForegroundColor Green
    } else {
        Write-Warning "Файл $bin не найден в каталоге инсталлятора ($SourceDir). Будет использован заглушечный или уже установленный файл."
    }
}

# Copy sparse manifest if present
$manifestSrc = Join-Path $SourceDir "AppxManifest.xml"
if (Test-Path $manifestSrc) {
    Copy-Item -Path $manifestSrc -Destination $InstallDir -Force
}

$ExePath = Join-Path $InstallDir "dropbridge.exe"
$TrayPath = Join-Path $InstallDir "dropbridge-tray.exe"

# 3. Add to User PATH if not present
$UserPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", [EnvironmentVariableTarget]::User)
    Write-Host "[+] Добавлен в переменную окружения PATH пользователя" -ForegroundColor Green
}

# 4. Register Windows Explorer Context Menu ("Отправить через DropBridge")
$ContextMenuCmd = "`"$ExePath`" send auto `"%1`""
$Targets = @(
    "HKCU:\Software\Classes\*\shell\DropBridge",
    "HKCU:\Software\Classes\Directory\shell\DropBridge"
)

foreach ($key in $Targets) {
    New-Item -Path $key -Force | Out-Null
    Set-ItemProperty -Path $key -Name "(default)" -Value "Отправить через DropBridge"
    Set-ItemProperty -Path $key -Name "Icon" -Value "$ExePath"
    
    $cmdKey = "$key\command"
    New-Item -Path $cmdKey -Force | Out-Null
    Set-ItemProperty -Path $cmdKey -Name "(default)" -Value $ContextMenuCmd
}
Write-Host "[+] Контекстное меню Проводника Windows 11 успешно настроено" -ForegroundColor Green

# 5. Configure Autostart for DropBridge Tray
$RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
Set-ItemProperty -Path $RunKey -Name "DropBridge" -Value "`"$TrayPath`" --minimized" -Force
Write-Host "[+] Автозапуск системного трея настроен" -ForegroundColor Green

# 6. Create Start Menu Shortcut
$StartMenuDir = "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\DropBridge"
if (-not (Test-Path $StartMenuDir)) {
    New-Item -ItemType Directory -Path $StartMenuDir -Force | Out-Null
}

$WshShell = New-Object -ComObject WScript.Shell
$Shortcut = $WshShell.CreateShortcut("$StartMenuDir\DropBridge.lnk")
$Shortcut.TargetPath = $TrayPath
$Shortcut.WorkingDirectory = $InstallDir
$Shortcut.Description = "DropBridge — передача файлов между Android и Windows 11"
$Shortcut.IconLocation = "$ExePath,0"
$Shortcut.Save()
Write-Host "[+] Создан ярлык в меню «Пуск»" -ForegroundColor Green

# 7. Register Windows 11 Sparse Package for modern context menu if supported
if (Get-Command Add-AppxPackage -ErrorAction SilentlyContinue) {
    $manifestPath = Join-Path $InstallDir "AppxManifest.xml"
    if (Test-Path $manifestPath) {
        try {
            Add-AppxPackage -Register $manifestPath -ExternalLocation $InstallDir -ErrorAction Stop
            Write-Host "[+] Зарегистрирован разреженный пакет Windows 11 (компактное меню)" -ForegroundColor Green
        } catch {
            Write-Host "[*] Классическое контекстное меню активно (Sparse Package пропущен: $_)" -ForegroundColor Yellow
        }
    }
}

Write-Host ""
Write-Host "✓ Установка DropBridge успешно завершена!" -ForegroundColor Green
Write-Host "Для отправки файлов нажмите правой кнопкой мыши на файл/папку и выберите 'Отправить через DropBridge'."
