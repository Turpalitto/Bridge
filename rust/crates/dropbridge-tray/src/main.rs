//! DropBridge Windows tray resident (spec §31).
//!
//! Responsibilities:
//! * live in the system tray with the DropBridge menu,
//! * supervise the `dropbridge daemon` process (the actual engine),
//! * open the receive/send folders,
//! * (un)register autostart (spec §71) — no admin rights needed,
//! * balloon notifications for completed receives (spec §73; richer toasts
//!   are tracked as a Phase-6 item in docs/WINDOWS.md).
//!
//! All file transfer logic lives in the core binary — the tray never touches
//! file bytes or keys.

#[cfg(not(windows))]
fn main() {
    eprintln!(
        "dropbridge-tray is a Windows shell; on this platform run `dropbridge daemon` directly."
    );
}

#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use std::process::{Child, Command};

    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};

    const ID_SEND: &str = "send";
    const ID_OPEN_RECEIVED: &str = "open_received";
    const ID_OPEN_TOPHONE: &str = "open_tophone";
    const ID_DEVICES: &str = "devices";
    const ID_AUTOSTART: &str = "autostart";
    const ID_PAUSE: &str = "pause";
    const ID_QUIT: &str = "quit";

    pub fn core_binary() -> PathBuf {
        // The core binary ships next to the tray executable.
        let exe = std::env::current_exe().unwrap_or_default();
        let dir = exe
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        let cand = dir.join("dropbridge.exe");
        if cand.exists() {
            cand
        } else {
            PathBuf::from("dropbridge.exe")
        }
    }

    pub fn user_dir(sub: &str) -> PathBuf {
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into());
        let p = PathBuf::from(home).join("DropBridge").join(sub);
        let _ = std::fs::create_dir_all(&p);
        p
    }

    fn spawn_daemon() -> Option<Child> {
        Command::new(core_binary()).args(["daemon"]).spawn().ok()
    }

    /// 32×32 procedural icon: calm rounded square with a bridge bar.
    pub fn make_icon() -> Icon {
        let size = 32u32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        for y in 0..size {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                let inside = (4..size - 4).contains(&x) && (4..size - 4).contains(&y);
                let bar = (14..18).contains(&y) && (6..size - 6).contains(&x);
                if bar {
                    rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                } else if inside {
                    rgba[i..i + 4].copy_from_slice(&[36, 99, 235, 255]);
                }
            }
        }
        Icon::from_rgba(rgba, size, size).expect("icon")
    }

    pub fn autostart_key() -> &'static str {
        r"Software\Microsoft\Windows\CurrentVersion\Run"
    }

    /// Toggle the HKCU Run key (no admin required, spec §71). Returns new state.
    pub fn toggle_autostart() -> Option<bool> {
        use windows_sys::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
            HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ,
        };
        let wide = |s: &str| {
            s.encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let key_path = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
        let value_name = wide("DropBridge");
        unsafe {
            let mut hk: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                key_path.as_ptr(),
                0,
                KEY_READ | KEY_WRITE,
                &mut hk,
            ) != 0
            {
                return None;
            }
            let mut exists_len: u32 = 0;
            let exists = RegQueryValueExW(
                hk,
                value_name.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut exists_len,
            ) == 0;
            let new_state = !exists;
            if exists {
                RegDeleteValueW(hk, value_name.as_ptr());
            } else {
                let exe = std::env::current_exe().unwrap_or_default();
                let cmd = format!("\"{}\" --tray-hidden", exe.display());
                let data = wide(&cmd);
                RegSetValueExW(
                    hk,
                    value_name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr() as *const u8,
                    (data.len() * 2) as u32,
                );
            }
            RegCloseKey(hk);
            Some(new_state)
        }
    }

    pub fn run() -> anyhow::Result<()> {
        tracing_subscriber::fmt().init();
        let mut daemon = spawn_daemon();

        let menu = Menu::new();
        let send = MenuItem::with_id(ID_SEND, "Send files…", true, None);
        let open_recv = MenuItem::with_id(ID_OPEN_RECEIVED, "Open received files", true, None);
        let open_send = MenuItem::with_id(ID_OPEN_TOPHONE, "Open “To Phone” folder", true, None);
        let devices = MenuItem::with_id(ID_DEVICES, "Devices", true, None);
        let autostart = MenuItem::with_id(ID_AUTOSTART, "Start with Windows", true, None);
        let pause = MenuItem::with_id(ID_PAUSE, "Pause receiving", true, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);
        menu.append_items(&[
            &send,
            &PredefinedMenuItem::separator(),
            &open_recv,
            &open_send,
            &devices,
            &PredefinedMenuItem::separator(),
            &autostart,
            &pause,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let _tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("DropBridge — pair once, drop anywhere")
            .with_icon(make_icon())
            .build()?;

        let rx = MenuEvent::receiver();
        let mut paused = false;
        loop {
            let Ok(ev) = rx.recv() else { break };
            match ev.id().as_ref() {
                ID_SEND => {
                    let core = core_binary();
                    let exe = std::env::current_exe().unwrap_or_default();
                    let dir = exe.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                    let _ = Command::new(&core).current_dir(dir).spawn();
                }
                ID_OPEN_RECEIVED => {
                    let _ = open::that(user_dir("From Phone"));
                }
                ID_OPEN_TOPHONE => {
                    let _ = open::that(user_dir("To Phone"));
                }
                ID_DEVICES => {
                    let core = core_binary();
                    let _ = Command::new(core).args(["devices"]).spawn();
                }
                ID_AUTOSTART => {
                    if let Some(on) = toggle_autostart() {
                        tracing::info!(autostart = on, "autostart toggled");
                    }
                }
                ID_PAUSE => {
                    paused = !paused;
                    // MVP: pausing stops supervising receives by stopping the
                    // daemon; a control-socket API for fine-grained pause is
                    // the next iteration (docs/WINDOWS.md).
                    if paused {
                        if let Some(d) = daemon.as_mut() {
                            let _ = d.kill();
                        }
                    } else {
                        daemon = spawn_daemon();
                    }
                }
                ID_QUIT => {
                    if let Some(d) = daemon.as_mut() {
                        let _ = d.kill();
                    }
                    break;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::run() {
        eprintln!("dropbridge-tray error: {e}");
        std::process::exit(1);
    }
}
