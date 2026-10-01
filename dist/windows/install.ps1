# DropBridge Windows 11 Installer
# Installs DropBridge into %LOCALAPPDATA%\DropBridge and configures:
#  1. Windows 11 Explorer Context Menu ("Отправить через DropBridge")
#  2. System Tray Background Launcher with Autostart
#  3. Start Menu Shortcuts and PATH registration
# Zero administrative privileges required (HKCU based).
#
# Every optional step is executed inside its own try/catch: a failure is
# reported in the final summary instead of aborting the whole installation and
# leaving a half-configured system behind.
#
# TIP: if double-click / right-click "Run with PowerShell" does nothing,
# the usual cause is the execution policy. Start PowerShell manually and run:
#   powershell -ExecutionPolicy Bypass -File .\install.ps1

[CmdletBinding()]
param(
    [string]$InstallDir = "$env:LOCALAPPDATA\DropBridge"
)

$ErrorActionPreference = "Stop"

# Collected [step, message] pairs; printed at the end. A non-empty list means
# the app is installed but something is not configured.
$Problems = New-Object System.Collections.ArrayList

function Add-Problem {
    param([string]$Step, [string]$Message)
    [void]$Problems.Add(@{ Step = $Step; Message = $Message })
}

# Runs a script block, swallowing any terminating error, and records it.
function Invoke-Step {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$Body
    )
    try {
        & $Body
    } catch {
        Add-Problem -Step $Name -Message $_.Exception.Message
        Write-Host "[!] $Name — НЕ ВЫПОЛНЕНО: $($_.Exception.Message)" -ForegroundColor Yellow
    }
}

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "     DropBridge — Установка для Windows 11" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# ---------------------------------------------------------------------------
# 0. Sanity check: required binaries must be present next to this script.
# ---------------------------------------------------------------------------
$SourceDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RequiredBinaries = @("dropbridge.exe", "dropbridge-tray.exe")
$missing = $RequiredBinaries | Where-Object { -not (Test-Path (Join-Path $SourceDir $_)) }
if ($missing) {
    Write-Error "Отсутствуют обязательные файлы: $($missing -join ', '). Распакуйте архив ЦЕЛИКОМ и запустите install.ps1 из распакованной папки."
}

# ---------------------------------------------------------------------------
# 1. Create target directory
# ---------------------------------------------------------------------------
Invoke-Step -Name "Создание $InstallDir" -Body {
    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Write-Host "[+] Создана директория: $InstallDir" -ForegroundColor Green
    }
}

# ---------------------------------------------------------------------------
# 2. Copy binaries
# ---------------------------------------------------------------------------
Invoke-Step -Name "Копирование движка" -Body {
    foreach ($bin in $RequiredBinaries) {
        Copy-Item -Path (Join-Path $SourceDir $bin) -Destination $InstallDir -Force
        Write-Host "[+] Скопирован: $bin" -ForegroundColor Green
    }
}

$ExePath = Join-Path $InstallDir "dropbridge.exe"
$TrayPath = Join-Path $InstallDir "dropbridge-tray.exe"

# ---------------------------------------------------------------------------
# 2b. Hard gate: the engine must be able to start on this machine.
#     Everything below (context menu, autostart, shortcut) points at these
#     binaries, so continuing after a failed --version would only register a
#     broken program in Explorer and at logon.
# ---------------------------------------------------------------------------
Write-Host "[*] Проверка запуска dropbridge.exe…" -ForegroundColor Cyan
$probe = & $ExePath --version 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Error @"
dropbridge.exe не запустился (код $LASTEXITCODE): $probe

Установка остановлена, потому что остальные шаги регистрируют именно эти файлы.
Частые причины:
  * Windows заблокировала файл (SmartScreen / «Mark of the Web»):
    ПКМ по dropbridge.exe → Свойства → внизу «Разблокировать» → ОК.
  * Не установлен Visual C++ Redistributable — пересоберите архив: релизные
    сборки статически линкуют CRT, поэтому зависимости быть не должно.
  * Антивирус quarantine — проверьте карантин.
Повторите после устранения причины.
"@
    # $ErrorActionPreference="Stop" already turned the Write-Error above into a
    # terminating error; the explicit exit covers `-ErrorAction Continue`.
    exit 1
}

Write-Host "[+] Движок запускается: $probe" -ForegroundColor Green

# ---------------------------------------------------------------------------
# 3. Add to User PATH if not present
# ---------------------------------------------------------------------------
Invoke-Step -Name "Переменная PATH" -Body {
    $UserPath = [Environment]::GetEnvironmentVariable("Path", [EnvironmentVariableTarget]::User)
    if ($null -eq $UserPath) { $UserPath = "" }
    if (($UserPath -split ';') -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", [EnvironmentVariableTarget]::User)
        Write-Host "[+] Добавлен в переменную окружения PATH пользователя" -ForegroundColor Green
    } else {
        Write-Host "[=] $InstallDir уже есть в PATH" -ForegroundColor DarkGray
    }
}

# ---------------------------------------------------------------------------
# 4. Register the Windows 11 context menu.
#    Preferred path is the engine's own `shell install`, which knows both
#    registry surfaces (the modern CLSID one and the classic per-class one).
#    The PowerShell fallback below mirrors it, including the modern key —
#    without that key Windows 11 hides the item behind «Показать ещё».
# ---------------------------------------------------------------------------
$modernKey = "HKCU:\Software\Classes\CLSID\{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}\shell\DropBridge"
$verbTargets = @(
    "HKCU:\Software\Classes\*\shell\DropBridge",
    "HKCU:\Software\Classes\Directory\shell\DropBridge",
    "HKCU:\Software\Classes\Directory\Background\shell\DropBridge",
    $modernKey
)

Invoke-Step -Name "Контекстное меню Проводника" -Body {
    $shellOk = $false
    try {
        & $ExePath shell install
        $shellOk = ($LASTEXITCODE -eq 0)
    } catch {
        $shellOk = $false
    }

    if (-not $shellOk) {
        Write-Host "[*] Резервная регистрация напрямую в реестр…" -ForegroundColor Cyan
        $ContextMenuCmd = "`"$ExePath`" send auto `"%1`""
        foreach ($key in $verbTargets) {
            New-Item -Path $key -Force | Out-Null
            Set-Item -Path $key -Value "Отправить через DropBridge"
            Set-ItemProperty -Path $key -Name "Icon" -Value "$ExePath,0" -Force
            if ($key -eq $modernKey) {
                # Windows 11 compact menu: allow more than one selected item.
                Set-ItemProperty -Path $key -Name "MultiSelectModel" -Value "Player" -Force
            }
            $cmdKey = "$key\command"
            New-Item -Path $cmdKey -Force | Out-Null
            Set-Item -Path $cmdKey -Value $ContextMenuCmd
        }
    }
    Write-Host "[+] Контекстное меню зарегистрировано" -ForegroundColor Green
    Write-Host "    Windows 11: пункт виден сразу в компактном меню (правый клик по файлу/папке)." -ForegroundColor DarkGray
    Write-Host "    Для папки целиком: правый клик по пустому месту внутри папки." -ForegroundColor DarkGray
}

# ---------------------------------------------------------------------------
# 5. Autostart for the tray
# ---------------------------------------------------------------------------
Invoke-Step -Name "Автозапуск трея" -Body {
    $RunKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Run"
    Set-ItemProperty -Path $RunKey -Name "DropBridge" -Value "`"$TrayPath`"" -Force
    Write-Host "[+] Автозапуск системного трея настроен" -ForegroundColor Green
}

# ---------------------------------------------------------------------------
# 6. Start Menu shortcut
# ---------------------------------------------------------------------------
Invoke-Step -Name "Ярлык в меню «Пуск»" -Body {
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
}

# ---------------------------------------------------------------------------
# 7. Start the tray now so the user sees the icon immediately.
# ---------------------------------------------------------------------------
Invoke-Step -Name "Запуск трея" -Body {
    Start-Process -FilePath $TrayPath
    Start-Sleep -Milliseconds 800
    if ($Problems.Count -eq 0) {
        Write-Host "[+] DropBridge запущен (значок в системном трее)" -ForegroundColor Green
    }
}

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
Write-Host ""
if ($Problems.Count -eq 0) {
    Write-Host "✓ Установка DropBridge успешно завершена!" -ForegroundColor Green
    Write-Host "Отправка: правый клик по файлу или папке → «Отправить через DropBridge»."
    Write-Host "Приём файлов: $env:USERPROFILE\DropBridge\From Phone"
    Write-Host "Логи и состояние: $env:USERPROFILE\DropBridge\  (лог: logs\dropbridge.log)"
    exit 0
}

Write-Host "Установка завершена с ошибками — DropBridge установлен, но часть настроек не применена:" -ForegroundColor Yellow
foreach ($p in $Problems) {
    Write-Host "  * $($p.Step): $($p.Message)" -ForegroundColor Yellow
}
Write-Host ""
Write-Host "Движок и трей работают. Неудачные шаги можно повторить вручную:" -ForegroundColor Yellow
Write-Host "  контекстное меню : `"$ExePath`" shell install"
Write-Host "  проверка меню    : `"$ExePath`" shell status"
Write-Host "  удаление         : .\uninstall.ps1"
exit 1
