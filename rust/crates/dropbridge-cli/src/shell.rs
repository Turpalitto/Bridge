//! Windows Explorer context menu integration.
//!
//! Registers "Отправить через DropBridge" for files, folders and folder
//! backgrounds under `HKCU\Software\Classes`, requiring zero administrative
//! privileges.
//!
//! Two registration surfaces are written, because Windows 11 shows two menus:
//!
//! * the **modern** (compact) menu, which is a shell-extension slot identified
//!   by the well-known CLSID `{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}`, and
//! * the **classic** menu (`\*\shell`, `Directory\shell`,
//!   `Directory\Background\shell`), which Windows 11 hides behind
//!   «Показать ещё» / «Show more options».
//!
//! Writing only the classic keys is the single most common reason "the context
//! menu item does not appear" on Windows 11, so both are always installed.
//! The modern entry uses `MultiSelectModel=Player`, i.e. the verb receives the
//! item that was clicked rather than the whole multi-selection; a real
//! multi-select verb needs a COM `IExplorerCommand` server plus a sparse
//! package, which is out of scope for a zero-install CLI.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

/// Windows 11 compact context-menu slot for shell extensions.
const MODERN_CLSID: &str = r"{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}";

/// Classic (Windows 7 style) verb keys.
const CLASSIC_KEYS: &[&str] = &[
    r"HKCU\Software\Classes\*\shell\DropBridge",
    r"HKCU\Software\Classes\Directory\shell\DropBridge",
    r"HKCU\Software\Classes\Directory\Background\shell\DropBridge",
];

/// Modern (Windows 11 compact) verb key — one slot covers files and folders.
fn modern_key() -> String {
    format!(r"HKCU\Software\Classes\CLSID\{MODERN_CLSID}\shell\DropBridge")
}

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
    let exe_str = exe.to_string_lossy().into_owned();
    let command_str = format!("\"{}\" send auto \"%1\"", exe_str);
    let label = "Отправить через DropBridge";

    for key in CLASSIC_KEYS {
        write_verb(key, label, &exe_str, &command_str, false)?;
    }
    write_verb(&modern_key(), label, &exe_str, &command_str, true)?;

    println!("Контекстное меню Проводника: пункт «{label}» зарегистрирован.");
    println!("  • Windows 11 (компактное меню) — сразу в меню файла и папки.");
    println!("  • Windows 10/11 (классическое меню) — «Показать ещё» → {label}.");
    Ok(())
}

fn write_verb(key: &str, label: &str, icon: &str, command: &str, modern: bool) -> Result<()> {
    let cmd_key = format!(r"{key}\command");
    run_reg(&["add", key, "/ve", "/d", label, "/f"])?;
    run_reg(&["add", key, "/v", "Icon", "/d", icon, "/f"])?;
    if modern {
        // Player = the verb gets the clicked item only, which is all a plain
        // command line can consume (a multi-select would need a COM server).
        run_reg(&["add", key, "/v", "MultiSelectModel", "/d", "Player", "/f"])?;
    }
    run_reg(&["add", &cmd_key, "/ve", "/d", command, "/f"])
}

fn uninstall_shell_verbs() -> Result<()> {
    let mut keys: Vec<String> = CLASSIC_KEYS.iter().map(|k| (*k).to_string()).collect();
    keys.push(modern_key());
    for key in &keys {
        // `reg delete` on a missing key is not an error worth surfacing.
        let _ = run_reg(&["delete", key, "/f"]);
    }

    println!("Контекстное меню Проводника: пункт DropBridge удалён.");
    Ok(())
}

fn check_shell_status() -> Result<()> {
    let modern = reg_query(&modern_key());
    let classic: Vec<(&str, bool)> = CLASSIC_KEYS.iter().map(|k| (*k, reg_query(k))).collect();

    println!(
        "Интеграция с Проводником Windows: {}",
        if modern || classic.iter().any(|(_, ok)| *ok) {
            "УСТАНОВЛЕНА"
        } else {
            "НЕ УСТАНОВЛЕНА"
        }
    );
    println!("  меню Windows 11 (компактное): {}", yes_no(modern));
    for (key, ok) in classic {
        println!("  классическое меню {key}: {}", yes_no(ok));
    }
    if !modern {
        println!(
            "  подсказка: без записи в CLSID\\{MODERN_CLSID} пункт не появится в \
             компактном меню Windows 11 — выполните `dropbridge shell install`."
        );
    }
    Ok(())
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "есть"
    } else {
        "нет"
    }
}

fn reg_query(key: &str) -> bool {
    reg_command(&["query", key])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_reg(args: &[&str]) -> Result<()> {
    let out = reg_command(args)
        .output()
        .context("failed to execute reg.exe")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("reg.exe {} failed: {}", args.join(" "), err.trim());
    }
    Ok(())
}

fn reg_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("reg");
    cmd.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: otherwise every reg.exe call flashes a console
        // window when the verb is launched from Explorer.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}
