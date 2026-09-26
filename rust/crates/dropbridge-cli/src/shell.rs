//! Windows 11 Explorer context menu integration.
//!
//! Registers "Send via DropBridge" in the Windows shell for files and directories
//! under `HKCU\Software\Classes`, requiring zero administrative privileges.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

pub enum ShellAction {
    Install,
    Uninstall,
    Status,
}

pub fn handle_shell_integration(action: ShellAction) -> Result<()> {
    if !cfg!(target_os = "windows") {
        println!("Note: Windows shell integration is only applicable when running on Windows.");
        return Ok(());
    }

    match action {
        ShellAction::Install => install_shell_verbs(),
        ShellAction::Uninstall => uninstall_shell_verbs(),
        ShellAction::Status => check_shell_status(),
    }
}

fn current_exe_path() -> Result<PathBuf> {
    std::env::current_exe().context("failed to determine current executable path")
}

fn install_shell_verbs() -> Result<()> {
    let exe = current_exe_path()?;
    let exe_str = exe.to_string_lossy();
    let command_str = format!("\"{}\" send auto \"%1\"", exe_str);

    let targets = [
        r"HKCU\Software\Classes\*\shell\DropBridge",
        r"HKCU\Software\Classes\Directory\shell\DropBridge",
    ];

    for key in targets {
        // Set menu item display text
        run_reg(&["add", key, "/ve", "/d", "Отправить через DropBridge", "/f"])?;
        // Set menu item icon
        run_reg(&["add", key, "/v", "Icon", "/d", &exe_str, "/f"])?;
        // Set command
        let cmd_key = format!(r"{}\command", key);
        run_reg(&["add", &cmd_key, "/ve", "/d", &command_str, "/f"])?;
    }

    println!("Успешно зарегистрирован пункт 'Отправить через DropBridge' в контекстном меню Проводника Windows.");
    Ok(())
}

fn uninstall_shell_verbs() -> Result<()> {
    let targets = [
        r"HKCU\Software\Classes\*\shell\DropBridge",
        r"HKCU\Software\Classes\Directory\shell\DropBridge",
    ];

    for key in targets {
        let _ = run_reg(&["delete", key, "/f"]);
    }

    println!("Пункт 'Отправить через DropBridge' успешно удалён из контекстного меню Проводника Windows.");
    Ok(())
}

fn check_shell_status() -> Result<()> {
    let output = Command::new("reg")
        .args(["query", r"HKCU\Software\Classes\*\shell\DropBridge"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            println!("Интеграция с Проводником Windows: УСТАНОВЛЕНА");
        }
        _ => {
            println!("Интеграция с Проводником Windows: НЕ УСТАНОВЛЕНА");
        }
    }
    Ok(())
}

fn run_reg(args: &[&str]) -> Result<()> {
    let out = Command::new("reg")
        .args(args)
        .output()
        .context("failed to execute reg.exe")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("reg.exe failed: {}", err.trim());
    }
    Ok(())
}
